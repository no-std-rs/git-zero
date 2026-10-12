//! Bounded targets shared by ordinary regression tests and libFuzzer.
use git_zero::{
    Error, Object, ObjectId, ObjectInfo, ObjectKind, Repository, Result, Snapshot,
    ValidatedRepository,
    client::{self, Transport},
    http::{self, Request},
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};

pub const MAX_INPUT: usize = 4096;

pub fn packets(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut remaining = data;
    loop {
        let before = remaining;
        match pktline::read(&mut remaining) {
            Ok(Some(packet)) => {
                assert!(before[..4].iter().all(u8::is_ascii_hexdigit));
                let consumed = before.len() - remaining.len();
                match packet {
                    Packet::Flush => {
                        assert_eq!(&before[..4], b"0000");
                        assert_eq!(consumed, 4);
                    }
                    Packet::Data(body) => {
                        assert_eq!(consumed, body.len() + 4);
                        assert_eq!(body, &before[4..consumed]);
                        assert!(consumed <= pktline::MAX_LENGTH);
                    }
                }
                assert_eq!(remaining, &before[consumed..]);
                assert!(remaining.len() < before.len(), "parser must make progress");
            }
            Ok(None) => {
                assert!(before.is_empty());
                break;
            }
            Err(_) => {
                assert_eq!(remaining, before, "failed parse must not consume input");
                break;
            }
        }
    }
}

struct Reply<'a> {
    data: &'a [u8],
    extra: usize,
}

impl Transport for Reply<'_> {
    fn request(&mut self, _: Request<'_>, storage: &mut [u8]) -> Result<(u16, usize)> {
        let destination = storage
            .get_mut(..self.data.len())
            .ok_or(Error::BufferTooSmall)?;
        destination.copy_from_slice(self.data);
        Ok((200, self.data.len() + self.extra))
    }
}

pub fn responses(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let mut storage = [0; MAX_INPUT];
    let mut response = [0; MAX_INPUT];
    let mut request = [0; 128];
    let mut transport = Reply { data, extra: 0 };
    if let Ok(discovery) = client::discover(&mut transport, &mut storage) {
        assert!(discovery.references().count() <= data.len());
        let _ = discovery.head();
        let _ = client::read_head(&mut transport, &discovery, &mut response);
    }
    let id = ObjectId([1; 20]);
    if let Ok(bytes) = client::fetch_pack(&mut transport, id, &mut request, &mut response) {
        assert!(bytes.len() <= data.len());
        pack::validate(bytes).unwrap();
    }
    let _ = pack::validate(data);
    // A transport may overreport its body size; every client entrypoint must
    // reject the size before indexing the caller's storage.
    transport.extra = MAX_INPUT + 1;
    assert!(matches!(
        client::discover(&mut transport, &mut storage),
        Err(Error::BufferTooSmall)
    ));
    assert_eq!(
        client::fetch_pack(&mut transport, id, &mut request, &mut response),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(
        client::fetch_loose(&mut transport, id, &mut response),
        Err(Error::BufferTooSmall)
    );
}

pub fn routing(data: &[u8]) {
    if data.len() < 2 || data.len() > MAX_INPUT {
        return;
    }
    let mut fields = data[2..].splitn(3, |&byte| byte == 0);
    let Ok(path) = core::str::from_utf8(fields.next().unwrap_or_default()) else {
        return;
    };
    let Ok(query) = core::str::from_utf8(fields.next().unwrap_or_default()) else {
        return;
    };
    let body = fields.next().unwrap_or_default();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let request = Request {
        method: ["GET", "POST", "PUT", "DELETE", "HEAD"][usize::from(data[0]) % 5],
        path,
        query: (!query.is_empty()).then_some(query),
        body,
    };
    let capacity = [0, 1, 16, 64, MAX_INPUT][usize::from(data[1]) % 5];
    let mut storage = [0; MAX_INPUT];
    let mut output = SliceWriter::new(&mut storage[..capacity]);
    if let Ok(response) = http::serve_with_scratch(&repository, request, &mut [0; 1], &mut output) {
        assert!(matches!(response.status, 200 | 403 | 404 | 405));
        assert!(output.len() <= capacity);
        if response.content_type == "application/x-git-upload-pack-result" {
            assert!(output.as_bytes().starts_with(b"0008NAK\n"));
            if output.len() > 8 {
                pack::validate(&output.as_bytes()[8..]).unwrap();
            }
        }
    }
}

struct Graph<'a> {
    objects: [Object<'a>; 3],
    borrowed: bool,
    read_limit: usize,
}

impl Repository for Graph<'_> {
    fn head(&self) -> ObjectId {
        self.objects[0].id()
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.borrowed
            .then(|| self.objects.get(index).copied())
            .flatten()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.objects
            .get(index)
            .map(Object::info)
            .ok_or(Error::ObjectNotFound)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects.get(index).map(Object::id)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        let object = self.objects.get(index).ok_or(Error::ObjectNotFound)?;
        let bytes = object.data.get(offset as usize..).unwrap_or_default();
        let count = bytes.len().min(storage.len()).min(self.read_limit);
        storage[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }
}

pub fn graph(data: &[u8]) {
    if data.len() < 2 || data.len() > MAX_INPUT {
        return;
    }
    let blob = Object {
        kind: ObjectKind::Blob,
        data: b"seed\n",
    };
    let mutate_tree = data[0] & 1 == 0;
    let tree_bytes = if mutate_tree {
        data[2..].to_vec()
    } else {
        let mut bytes = b"100644 file\0".to_vec();
        bytes.extend_from_slice(&blob.id().0);
        bytes
    };
    let tree = Object {
        kind: ObjectKind::Tree,
        data: &tree_bytes,
    };
    // Recompute IDs after mutations so malformed records reach the graph parser
    // rather than failing only at the initial object checksum check.
    let commit_bytes = if mutate_tree {
        format!(
            "tree {}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nfuzz\n",
            tree.id()
        )
        .into_bytes()
    } else {
        data[2..].to_vec()
    };
    let objects = [
        Object {
            kind: ObjectKind::Commit,
            data: &commit_bytes,
        },
        tree,
        blob,
    ];
    let mut repository = Graph {
        objects,
        borrowed: true,
        read_limit: usize::from(data[1] % 64) + 1,
    };
    let mut expected = None;
    for borrowed in [true, false] {
        repository.borrowed = borrowed;
        let result = ValidatedRepository::new(
            &repository,
            &mut [None; 3],
            &mut [0; 3],
            &mut [0; 64],
            &mut [0; 128],
        )
        .map(|validated| {
            assert_eq!(validated.head(), repository.head());
            assert_eq!(validated.object_count(), 3);
        });
        if let Some(expected) = expected {
            assert_eq!(result, expected);
        }
        expected = Some(result);
    }
}
