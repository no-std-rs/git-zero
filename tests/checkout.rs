#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, Object, ObjectFormat, ObjectId, ObjectKind,
    checkout::{self, Mode, ObjectSource},
};

// Every read overwrites the same buffer, like an aggressively evicting cache.
struct Store {
    entries: Vec<(ObjectId, ObjectKind, Vec<u8>)>,
    scratch: [u8; 1024],
}
impl Store {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            scratch: [0; 1024],
        }
    }
    fn insert(&mut self, format: ObjectFormat, kind: ObjectKind, data: &[u8]) -> ObjectId {
        let id = Object { kind, data }.id_with_format(format);
        self.entries.push((id, kind, data.to_vec()));
        id
    }
}
impl ObjectSource for Store {
    fn object(&mut self, id: ObjectId) -> git_zero::Result<Object<'_>> {
        let (_, kind, data) = self
            .entries
            .iter()
            .find(|(key, _, _)| *key == id)
            .ok_or(Error::ObjectNotFound)?;
        self.scratch.fill(0xaa);
        self.scratch[..data.len()].copy_from_slice(data);
        Ok(Object {
            kind: *kind,
            data: &self.scratch[..data.len()],
        })
    }
}

#[test]
fn roots_and_messages_survive_eviction_in_both_formats_without_allocations() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut store = Store::new();
        let tree = store.insert(format, ObjectKind::Tree, b"");
        let body = format!("tree {tree}\nauthor test\n\nfirst\n\nsecond\n");
        let commit = store.insert(format, ObjectKind::Commit, body.as_bytes());
        let tag_body = format!("object {commit}\ntype commit\ntag v1\n\ntag message\n");
        let tag = store.insert(format, ObjectKind::Tag, tag_body.as_bytes());
        let outer_body = format!("object {tag}\ntype tag\ntag outer\n\n");
        let outer = store.insert(format, ObjectKind::Tag, outer_body.as_bytes());
        allocations::without_allocations(|| {
            assert_eq!(
                checkout::resolve_root(&mut store, outer, 1),
                Err(Error::LimitExceeded)
            );
            let root = checkout::resolve_root(&mut store, outer, 2).unwrap();
            assert_eq!(root.entry.id, tree);
            assert_eq!(root.entry.mode, Mode::Directory);
            let message = root.message.unwrap();
            assert_eq!(message.id, commit);
            let object = store.object(message.id).unwrap();
            assert_eq!(
                &object.data[message.offset..][..message.length],
                b"first\n\nsecond\n"
            );
            let root = checkout::resolve_root(&mut store, tree, 0).unwrap();
            assert_eq!(root.message, None);
        });
    }
}

#[test]
fn declared_types_formats_and_missing_targets_are_checked() {
    let format = ObjectFormat::Sha1;
    let mut store = Store::new();
    let blob = store.insert(format, ObjectKind::Blob, b"binary\0\xff");
    let bad_commit = store.insert(
        format,
        ObjectKind::Commit,
        format!("tree {blob}\n\nmsg").as_bytes(),
    );
    let bad_tag = store.insert(
        format,
        ObjectKind::Tag,
        format!("object {blob}\ntype tree\ntag bad\n\n").as_bytes(),
    );
    let wide_tag = store.insert(
        format,
        ObjectKind::Tag,
        format!(
            "object {}\ntype blob\ntag bad\n\n",
            ObjectId::Sha256([1; 32])
        )
        .as_bytes(),
    );
    allocations::without_allocations(|| {
        let root = checkout::resolve_root(&mut store, blob, 0).unwrap();
        assert_eq!(root.entry.mode, Mode::File);
        assert_eq!(root.message, None);
        for id in [bad_commit, bad_tag] {
            assert_eq!(
                checkout::resolve_root(&mut store, id, 8),
                Err(Error::InvalidObject)
            );
        }
        assert_eq!(
            checkout::resolve_root(&mut store, wide_tag, 8),
            Err(Error::InvalidObjectId)
        );
        assert_eq!(
            checkout::resolve_root(&mut store, ObjectId::Sha1([1; 20]), 8),
            Err(Error::ObjectNotFound)
        );
    });
}

#[test]
fn tree_lookup_preserves_modes_ids_and_byte_names() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let id = Object {
            kind: ObjectKind::Blob,
            data: b"contents",
        }
        .id_with_format(format);
        let mut tree = b"120000 \xffname\0".to_vec();
        tree.extend_from_slice(id.as_bytes());
        allocations::without_allocations(|| {
            let entry = checkout::lookup(&tree, format, b"\xffname")
                .unwrap()
                .unwrap();
            assert_eq!(entry.id, id);
            assert_eq!(entry.mode, Mode::Symlink);
            assert_eq!(checkout::lookup(&tree, format, b"missing"), Ok(None));
            for name in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
                assert_eq!(
                    checkout::lookup(&tree, format, name),
                    Err(Error::InvalidFileName)
                );
            }
            assert!(checkout::lookup(&tree[..tree.len() - 1], format, b"missing").is_err());
        });
        let duplicate = [tree.as_slice(), tree.as_slice()].concat();
        assert_eq!(
            checkout::lookup(&duplicate, format, b"\xffname"),
            Err(Error::InvalidObject)
        );
    }
}

#[test]
fn file_and_symlink_contents_are_exact_borrowed_blob_bytes() {
    allocations::without_allocations(|| {
        let bytes = b"\0binary\xff\r\n";
        let blob = Object {
            kind: ObjectKind::Blob,
            data: bytes,
        };
        for mode in [Mode::File, Mode::Executable] {
            assert_eq!(mode.contents(blob), Ok(bytes.as_slice()));
            assert_eq!(Mode::from_raw(mode.raw()), Ok(mode));
        }
        let target = b"../dir/\xfffile";
        let object = Object {
            kind: ObjectKind::Blob,
            data: target,
        };
        assert_eq!(Mode::Symlink.contents(object), Ok(target.as_slice()));
        assert_eq!(Mode::Symlink.contents(blob), Err(Error::InvalidObject));
        assert_eq!(
            Mode::File.contents(Object {
                kind: ObjectKind::Tree,
                data: b""
            }),
            Err(Error::InvalidObject)
        );
        assert_eq!(Mode::Directory.contents(blob), Err(Error::Unsupported));
        assert_eq!(Mode::Gitlink.contents(blob), Err(Error::Unsupported));
        assert_eq!(Mode::from_raw(0o100600), Err(Error::InvalidObject));
    });
}
