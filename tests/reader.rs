#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    Error, File, Object, ObjectHasher, ObjectId, ObjectInfo, ObjectKind, ObjectReader, Repository,
    Result, Snapshot,
    http::{self, Request},
    io::SliceWriter,
    pack,
};
use std::cell::Cell;
use streamed::{GeneratedRepository, byte_at};

struct Borrowed<'a>(Object<'a>);

impl Repository for Borrowed<'_> {
    fn head(&self) -> ObjectId {
        self.0.id()
    }

    fn object_count(&self) -> usize {
        1
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        (index == 0).then_some(self.0)
    }
}

#[test]
fn borrowed_readers_seek_interleave_and_hash_every_kind() {
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        let repository = Borrowed(Object {
            kind,
            data: b"hello\n",
        });
        // Exercise trait-object repositories and the default borrowed adapter.
        let source: &dyn Repository = &repository;
        let mut reader = ObjectReader::new(source, 0).unwrap();
        let mut other = ObjectReader::new(source, 0).unwrap();
        let mut scratch = [0; 4];
        assert_eq!(reader.info(), repository.0.info());
        assert_eq!(reader.read(&mut []), Err(Error::BufferTooSmall));
        assert_eq!(reader.position(), 0);
        assert_eq!(reader.read(&mut scratch).unwrap(), 4);
        assert_eq!(&scratch, b"hell");
        other.seek(3).unwrap();
        assert_eq!(other.read(&mut scratch).unwrap(), 3);
        assert_eq!(&scratch[..3], b"lo\n");
        assert_eq!(reader.position(), 4);
        assert_eq!(reader.remaining(), 2);
        assert_eq!(reader.read(&mut scratch).unwrap(), 2);
        assert_eq!(&scratch[..2], b"o\n");
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert_eq!(reader.seek(7), Err(Error::InvalidObject));
        assert_eq!(reader.position(), 6);
        reader.seek(0).unwrap();
        assert_eq!(reader.remaining(), 6);
        assert_eq!(source.read_object(0, u64::MAX, &mut scratch), Ok(0));
        assert_eq!(
            source.read_object(1, 0, &mut scratch),
            Err(Error::ObjectNotFound)
        );
        assert!(matches!(
            ObjectReader::new(source, 1),
            Err(Error::ObjectNotFound)
        ));
        assert_eq!(
            ObjectHasher::new(source, 0)
                .unwrap()
                .finish(&mut [0; 1])
                .unwrap(),
            repository.0.id()
        );
    }
}

#[test]
fn generated_hashes_resume_across_changing_buffers_and_short_reads() {
    let repository = GeneratedRepository::new(131071, 29);
    let mut hasher = ObjectHasher::new(&repository, 2).unwrap();
    assert_eq!(hasher.step(&mut []), Err(Error::BufferTooSmall));
    assert_eq!(hasher.position(), 0);
    let mut scratch = [0; 128];
    assert_eq!(hasher.step(&mut scratch).unwrap(), None);
    assert_eq!(hasher.position(), 29);
    assert_eq!(hasher.step(&mut scratch[..3]).unwrap(), None);
    assert_eq!(hasher.position(), 32);
    let expected: Vec<_> = (0..131071).map(byte_at).collect();
    let expected = Object {
        kind: ObjectKind::Blob,
        data: &expected,
    }
    .id();
    loop {
        if let Some(id) = hasher.step(&mut scratch).unwrap() {
            assert_eq!(id, expected);
            break;
        }
    }
    assert_eq!(hasher.position(), 131071);
    assert_eq!(hasher.step(&mut []).unwrap(), Some(expected));
    assert_eq!(hasher.finish(&mut []).unwrap(), expected);
}

struct Faulty {
    mode: Cell<u8>,
    calls: Cell<usize>,
}

impl Repository for Faulty {
    fn head(&self) -> ObjectId {
        ObjectId::Sha1([0; 20])
    }

    fn object_count(&self) -> usize {
        1
    }

    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        if index != 0 {
            return Err(Error::ObjectNotFound);
        }
        Ok(ObjectInfo {
            kind: ObjectKind::Blob,
            size: 5,
        })
    }

    fn read_object(&self, _: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        self.calls.set(self.calls.get() + 1);
        match self.mode.get() {
            0 => Err(Error::Backend),
            1 => Ok(0),
            2 => Ok(storage.len() + 1),
            _ => {
                storage.fill(b'x');
                Ok((5 - offset as usize).min(storage.len()))
            }
        }
    }
}

#[test]
fn invalid_reads_and_backend_errors_preserve_cursor_and_hash_for_retry() {
    let repository = Faulty {
        mode: Cell::new(3),
        calls: Cell::new(0),
    };
    let mut hasher = ObjectHasher::new(&repository, 0).unwrap();
    assert_eq!(hasher.step(&mut [0; 2]).unwrap(), None);
    for (mode, error) in [
        (0, Error::Backend),
        (1, Error::InvalidObject),
        (2, Error::InvalidObject),
    ] {
        repository.mode.set(mode);
        assert_eq!(hasher.step(&mut [0; 8]), Err(error));
        assert_eq!(hasher.position(), 2);
        let mut storage = [0; 256];
        let mut output = SliceWriter::new(&mut storage);
        assert_eq!(
            pack::write_with_scratch(&repository, &mut [0; 8], &mut output),
            Err(error)
        );
        let mut output = SliceWriter::new(&mut storage);
        assert_eq!(
            pack::write_loose_with_scratch(&repository, 0, &mut [0; 8], &mut output),
            Err(error)
        );
    }
    repository.mode.set(3);
    let expected = Object {
        kind: ObjectKind::Blob,
        data: b"xxxxx",
    }
    .id();
    assert_eq!(hasher.step(&mut [0; 8]).unwrap(), Some(expected));
    let calls = repository.calls.get();
    assert_eq!(hasher.step(&mut []).unwrap(), Some(expected));
    assert_eq!(repository.calls.get(), calls);
}

struct Huge;

impl Repository for Huge {
    fn head(&self) -> ObjectId {
        ObjectId::Sha1([0; 20])
    }

    fn object_count(&self) -> usize {
        1
    }

    fn object_info(&self, _: usize) -> Result<ObjectInfo> {
        Ok(ObjectInfo {
            kind: ObjectKind::Blob,
            size: u64::MAX,
        })
    }

    fn read_object(&self, _: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        let length = (u64::MAX - offset).min(storage.len() as u64) as usize;
        for (index, byte) in storage[..length].iter_mut().enumerate() {
            *byte = byte_at(offset + index as u64);
        }
        Ok(length)
    }
}

#[test]
fn reader_uses_64_bit_sizes_and_offsets_without_object_sized_storage() {
    let mut reader = ObjectReader::new(&Huge, 0).unwrap();
    reader.seek(u64::MAX - 3).unwrap();
    let mut storage = [0; 16];
    assert_eq!(reader.read(&mut storage).unwrap(), 3);
    assert_eq!(reader.position(), u64::MAX);
    assert_eq!(reader.remaining(), 0);
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert_eq!(
        &storage[..3],
        &[
            byte_at(u64::MAX - 3),
            byte_at(u64::MAX - 2),
            byte_at(u64::MAX - 1)
        ]
    );
    for kind in [
        ObjectKind::Commit,
        ObjectKind::Tree,
        ObjectKind::Blob,
        ObjectKind::Tag,
    ] {
        let info = ObjectInfo {
            kind,
            size: u64::MAX,
        };
        assert_eq!(
            info.header(&mut [0; 32]).unwrap(),
            format!("{} {}\0", kind.name(), u64::MAX).as_bytes()
        );
        assert_eq!(info.header(&mut [0; 8]), Err(Error::BufferTooSmall));
    }
}

struct Bounded<'a> {
    repository: &'a GeneratedRepository,
    limit: usize,
}

impl Repository for Bounded<'_> {
    fn head(&self) -> ObjectId {
        self.repository.head()
    }

    fn object_count(&self) -> usize {
        self.repository.object_count()
    }

    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.repository.object_id(index)
    }

    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.repository.object_info(index)
    }

    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        assert!(storage.len() <= self.limit);
        self.repository.read_object(index, offset, storage)
    }
}

#[test]
fn generated_pack_and_http_bytes_match_borrowed_snapshots_for_every_budget() {
    // Test either side of a DEFLATE block boundary and multiple full blocks.
    for size in [0, 1, 65534, 65535, 65536, 131071] {
        let contents: Vec<_> = (0..size).map(byte_at).collect();
        let files = [File {
            name: "generated.bin",
            contents: &contents,
        }];
        let mut tree = [0; 128];
        let mut commit = [0; 256];
        let borrowed = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
        let generated = GeneratedRepository::new(size, 137);
        assert_eq!(generated.head(), borrowed.head());
        let mut expected = vec![0; size as usize + 2048];
        let mut actual = expected.clone();
        let mut expected_pack = SliceWriter::new(&mut expected);
        let checksum = pack::write(&borrowed, &mut expected_pack).unwrap();
        let mut default_bytes = vec![0; size as usize + 2048];
        let mut default_output = SliceWriter::new(&mut default_bytes);
        assert_eq!(
            pack::write(&generated, &mut default_output).unwrap(),
            checksum
        );
        assert_eq!(default_output.as_bytes(), expected_pack.as_bytes());
        for capacity in [1, 7, 1024, 65536] {
            let bounded = Bounded {
                repository: &generated,
                limit: capacity,
            };
            let mut scratch = vec![0; capacity];
            let mut output = SliceWriter::new(&mut actual);
            assert_eq!(
                pack::write_with_scratch(&bounded, &mut scratch[..capacity], &mut output).unwrap(),
                checksum
            );
            assert_eq!(output.as_bytes(), expected_pack.as_bytes());
            pack::validate(output.as_bytes()).unwrap();
        }

        let upload = format!("0032want {}\n00000009done\n", borrowed.head());
        let mut requests = vec![
            ("GET", "HEAD".to_string(), None, &[][..]),
            ("GET", "info/refs".to_string(), None, &[][..]),
            (
                "GET",
                "info/refs".to_string(),
                Some("service=git-upload-pack"),
                &[][..],
            ),
            (
                "POST",
                "git-upload-pack".to_string(),
                None,
                upload.as_bytes(),
            ),
            (
                "GET",
                "objects/00/00000000000000000000000000000000000000".to_string(),
                None,
                &[][..],
            ),
        ];
        for index in 0..borrowed.object_count() {
            let id = borrowed.object_id(index).unwrap();
            assert_eq!(generated.object_id(index), Some(id));
            assert_eq!(generated.find_index(id), Some(index));
            assert!(generated.find(id).is_none());
            let id = id.to_string();
            requests.push((
                "GET",
                format!("objects/{}/{}", &id[..2], &id[2..]),
                None,
                &[][..],
            ));
        }
        for (method, path, query, body) in requests {
            let request = Request {
                method,
                path: &path,
                query,
                body,
            };
            let mut expected_bytes = vec![0; size as usize + 2048];
            let mut expected_output = SliceWriter::new(&mut expected_bytes);
            let expected_response = http::serve(&borrowed, request, &mut expected_output).unwrap();
            let mut default_output = SliceWriter::new(&mut default_bytes);
            http::serve(&generated, request, &mut default_output).unwrap();
            assert_eq!(
                default_output.as_bytes(),
                expected_output.as_bytes(),
                "{path}"
            );
            for capacity in [1, 7, 1024, 65536] {
                let bounded = Bounded {
                    repository: &generated,
                    limit: capacity,
                };
                let mut scratch = vec![0; capacity];
                let mut output = SliceWriter::new(&mut actual);
                let response = http::serve_with_scratch(
                    &bounded,
                    request,
                    &mut scratch[..capacity],
                    &mut output,
                )
                .unwrap();
                assert_eq!(response.status, expected_response.status, "{path}");
                assert_eq!(
                    response.content_type, expected_response.content_type,
                    "{path}"
                );
                assert_eq!(output.as_bytes(), expected_output.as_bytes(), "{path}");
            }
        }
    }
}

#[test]
fn empty_scratch_supports_empty_streams_and_borrowed_bodies() {
    let generated = GeneratedRepository::new(0, 1);
    let empty = Object {
        kind: ObjectKind::Blob,
        data: &[],
    };
    assert_eq!(
        ObjectHasher::new(&generated, 2)
            .unwrap()
            .finish(&mut [])
            .unwrap(),
        empty.id()
    );
    let mut expected_bytes = [0; 64];
    let mut actual_bytes = [0; 64];
    let mut expected = SliceWriter::new(&mut expected_bytes);
    let mut actual = SliceWriter::new(&mut actual_bytes);
    pack::write_loose(empty, &mut expected).unwrap();
    pack::write_loose_with_scratch(&generated, 2, &mut [], &mut actual).unwrap();
    assert_eq!(expected.as_bytes(), actual.as_bytes());

    let generated = GeneratedRepository::new(1, 1);
    let mut actual = SliceWriter::new(&mut actual_bytes);
    assert_eq!(
        pack::write_loose_with_scratch(&generated, 2, &mut [], &mut actual),
        Err(Error::BufferTooSmall)
    );
    assert!(actual.is_empty());
    assert_eq!(
        pack::write_loose_with_scratch(&generated, 3, &mut [0; 1], &mut actual),
        Err(Error::ObjectNotFound)
    );
    let borrowed = Borrowed(Object {
        kind: ObjectKind::Blob,
        data: b"borrowed",
    });
    pack::write_with_scratch(&borrowed, &mut [], &mut actual).unwrap();
}
