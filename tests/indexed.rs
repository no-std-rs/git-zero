mod common;

use git_zero::{
    Error, IndexedRepository, Object, ObjectId, ObjectKind, Repository, Snapshot,
    http::{self, Request},
    io::SliceWriter,
    pack,
};
use std::cell::Cell;

struct Counted<'a> {
    source: &'a dyn Repository,
    counts: Cell<usize>,
    reads: Cell<usize>,
}

impl Repository for Counted<'_> {
    fn head(&self) -> ObjectId {
        self.source.head()
    }

    fn object_count(&self) -> usize {
        self.counts.set(self.counts.get() + 1);
        self.source.object_count()
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.reads.set(self.reads.get() + 1);
        self.source.object(index)
    }

    fn head_ref(&self) -> &str {
        self.source.head_ref()
    }

    fn reference_count(&self) -> usize {
        self.source.reference_count()
    }

    fn reference_name(&self, index: usize) -> Option<&str> {
        self.source.reference_name(index)
    }
}

#[test]
fn indexed_reads_use_metadata_without_reentering_the_source() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let snapshot = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit)
        .unwrap()
        .with_refs(common::HEAD_REF, common::REFS)
        .unwrap();
    let source = Counted {
        source: &snapshot,
        counts: Cell::new(0),
        reads: Cell::new(0),
    };
    let mut objects = [None; 6];
    let mut by_id = [0; 6];
    let indexed = IndexedRepository::new(&source, &mut objects, &mut by_id).unwrap();
    assert_eq!(source.counts.get(), 1);
    assert_eq!(source.reads.get(), 6);
    for _ in 0..3 {
        assert_eq!(indexed.head(), snapshot.head());
        assert_eq!(indexed.head_ref(), common::HEAD_REF);
        assert_eq!(indexed.reference_count(), common::REFS.len());
        for (index, name) in common::REFS.iter().enumerate() {
            assert_eq!(indexed.reference_name(index), Some(*name));
        }
        assert_eq!(indexed.reference_name(common::REFS.len()), None);
        for index in 0..indexed.object_count() {
            let expected = snapshot.object(index).unwrap();
            let id = expected.id();
            assert_eq!(snapshot.object_id(index), Some(id));
            assert_eq!(indexed.object_id(index), Some(id));
            let metadata = indexed.metadata(index).unwrap();
            assert_eq!(metadata.id, id);
            for object in [
                metadata.object,
                indexed.object(index).unwrap(),
                indexed.find(id).unwrap(),
            ] {
                assert_eq!(object.kind, expected.kind);
                assert_eq!(object.data, expected.data);
                assert_eq!(object.data.as_ptr(), expected.data.as_ptr());
            }
        }
        assert!(indexed.find(ObjectId([0; 20])).is_none());
        assert!(indexed.find(ObjectId([255; 20])).is_none());
        for index in [indexed.object_count(), usize::MAX] {
            assert!(indexed.metadata(index).is_none());
            assert!(indexed.object(index).is_none());
            assert!(indexed.object_id(index).is_none());
        }
        let mut bytes = [0; 4096];
        let mut output = SliceWriter::new(&mut bytes);
        pack::write(&indexed, &mut output).unwrap();
        pack::validate(output.as_bytes()).unwrap();
    }
    assert_eq!(source.counts.get(), 1);
    assert_eq!(source.reads.get(), 6);
}

#[test]
fn indexed_snapshots_preserve_pack_and_http_bytes() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let snapshot = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit)
        .unwrap()
        .with_refs(common::HEAD_REF, common::REFS)
        .unwrap();
    let mut objects = [None; 6];
    let mut by_id = [0; 6];
    let indexed = IndexedRepository::new(&snapshot, &mut objects, &mut by_id).unwrap();
    let mut plain_bytes = [0; 4096];
    let mut indexed_bytes = [0; 4096];
    let mut plain_output = SliceWriter::new(&mut plain_bytes);
    let mut indexed_output = SliceWriter::new(&mut indexed_bytes);
    assert_eq!(
        pack::write(&snapshot, &mut plain_output).unwrap(),
        pack::write(&indexed, &mut indexed_output).unwrap()
    );
    assert_eq!(plain_output.as_bytes(), indexed_output.as_bytes());

    let upload = format!("0032want {}\n00000009done\n", snapshot.head());
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
            "refs/heads/releases/current%23%25%c3%a9+".to_string(),
            None,
            &[][..],
        ),
        (
            "GET",
            "objects/00/00000000000000000000000000000000000000".to_string(),
            None,
            &[][..],
        ),
    ];
    for index in 0..snapshot.object_count() {
        let id = snapshot.object_id(index).unwrap().to_string();
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
        let mut plain_output = SliceWriter::new(&mut plain_bytes);
        let mut indexed_output = SliceWriter::new(&mut indexed_bytes);
        let plain_response = http::serve(&snapshot, request, &mut plain_output).unwrap();
        let indexed_response = http::serve(&indexed, request, &mut indexed_output).unwrap();
        assert_eq!(plain_response.status, indexed_response.status, "{path}");
        assert_eq!(
            plain_response.content_type, indexed_response.content_type,
            "{path}"
        );
        assert_eq!(plain_output.as_bytes(), indexed_output.as_bytes(), "{path}");
    }
}

struct Objects<'a>(&'a [Option<Object<'a>>]);

impl Repository for Objects<'_> {
    fn head(&self) -> ObjectId {
        self.object_id(0).unwrap_or(ObjectId([0; 20]))
    }

    fn object_count(&self) -> usize {
        self.0.len()
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.0.get(index).copied().flatten()
    }
}

#[test]
fn indexing_checks_both_capacities_before_reading_objects() {
    let source = Objects(&[
        Some(Object {
            kind: ObjectKind::Blob,
            data: b"first",
        }),
        Some(Object {
            kind: ObjectKind::Blob,
            data: b"second",
        }),
    ]);
    let source = Counted {
        source: &source,
        counts: Cell::new(0),
        reads: Cell::new(0),
    };
    for (object_capacity, index_capacity) in [(0, 0), (1, 2), (2, 1)] {
        let mut objects = [None; 2];
        let mut by_id = [usize::MAX; 2];
        assert!(matches!(
            IndexedRepository::new(
                &source,
                &mut objects[..object_capacity],
                &mut by_id[..index_capacity]
            ),
            Err(Error::BufferTooSmall)
        ));
        assert!(objects.iter().all(Option::is_none));
        assert_eq!(by_id, [usize::MAX; 2]);
    }
    assert_eq!(source.counts.get(), 3);
    assert_eq!(source.reads.get(), 0);

    let mut objects = [None; 3];
    let mut by_id = [usize::MAX; 3];
    {
        let indexed = IndexedRepository::new(&source, &mut objects, &mut by_id).unwrap();
        assert_eq!(indexed.object_count(), 2);
        assert!(indexed.object(2).is_none());
    }
    assert!(objects[2].is_none());
    assert_eq!(by_id[2], usize::MAX);
    assert_eq!(source.reads.get(), 2);
}

#[test]
fn indexing_handles_empty_sources_and_reuses_storage_after_missing_objects() {
    let empty = Objects(&[]);
    let indexed = IndexedRepository::new(&empty, &mut [], &mut []).unwrap();
    assert_eq!(indexed.object_count(), 0);
    assert!(indexed.object(0).is_none());
    assert!(indexed.object_id(0).is_none());
    assert!(indexed.find(ObjectId([0; 20])).is_none());

    let object = Some(Object {
        kind: ObjectKind::Blob,
        data: b"present",
    });
    let mut inputs = [[object; 3]; 3];
    for (missing, entries) in inputs.iter_mut().enumerate() {
        entries[missing] = None;
    }
    let sources = inputs.each_ref().map(|entries| Objects(entries));
    let source = Objects(&[object; 3]);
    let mut objects = [None; 3];
    let mut by_id = [0; 3];
    for source in &sources {
        assert!(matches!(
            IndexedRepository::new(source, &mut objects, &mut by_id),
            Err(Error::ObjectNotFound)
        ));
    }
    let indexed = IndexedRepository::new(&source, &mut objects, &mut by_id).unwrap();
    // A wrapper preserves enumeration, even for duplicate source objects.
    assert_eq!(indexed.object_count(), 3);
    assert_eq!(indexed.find(source.head()).unwrap().data, b"present");
}

#[test]
fn shuffled_ids_lookup_every_kind_without_reordering_objects() {
    let data: Vec<_> = (0..257_u32).map(|value| value.to_be_bytes()).collect();
    let entries: Vec<_> = (0..257)
        .map(|index| {
            Some(Object {
                kind: match index % 4 {
                    0 => ObjectKind::Commit,
                    1 => ObjectKind::Tree,
                    2 => ObjectKind::Blob,
                    _ => ObjectKind::Tag,
                },
                data: &data[index],
            })
        })
        .collect();
    let source = Objects(&entries);
    let mut objects = vec![None; entries.len()];
    let mut by_id = vec![0; entries.len()];
    // Exercise the unsized source supported by the wrapper.
    let source: &dyn Repository = &source;
    let indexed = IndexedRepository::new(source, &mut objects, &mut by_id).unwrap();
    for (index, expected) in entries.iter().enumerate() {
        let expected = expected.unwrap();
        assert_eq!(indexed.object(index).unwrap().data, expected.data);
        let found = indexed.find(expected.id()).unwrap();
        assert_eq!(found.kind, expected.kind);
        assert_eq!(found.data, expected.data);
    }
    assert!(indexed.find(ObjectId([0; 20])).is_none());
}
