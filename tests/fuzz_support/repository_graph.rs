use super::MAX_INPUT;
use git_zero::{
    Error, Object, ObjectId, ObjectInfo, ObjectKind, Repository, Result, ValidatedRepository,
};

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
        if !self.borrowed {
            return None;
        }
        self.objects.get(index).copied()
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
    let mut tree_storage = [0; 32];
    tree_storage[..12].copy_from_slice(b"100644 file\0");
    tree_storage[12..].copy_from_slice(&blob.id().0);
    let tree_bytes = if mutate_tree {
        &data[2..]
    } else {
        &tree_storage
    };
    let tree = Object {
        kind: ObjectKind::Tree,
        data: tree_bytes,
    };
    // Recompute IDs after mutations so malformed records reach the graph parser
    // rather than failing only at the initial object checksum check.
    let commit_storage = format!(
        "tree {}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nfuzz\n",
        tree.id()
    );
    let commit_bytes = if mutate_tree {
        commit_storage.as_bytes()
    } else {
        &data[2..]
    };
    let objects = [
        Object {
            kind: ObjectKind::Commit,
            data: commit_bytes,
        },
        tree,
        blob,
    ];
    let mut repository = Graph {
        objects,
        borrowed: true,
        read_limit: usize::from(data[1] % 64) + 1,
    };
    let expected = validate(&repository);
    repository.borrowed = false;
    assert_eq!(validate(&repository), expected);
}

fn validate(repository: &Graph<'_>) -> Result<()> {
    ValidatedRepository::new(
        repository,
        &mut [None; 3],
        &mut [0; 3],
        &mut [0; 64],
        &mut [0; 128],
    )
    .map(|validated| {
        assert_eq!(validated.head(), repository.head());
        assert_eq!(validated.object_count(), 3);
    })
}
