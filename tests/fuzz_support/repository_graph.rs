use super::MAX_INPUT;
use git_zero::{Object, ObjectId, ObjectKind, Repository, Result, ValidatedRepository};
#[path = "../common/short_reads.rs"]
mod short_reads;
use short_reads::Streamed;

struct Graph<'a> {
    objects: [Object<'a>; 3],
}

impl Repository for Graph<'_> {
    fn head(&self) -> ObjectId {
        self.objects[0].id()
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).copied()
    }
}

/// Compare borrowed and short-read validation of one mutated object record.
///
/// Byte zero chooses a tree (even) or commit (odd); byte one sets a read limit of
/// 1..=64 bytes. Remaining bytes replace that record in a three-object snapshot
/// containing a fixed blob. Tree mutations rebuild their enclosing commit with
/// the new tree ID so failures reach structural parsing. Both providers use three metadata/index
/// slots, 64-byte read scratch, and 128-byte parse scratch. Their results must
/// match, including errors; successful graphs preserve HEAD and all objects.
pub fn graph(data: &[u8]) {
    if !(2..=MAX_INPUT).contains(&data.len()) {
        return;
    }
    let blob = Object {
        kind: ObjectKind::Blob,
        data: b"seed\n",
    };
    let mutate_tree = data[0] & 1 == 0;
    // The independent tree seed supplies the canonical tree for commit mutations.
    let tree_storage = &include_bytes!("../../fuzz/corpus/graph/tree")[2..];
    let tree_bytes = if mutate_tree {
        &data[2..]
    } else {
        tree_storage
    };
    let tree = Object {
        kind: ObjectKind::Tree,
        data: tree_bytes,
    };
    // Recompute IDs after mutations so malformed records reach the graph parser
    // rather than failing only at the initial object checksum check.
    let commit_storage;
    let commit_bytes = if mutate_tree {
        commit_storage = format!(
            "tree {}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nfuzz\n",
            tree.id()
        );
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
    let repository = Graph { objects };
    let streamed = Streamed {
        source: &repository,
        limit: usize::from(data[1] % 64) + 1,
    };
    assert_eq!(validate(&repository), validate(&streamed));
}

fn validate(repository: &dyn Repository) -> Result<()> {
    ValidatedRepository::new(
        repository,
        &mut [None; 3],
        &mut [0; 3],
        &mut [0; 64],
        &mut [0; 128],
    )
    .map(|validated| {
        assert_eq!(
            (validated.head(), validated.object_count()),
            (repository.head(), 3)
        );
    })
}
