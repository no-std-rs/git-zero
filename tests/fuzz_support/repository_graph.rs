use super::MAX_INPUT;
use git_zero::{
    Head, Object, ObjectFormat, ObjectId, ObjectKind, RefRepository, Reference, Repository, Result,
    ValidatedRepository,
};
#[path = "../common/short_reads.rs"]
mod short_reads;
use short_reads::Streamed;

struct Graph<'a> {
    format: ObjectFormat,
    objects: [Object<'a>; 3],
    count: usize,
}

impl Repository for Graph<'_> {
    fn object_format(&self) -> ObjectFormat {
        self.format
    }
    fn head(&self) -> ObjectId {
        self.objects[0].id_with_format(self.format)
    }
    fn object_count(&self) -> usize {
        self.count
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).copied()
    }
}

/// Compare borrowed and short-read validation of one mutated object record.
///
/// Bit seven selects SHA-256. Bit one selects a tag/blob graph; otherwise bit zero chooses
/// a tree or commit mutation in a three-object snapshot. Byte one sets a read
/// limit of 1..=64 bytes. Remaining bytes replace that record. Tree mutations
/// rebuild their enclosing commit so failures reach structural parsing. Both
/// providers use three metadata/index slots, 64-byte read scratch, and 128-byte
/// parse scratch. Results must match, including errors; successful graphs also
/// select their complete closure using fresh caller-owned traversal storage.
pub fn graph(data: &[u8]) {
    if !(2..=MAX_INPUT).contains(&data.len()) {
        return;
    }
    let format = if data[0] & 128 == 0 {
        ObjectFormat::Sha1
    } else {
        ObjectFormat::Sha256
    };
    let blob = Object {
        kind: ObjectKind::Blob,
        data: b"seed\n",
    };
    if data[0] & 2 != 0 {
        tagged(data, blob, format);
        return;
    }
    let mutate_tree = data[0] & 1 == 0;
    // The independent tree seed supplies the canonical tree for commit mutations.
    let tree_storage = if format == ObjectFormat::Sha1 {
        &include_bytes!("../../fuzz/corpus/graph/tree")[2..]
    } else {
        &include_bytes!("../../fuzz/corpus/graph/tree-sha256")[2..]
    };
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
            tree.id_with_format(format)
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
    let repository = Graph {
        objects,
        count: 3,
        format,
    };
    let streamed = Streamed {
        source: &repository,
        limit: usize::from(data[1] % 64) + 1,
    };
    assert_eq!(validate(&repository), validate(&streamed));
}

fn tagged(data: &[u8], blob: Object<'_>, format: ObjectFormat) {
    let tag = Object {
        kind: ObjectKind::Tag,
        data: &data[2..],
    };
    let source = Graph {
        format,
        objects: [tag, blob, blob],
        count: 2,
    };
    let refs = [Reference {
        name: "refs/tags/fuzz",
        id: tag.id_with_format(format),
        symbolic_target: None,
        peeled: Some(blob.id_with_format(format)),
    }];
    let borrowed = RefRepository::new(&source, Head::Unborn("refs/heads/main"), &refs).unwrap();
    let streamed_source = Streamed {
        source: &source,
        limit: usize::from(data[1] % 64) + 1,
    };
    let streamed =
        RefRepository::new(&streamed_source, Head::Unborn("refs/heads/main"), &refs).unwrap();
    assert_eq!(validate(&borrowed), validate(&streamed));
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
            (repository.head(), repository.object_count())
        );
        let root = validated
            .head_state()
            .id()
            .or_else(|| validated.reference_id(0))
            .unwrap();
        let selected = validated
            .select_objects(
                &[root],
                &mut [None; 3],
                &mut [0; 3],
                &mut [0; 64],
                &mut [0; 128],
                8192,
            )
            .unwrap()
            .len();
        assert_eq!(selected, repository.object_count());
    })
}
