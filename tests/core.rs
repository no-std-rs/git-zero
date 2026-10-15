use git_zero::{
    Error, File, Object, ObjectId, ObjectKind, Repository, Snapshot,
    cache::{self, Key, PackCache},
    client::{self, Protocol, Transport},
    http::{self, Request},
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};

#[test]
fn object_ids_match_git_known_vectors() {
    for (data, expected) in [
        (b"".as_slice(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
        (b"hello\n", "ce013625030ba8dba906f756967f9e9ca394464a"),
    ] {
        let object = Object {
            kind: ObjectKind::Blob,
            data,
        };
        assert_eq!(object.id().to_string(), expected);
    }
    for hex in [b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".as_slice(), b"a"] {
        assert_eq!(ObjectId::from_hex(hex), Err(Error::InvalidObjectId));
    }
}

#[test]
fn snapshot_deduplicates_blobs_and_has_no_parents() {
    let files = [
        File {
            name: "a",
            contents: b"same",
        },
        File {
            name: "b",
            contents: b"same",
        },
    ];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repo = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    assert_eq!(repo.object_count(), 3);
    assert!(
        !repo
            .object(0)
            .unwrap()
            .data
            .windows(7)
            .any(|part| part == b"parent ")
    );
    assert_eq!(repo.object(0).unwrap().id(), repo.head());
    let mut storage = [0; 2048];
    let mut output = SliceWriter::new(&mut storage);
    pack::write(&repo, &mut output).unwrap();
    pack::validate(output.as_bytes()).unwrap();
    let mut broken = output.as_bytes().to_vec();
    broken[12] ^= 1;
    assert_eq!(pack::validate(&broken), Err(Error::CorruptPack));
}

#[test]
fn snapshot_rejects_invalid_names_order_and_small_buffers() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    for name in ["", ".", "..", ".git", ".GIT", "a/b", "a\\b", "a\0b"] {
        let files = [File {
            name,
            contents: b"",
        }];
        assert!(matches!(
            Snapshot::new(&files, &mut tree, &mut commit),
            Err(Error::InvalidFileName)
        ));
    }
    let files = [
        File {
            name: "b",
            contents: b"",
        },
        File {
            name: "a",
            contents: b"",
        },
    ];
    assert!(matches!(
        Snapshot::new(&files, &mut tree, &mut commit),
        Err(Error::UnsortedFiles)
    ));
    let files = [File {
        name: "file",
        contents: b"",
    }];
    assert!(matches!(
        Snapshot::new(&files, &mut [], &mut commit),
        Err(Error::BufferTooSmall)
    ));
}

#[test]
fn packet_parser_rejects_truncation_and_v2_control_packets() {
    for bytes in [
        b"000".as_slice(),
        b"ffffx",
        b"0001",
        b"0002",
        b"0003",
        b"0008a",
        b"xxxx",
    ] {
        reject_packet(bytes);
    }
    let mut bytes = b"0000".as_slice();
    assert_eq!(pktline::read(&mut bytes), Ok(Some(Packet::Flush)));
    assert_eq!(pktline::read(&mut bytes), Ok(None));
}

#[test]
fn packet_prefixes_require_four_hex_digits() {
    for data in [b"+000".as_slice(), b"+004", b"+00a123456", b"-000"] {
        reject_packet(data);
    }
    for byte in (0..=255_u8).filter(|byte| !byte.is_ascii_hexdigit()) {
        for index in 0..4 {
            let mut data = *b"0004";
            data[index] = byte;
            reject_packet(&data);
        }
    }
    let mut remaining = b"000Aabcdef".as_slice();
    assert_eq!(
        pktline::read(&mut remaining),
        Ok(Some(Packet::Data(b"abcdef")))
    );
    assert!(remaining.is_empty());
}

#[test]
fn packet_length_boundary_is_enforced() {
    for length in [
        pktline::MAX_LENGTH - 1,
        pktline::MAX_LENGTH,
        pktline::MAX_LENGTH + 1,
    ] {
        let mut data = format!("{length:04x}").into_bytes();
        data.resize(length, 0xff);
        let mut remaining = data.as_slice();
        if length <= pktline::MAX_LENGTH {
            assert_eq!(
                pktline::read(&mut remaining),
                Ok(Some(Packet::Data(&data[4..])))
            );
            assert!(remaining.is_empty());
        } else {
            reject_packet(&data);
        }
    }
}

fn reject_packet(data: &[u8]) {
    let mut remaining = data;
    assert_eq!(
        pktline::read(&mut remaining),
        Err(Error::InvalidPacket),
        "{data:?}"
    );
    assert_eq!(remaining, data);
}

struct Loopback<'a, R> {
    repo: &'a R,
    dumb: bool,
}

#[test]
fn empty_smart_advertisement_has_no_refs_or_fetchable_head() {
    struct Advertisement(Vec<u8>);
    impl Transport for Advertisement {
        fn request(
            &mut self,
            request: Request<'_>,
            response: &mut [u8],
        ) -> git_zero::Result<(u16, usize)> {
            assert_eq!(request.path, "info/refs");
            response[..self.0.len()].copy_from_slice(&self.0);
            Ok((200, self.0.len()))
        }
    }
    for reference in [
        b"0000000000000000000000000000000000000000 capabilities^{}\0multi_ack symref=HEAD:refs/heads/main\n".as_slice(),
        b"0000000000000000000000000000000000000000 capabilities^{}\n",
    ] {
        let mut wire = b"001e# service=git-upload-pack\n0000".to_vec();
        wire.extend_from_slice(format!("{:04x}", reference.len() + 4).as_bytes());
        wire.extend_from_slice(reference);
        wire.extend_from_slice(b"0000");
        let mut transport = Advertisement(wire);
        let mut storage = [0; 512];
        let discovery = client::discover(&mut transport, &mut storage).unwrap();
        assert_eq!(discovery.protocol, Protocol::Smart);
        assert_eq!(discovery.references().count(), 0);
        assert_eq!(discovery.head(), Err(Error::ObjectNotFound));
        assert_eq!(
            client::read_head(&mut transport, &discovery, &mut []),
            Err(Error::ObjectNotFound)
        );
    }
    // The marker must not mask a truncated packet after it.
    let mut wire = b"001e# service=git-upload-pack\n0000".to_vec();
    let reference = b"0000000000000000000000000000000000000000 capabilities^{}\0agent=git\n";
    wire.extend_from_slice(format!("{:04x}", reference.len() + 4).as_bytes());
    wire.extend_from_slice(reference);
    wire.extend_from_slice(b"0008a");
    assert!(matches!(
        client::discover(&mut Advertisement(wire), &mut [0; 512]),
        Err(Error::InvalidPacket)
    ));
}
impl<R: Repository> Transport for Loopback<'_, R> {
    fn request(
        &mut self,
        mut request: Request<'_>,
        response: &mut [u8],
    ) -> git_zero::Result<(u16, usize)> {
        if self.dumb {
            request.query = None;
        }
        let mut output = SliceWriter::new(response);
        let response = http::serve(self.repo, request, &mut output)?;
        Ok((response.status, output.len()))
    }
}

#[test]
fn in_memory_client_server_pair_supports_both_transports() {
    let files = [File {
        name: "empty",
        contents: b"",
    }];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repo = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    for dumb in [false, true] {
        let mut transport = Loopback { repo: &repo, dumb };
        let mut discovery_storage = [0; 1024];
        let mut request = [0; 1024];
        let mut response = [0; 4096];
        let discovery = client::discover(&mut transport, &mut discovery_storage).unwrap();
        assert_eq!(
            discovery.protocol,
            if dumb {
                Protocol::Dumb
            } else {
                Protocol::Smart
            }
        );
        assert_eq!(
            client::read_head(&mut transport, &discovery, &mut response).unwrap(),
            repo.head()
        );
        if dumb {
            let bytes = client::fetch_loose(&mut transport, repo.head(), &mut response).unwrap();
            assert_eq!(&bytes[..2], &[0x78, 0x01]);
        } else {
            let bytes =
                client::fetch_pack(&mut transport, repo.head(), &mut request, &mut response)
                    .unwrap();
            pack::validate(bytes).unwrap();
            assert_eq!(&bytes[8..12], &3_u32.to_be_bytes());
        }
    }
}

#[test]
fn cache_hits_do_not_need_pack_scratch() {
    struct Cache {
        key: Option<Key>,
        bytes: Vec<u8>,
        writes: usize,
    }
    impl PackCache for Cache {
        fn get(&self, key: Key) -> git_zero::Result<Option<&[u8]>> {
            Ok((self.key == Some(key)).then_some(self.bytes.as_slice()))
        }
        fn put(&mut self, key: Key, bytes: &[u8]) -> git_zero::Result<()> {
            self.key = Some(key);
            self.bytes = bytes.to_vec();
            self.writes += 1;
            Ok(())
        }
    }
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repo = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut cache = Cache {
        key: None,
        bytes: Vec::new(),
        writes: 0,
    };
    let mut scratch = [0; 4096];
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    cache::write(&repo, &mut cache, &mut scratch, &mut output).unwrap();
    let original = output.as_bytes().to_vec();
    let mut output = SliceWriter::new(&mut storage);
    cache::write(&repo, &mut cache, &mut [], &mut output).unwrap();
    assert_eq!(output.as_bytes(), original);
    assert_eq!(cache.writes, 1);
}

#[test]
fn server_rejects_unadvertised_wants_and_push() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repo = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    let request = Request {
        content_type: None,
        git_protocol: None,
        method: "GET",
        path: "info/refs",
        query: Some("service=git-receive-pack"),
        body: &[],
    };
    assert_eq!(
        http::serve(&repo, request, &mut output).unwrap().status,
        403
    );
    let request = Request {
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: None,
        method: "POST",
        path: "git-upload-pack",
        query: None,
        body: b"0032want 0000000000000000000000000000000000000000\n00000009done\n",
    };
    assert_eq!(
        http::serve(&repo, request, &mut output).unwrap_err(),
        Error::InvalidRequest
    );
}
