use git_zero::{Error, Object, ObjectFormat, ObjectId, ObjectInfo, ObjectKind, Repository, Result};
use std::cell::Cell;

pub const N: usize = 24;
pub const BLOB: usize = 0;
pub const TREE: usize = 1;
pub const A: usize = 2;
pub const B: usize = 3;
pub const C: usize = 4;
pub const D: usize = 5;
pub const E: usize = 6;
pub const M: usize = 7;
pub const TAG: usize = 8;
pub const BLOB_TAG: usize = 9;

pub struct Fixture {
    pub format: ObjectFormat,
    pub objects: Vec<(ObjectId, ObjectKind, Vec<u8>)>,
    pub ids: Vec<ObjectId>,
    pub bytes: Cell<u64>,
    pub failed: Cell<bool>,
}

impl Fixture {
    pub fn new(format: ObjectFormat) -> Self {
        let mut fixture = Self {
            format,
            objects: Vec::new(),
            ids: Vec::new(),
            bytes: Cell::new(0),
            failed: Cell::new(false),
        };
        let blob = fixture.add(
            ObjectKind::Blob,
            b"shared across excluded and selected commits\n".to_vec(),
        );
        let mut tree = b"100644 shared\0".to_vec();
        tree.extend_from_slice(blob.as_bytes());
        let tree = fixture.add(ObjectKind::Tree, tree);
        for (name, date, parents) in [
            ("A", 100, vec![]),
            ("B", 300, vec![A]),
            ("C", 200, vec![B]),
            ("D", 400, vec![C]),
            ("E", 350, vec![B]),
            ("M", 500, vec![D, E]),
        ] {
            let mut body = format!("tree {tree}\n");
            for parent in parents {
                body.push_str(&format!("parent {}\n", fixture.ids[parent]));
            }
            body.push_str(&format!("author Oracle <oracle@example.test> 1700000000 +0000\ncommitter Oracle <oracle@example.test> {} +0000\n\n{name}\n", 1700000000 + date));
            fixture.add(ObjectKind::Commit, body.into_bytes());
        }
        for (target, kind, name) in [(C, "commit", "tagC"), (BLOB, "blob", "tagBlob")] {
            fixture.add(ObjectKind::Tag, format!("object {}\ntype {kind}\ntag {name}\ntagger Oracle <oracle@example.test> 1700000000 +0000\n\n{name}\n", fixture.ids[target]).into_bytes());
        }
        fixture
    }
    fn add(&mut self, kind: ObjectKind, bytes: Vec<u8>) -> ObjectId {
        let id = Object { kind, data: &bytes }.id_with_format(self.format);
        self.objects.push((id, kind, bytes));
        self.ids.push(id);
        id
    }
}

const REFS: &[(&str, usize, Option<&str>, Option<usize>)] = &[
    ("refs/heads/ambiguous", B, None, None),
    ("refs/heads/cutoff", B, None, None),
    ("refs/heads/main", D, None, None),
    ("refs/heads/merge", M, None, None),
    ("refs/heads/symcut", B, Some("refs/heads/cutoff"), None),
    ("refs/tags/ambiguous", C, None, None),
    ("refs/tags/blob", BLOB, None, None),
    ("refs/tags/tagBlob", BLOB_TAG, None, Some(BLOB)),
    ("refs/tags/tagC", TAG, None, Some(C)),
    ("refs/tags/tree", TREE, None, None),
];

impl Repository for Fixture {
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn head(&self) -> ObjectId {
        self.ids[D]
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects.get(index).map(|object| object.0)
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.objects
            .get(index)
            .map(|object| ObjectInfo {
                kind: object.1,
                size: object.2.len() as u64,
            })
            .ok_or(Error::ObjectNotFound)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        if self.failed.get() {
            return Err(Error::Backend);
        }
        let object = self.objects.get(index).ok_or(Error::ObjectNotFound)?;
        let bytes = object
            .2
            .get(usize::try_from(offset).map_err(|_| Error::InvalidObject)?..)
            .ok_or(Error::InvalidObject)?;
        let count = bytes.len().min(storage.len());
        self.bytes.set(self.bytes.get() + count as u64);
        storage[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn find_index(&self, _: ObjectId) -> Option<usize> {
        panic!("use validated index")
    }
    fn reference_count(&self) -> usize {
        REFS.len()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        REFS.get(index).map(|entry| entry.0)
    }
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        REFS.get(index).map(|entry| self.ids[entry.1])
    }
    fn reference_symbolic_target(&self, index: usize) -> Option<&str> {
        REFS.get(index).and_then(|entry| entry.2)
    }
    fn reference_peeled(&self, index: usize) -> Option<ObjectId> {
        REFS.get(index)
            .and_then(|entry| entry.3)
            .map(|index| self.ids[index])
    }
}
