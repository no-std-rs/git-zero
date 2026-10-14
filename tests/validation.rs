#![cfg(all(feature = "read", feature = "server"))]

#[path = "common/short_reads.rs"]
mod short_reads;
use short_reads::Streamed;
mod common;
#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    Error, Object, ObjectId, ObjectInfo, ObjectKind, Repository, Result, Snapshot,
    ValidatedRepository, ValidationMetadata,
    http::{self, Request},
    io::SliceWriter,
    pack,
};
use std::{
    cell::Cell,
    io::Write as _,
    process::{Command, Stdio},
};

#[derive(Clone)]
struct OwnedObject {
    kind: ObjectKind,
    data: Vec<u8>,
}

impl OwnedObject {
    fn borrowed(&self) -> Object<'_> {
        Object {
            kind: self.kind,
            data: &self.data,
        }
    }
    fn id(&self) -> ObjectId {
        self.borrowed().id()
    }
}

struct Fixture {
    objects: Vec<OwnedObject>,
    head: ObjectId,
    refs: Vec<String>,
    head_ref: String,
}

impl Fixture {
    fn new(objects: Vec<OwnedObject>, head: ObjectId) -> Self {
        Self {
            objects,
            head,
            refs: vec!["refs/heads/main".into()],
            head_ref: "refs/heads/main".into(),
        }
    }
    fn tree(data: Vec<u8>, mut children: Vec<OwnedObject>) -> Self {
        let tree = OwnedObject {
            kind: ObjectKind::Tree,
            data,
        };
        let commit = commit(tree.id(), &[], b"fixture\n");
        let head = commit.id();
        // Deliberately enumerate children before HEAD, without sorting by ID.
        children.push(tree);
        children.push(commit);
        Self::new(children, head)
    }
}

impl Repository for Fixture {
    fn head(&self) -> ObjectId {
        self.head
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).map(OwnedObject::borrowed)
    }
    fn head_ref(&self) -> &str {
        &self.head_ref
    }
    fn reference_count(&self) -> usize {
        self.refs.len()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.refs.get(index).map(String::as_str)
    }
}

fn commit(tree: ObjectId, parents: &[ObjectId], message: &[u8]) -> OwnedObject {
    let mut data = format!("tree {tree}\n").into_bytes();
    for parent in parents {
        data.extend_from_slice(format!("parent {parent}\n").as_bytes());
    }
    data.extend_from_slice(b"author A <a@example.invalid> 1700000000 +0000\ncommitter C <c@example.invalid> 1700000001 -0230\nencoding UTF-8\ngpgsig test\n continuation\n\n");
    data.extend_from_slice(message);
    OwnedObject {
        kind: ObjectKind::Commit,
        data,
    }
}

fn entry(tree: &mut Vec<u8>, mode: &[u8], name: &[u8], id: ObjectId) {
    tree.extend_from_slice(mode);
    tree.push(b' ');
    tree.extend_from_slice(name);
    tree.push(0);
    tree.extend_from_slice(&id.0);
}

fn blob() -> OwnedObject {
    OwnedObject {
        kind: ObjectKind::Blob,
        data: b"data\0\xff".to_vec(),
    }
}
fn empty_tree() -> OwnedObject {
    OwnedObject {
        kind: ObjectKind::Tree,
        data: vec![],
    }
}

fn outcome(repository: &dyn Repository, read_size: usize, parse_size: usize) -> Result<()> {
    let count = repository.object_count();
    let mut objects = vec![None; count];
    let mut by_id = vec![0; count];
    let mut read = vec![0; read_size];
    let mut parse = vec![0; parse_size];
    ValidatedRepository::new(repository, &mut objects, &mut by_id, &mut read, &mut parse)
        .map(|_| ())
}

fn tree_outcome(
    data: Vec<u8>,
    children: Vec<OwnedObject>,
    read_size: usize,
    parse_size: usize,
) -> Result<()> {
    outcome(&Fixture::tree(data, children), read_size, parse_size)
}

#[test]
fn nested_snapshot_index_preserves_metadata_refs_pack_and_http() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let snapshot = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit)
        .unwrap()
        .with_refs(common::HEAD_REF, common::REFS)
        .unwrap();
    let mut objects = [None; 9];
    let mut by_id = [usize::MAX; 9];
    let mut read = [0; 7];
    let mut parse = [0; 128];
    {
        let validated =
            ValidatedRepository::new(&snapshot, &mut objects, &mut by_id, &mut read, &mut parse)
                .unwrap();
        assert_eq!(validated.head(), snapshot.head());
        assert_eq!(validated.object_count(), snapshot.object_count());
        assert_eq!(validated.head_ref(), common::HEAD_REF);
        assert_eq!(validated.reference_count(), common::REFS.len());
        for (index, name) in common::REFS.iter().enumerate() {
            assert_eq!(validated.reference_name(index), Some(*name));
        }
        let mut paths = vec!["info/refs".to_string(), "HEAD".to_string()];
        for index in 0..validated.object_count() {
            let expected = snapshot.object(index).unwrap();
            let id = expected.id();
            assert_eq!(validated.object_id(index), Some(id));
            assert_eq!(validated.metadata(index).unwrap().id, id);
            assert_eq!(validated.metadata(index).unwrap().info, expected.info());
            assert_eq!(validated.object_info(index).unwrap(), expected.info());
            assert_eq!(validated.find_index(id), Some(index));
            assert_eq!(validated.find(id).unwrap().data, expected.data);
            let id = id.to_string();
            paths.push(format!("objects/{}/{}", &id[..2], &id[2..]));
        }
        assert!(validated.metadata(validated.object_count()).is_none());
        assert!(validated.object(validated.object_count()).is_none());
        assert_eq!(
            validated.object_info(validated.object_count()),
            Err(Error::ObjectNotFound)
        );
        assert_eq!(
            validated.read_object(validated.object_count(), 0, &mut [0]),
            Err(Error::ObjectNotFound)
        );
        assert!(validated.find_index(ObjectId([0; 20])).is_none());
        let mut expected = vec![0; 4096];
        let mut actual = vec![0; 4096];
        let mut left = SliceWriter::new(&mut expected);
        let mut right = SliceWriter::new(&mut actual);
        assert_eq!(
            pack::write(&validated, &mut right).unwrap(),
            pack::write(&snapshot, &mut left).unwrap()
        );
        assert_eq!(left.as_bytes(), right.as_bytes());
        for path in &paths {
            for query in [None, Some("service=git-upload-pack")] {
                let request = Request {
                    method: "GET",
                    path,
                    query,
                    body: &[],
                };
                let mut left = SliceWriter::new(&mut expected);
                let mut right = SliceWriter::new(&mut actual);
                let a = http::serve(&snapshot, request, &mut left).unwrap();
                let b = http::serve(&validated, request, &mut right).unwrap();
                assert_eq!(a.status, b.status);
                assert_eq!(left.as_bytes(), right.as_bytes());
            }
        }
    }
    assert!(
        objects[snapshot.object_count()..]
            .iter()
            .all(Option::is_none)
    );
    assert!(
        by_id[snapshot.object_count()..]
            .iter()
            .all(|&index| index == usize::MAX)
    );
    for size in [1, 2, 31, 1024] {
        assert_eq!(
            outcome(
                &Streamed {
                    source: &snapshot,
                    limit: 3
                },
                size,
                128
            ),
            Ok(())
        );
    }
}

#[test]
fn generated_large_bodies_and_unbounded_history_use_bounded_scratch() {
    let generated = streamed::GeneratedRepository::new(1_048_577, 139);
    assert_eq!(outcome(&generated, 257, 128), Ok(()));
    // Depth is independent of call stack or the snapshot constructor's limit.
    let blob = blob();
    let mut objects = vec![blob.clone()];
    let mut id = blob.id();
    for depth in 0..96 {
        let mut data = vec![];
        entry(
            &mut data,
            if depth == 0 { b"100644" } else { b"40000" },
            b"child",
            id,
        );
        let tree = OwnedObject {
            kind: ObjectKind::Tree,
            data,
        };
        id = tree.id();
        objects.push(tree);
    }
    let root = commit(id, &[], b"root");
    let mut head = root.id();
    objects.push(root);
    for index in 0..180 {
        let next = commit(id, &[head], format!("commit {index}").as_bytes());
        head = next.id();
        objects.push(next);
    }
    let fixture = Fixture::new(objects, head);
    assert_eq!(
        outcome(
            &Streamed {
                source: &fixture,
                limit: 1
            },
            7,
            128
        ),
        Ok(())
    );
}

#[test]
fn graph_rejects_missing_targets_wrong_kinds_extra_and_duplicate_objects() {
    let blob = blob();
    let empty = empty_tree();
    for (kind, target, children, expected) in [
        (
            b"100644".as_slice(),
            ObjectId([9; 20]),
            vec![],
            Error::ObjectNotFound,
        ),
        (
            b"40000".as_slice(),
            blob.id(),
            vec![blob.clone()],
            Error::InvalidObject,
        ),
        (
            b"100644".as_slice(),
            empty.id(),
            vec![empty.clone()],
            Error::InvalidObject,
        ),
        (
            b"100644".as_slice(),
            blob.id(),
            vec![blob.clone(), empty.clone()],
            Error::InvalidObject,
        ),
        (
            b"100644".as_slice(),
            blob.id(),
            vec![blob.clone(), blob.clone()],
            Error::InvalidObject,
        ),
    ] {
        let mut tree = vec![];
        entry(&mut tree, kind, b"x", target);
        assert_eq!(tree_outcome(tree, children, 7, 128), Err(expected));
    }
    let mut fixture = Fixture::tree(vec![], vec![]);
    fixture.head = empty.id();
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidObject));
    fixture.head = ObjectId([9; 20]);
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::ObjectNotFound));
    fixture.head = ObjectId([0; 20]);
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidObject));
    let parent = commit(empty.id(), &[], b"parent");
    let child = commit(empty.id(), &[parent.id()], b"child");
    let head = child.id();
    let fixture = Fixture::new(vec![empty.clone(), child], head);
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::ObjectNotFound));
    let child = commit(empty.id(), &[blob.id()], b"child");
    let head = child.id();
    let fixture = Fixture::new(vec![empty, blob, child], head);
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidObject));
    let tag = OwnedObject {
        kind: ObjectKind::Tag,
        data: vec![],
    };
    let mut fixture = Fixture::tree(vec![], vec![]);
    fixture.objects.push(tag);
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidObject));
    assert_eq!(
        outcome(&Fixture::new(vec![], ObjectId([9; 20])), 7, 128),
        Err(Error::ObjectNotFound)
    );
}

#[test]
fn tree_order_names_modes_and_framing_are_checked_across_read_boundaries() {
    let blob = blob();
    let empty = empty_tree();
    for names in [
        [b"b".as_slice(), b"a".as_slice()],
        [b"a".as_slice(), b"a".as_slice()],
    ] {
        let mut tree = vec![];
        for name in names {
            entry(&mut tree, b"100644", name, blob.id());
        }
        assert_eq!(
            tree_outcome(tree, vec![blob.clone()], 1, 128),
            Err(Error::UnsortedFiles)
        );
    }
    let mut tree = vec![];
    entry(&mut tree, b"40000", b"foo", empty.id());
    entry(&mut tree, b"100644", b"foo.bar", blob.id());
    assert_eq!(
        tree_outcome(tree, vec![blob.clone(), empty.clone()], 31, 128),
        Err(Error::UnsortedFiles)
    );
    let mut tree = vec![];
    entry(&mut tree, b"100644", b"foo", blob.id());
    entry(&mut tree, b"100644", b"foo.bar", blob.id());
    entry(&mut tree, b"40000", b"foo", empty.id());
    let fixture = Fixture::tree(tree, vec![blob.clone(), empty.clone()]);
    for read in [1, 7, 4096] {
        assert_eq!(outcome(&fixture, read, 129), Err(Error::UnsortedFiles));
    }
    for name in [b"".as_slice(), b".", b"..", b".GiT", b"a/b", b"a\\b"] {
        let mut tree = vec![];
        entry(&mut tree, b"100644", name, blob.id());
        assert_eq!(
            tree_outcome(tree, vec![blob.clone()], 1, 128),
            Err(Error::InvalidFileName)
        );
    }
    for mode in [
        b"040000".as_slice(),
        b"100664",
        b"100600",
        b"0",
        b"abcdef",
        b"1234567",
    ] {
        let mut tree = vec![];
        entry(&mut tree, mode, b"x", blob.id());
        assert_eq!(
            tree_outcome(tree, vec![blob.clone()], 1, 128),
            Err(Error::InvalidObject)
        );
    }
    let mut complete = vec![];
    entry(&mut complete, b"100644", b"x", blob.id());
    for length in 1..complete.len() {
        assert_eq!(
            tree_outcome(complete[..length].to_vec(), vec![blob.clone()], 1, 128),
            Err(Error::InvalidObject),
            "truncated at {length}"
        );
    }
    let mut tree = vec![];
    entry(&mut tree, b"100644", b"x", ObjectId([0; 20]));
    assert_eq!(
        tree_outcome(tree, vec![], 1, 128),
        Err(Error::InvalidObject)
    );
    // Binary names are legal Git bytes, independently of UTF-8.
    let mut tree = vec![];
    entry(&mut tree, b"100644", b"\xfe", blob.id());
    entry(&mut tree, b"100755", b"\xff", blob.id());
    assert_eq!(tree_outcome(tree, vec![blob], 1, 128), Ok(()));
}

#[test]
fn commit_headers_reject_broken_edges_and_missing_or_reordered_fields() {
    let empty = empty_tree();
    let good = commit(empty.id(), &[], b"message\0tree bogus");
    let text = String::from_utf8(good.data.clone()).unwrap();
    let tree_line = format!("tree {}", empty.id());
    for data in [
        text.replace(&tree_line, "tree xyz"),
        text.replace(&tree_line, &format!("tree {}", "0".repeat(40))),
        text.replace("author A <a@example.invalid> 1700000000 +0000\n", ""),
        text.replace("committer C <c@example.invalid> 1700000001 -0230\n", ""),
        text.replace("author A", "author\0A"),
        text.replace("encoding UTF-8", &tree_line),
        text.replace("encoding UTF-8", "parent xyz"),
        text.replace("encoding UTF-8", "no-separator"),
        format!("tree {}\nauthor A\ncommitter C\n", empty.id()),
    ] {
        let object = OwnedObject {
            kind: ObjectKind::Commit,
            data: data.into_bytes(),
        };
        let head = object.id();
        let fixture = Fixture::new(vec![empty.clone(), object], head);
        assert_eq!(outcome(&fixture, 1, 128), Err(Error::InvalidObject));
    }
    let head = good.id();
    let fixture = Fixture::new(vec![empty, good], head);
    assert_eq!(outcome(&fixture, 1, 128), Ok(())); // Message is opaque graph data.
}

#[test]
fn refs_and_parse_capacity_have_explicit_limits() {
    let mut fixture = Fixture::tree(vec![], vec![]);
    for refs in [
        vec![],
        vec!["refs/heads/other"],
        vec!["refs/heads/main", "refs/heads/main"],
        vec!["refs/heads/main", "refs/heads/main/child"],
        vec!["refs/heads/z", "refs/heads/main"],
        vec!["refs/heads/../main"],
    ] {
        fixture.refs = refs.into_iter().map(String::from).collect();
        assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidReference));
    }
    fixture.refs = vec!["refs/heads/main".into()];
    fixture.head_ref = "refs/tags/main".into();
    assert_eq!(outcome(&fixture, 7, 128), Err(Error::InvalidReference));
    fixture.head_ref = "refs/heads/main".into();
    assert_eq!(outcome(&fixture, 0, 128), Err(Error::BufferTooSmall));
    assert_eq!(outcome(&fixture, 1, 0), Err(Error::BufferTooSmall));
    let longest = fixture
        .objects
        .last()
        .unwrap()
        .data
        .split(|&byte| byte == b'\n')
        .map(<[u8]>::len)
        .max()
        .unwrap();
    assert_eq!(outcome(&fixture, 1, longest), Ok(()));
    assert_eq!(
        outcome(&fixture, 1, longest - 1),
        Err(Error::BufferTooSmall)
    );
    let blob = blob();
    let mut tree = vec![];
    entry(&mut tree, b"100644", &[b'x'; 80], blob.id());
    let fixture = Fixture::tree(tree, vec![blob]);
    assert_eq!(outcome(&fixture, 1, 160), Ok(()));
    for parse in [128, 159] {
        assert_eq!(outcome(&fixture, 1, parse), Err(Error::BufferTooSmall));
    }
}

struct Broken<'a> {
    source: &'a dyn Repository,
    fault: u8,
    reads: Cell<usize>,
}
impl Repository for Broken<'_> {
    fn head(&self) -> ObjectId {
        self.source.head()
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        if self.fault == 0 {
            return Err(Error::ObjectNotFound);
        }
        self.source.object_info(index)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        match self.fault {
            1 => None,
            2 => Some(ObjectId([9; 20])),
            _ => self.source.object_id(index),
        }
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        self.reads.set(self.reads.get() + 1);
        match self.fault {
            3 => Err(Error::Backend),
            4 => Ok(0),
            5 => Ok(storage.len() + 1),
            _ => self.source.read_object(index, offset, storage),
        }
    }
}

#[test]
fn capacity_and_provider_failures_never_produce_a_validated_wrapper() {
    let fixture = Fixture::tree(vec![], vec![]);
    for (fault, expected) in [
        (0, Error::ObjectNotFound),
        (1, Error::ObjectNotFound),
        (2, Error::InvalidObject),
        (3, Error::Backend),
        (4, Error::InvalidObject),
        (5, Error::InvalidObject),
    ] {
        let broken = Broken {
            source: &fixture,
            fault,
            reads: Cell::new(0),
        };
        assert_eq!(outcome(&broken, 7, 128), Err(expected));
    }
    let broken = Broken {
        source: &fixture,
        fault: 3,
        reads: Cell::new(0),
    };
    let count = fixture.object_count();
    for (metadata_size, index_size) in [(count - 1, count), (count, count - 1)] {
        let mut objects: Vec<Option<ValidationMetadata>> = vec![None; metadata_size];
        let mut by_id = vec![usize::MAX; index_size];
        assert!(matches!(
            ValidatedRepository::new(
                &broken,
                &mut objects,
                &mut by_id,
                &mut [0; 7],
                &mut [0; 128]
            ),
            Err(Error::BufferTooSmall)
        ));
        assert!(objects.iter().all(Option::is_none));
        assert!(by_id.iter().all(|&index| index == usize::MAX));
        assert_eq!(broken.reads.get(), 0);
    }
}

#[test]
fn strict_git_accepts_validated_streamed_merge_history_and_gitlinks() {
    let blob = blob();
    let empty = empty_tree();
    let mut data = vec![];
    entry(&mut data, b"100644", b"a.c", blob.id());
    entry(&mut data, b"40000", b"a", empty.id());
    entry(&mut data, b"40000", b"b", empty.id());
    entry(&mut data, b"160000", b"external", ObjectId([9; 20]));
    entry(&mut data, b"100755", b"run", blob.id());
    entry(&mut data, b"120000", b"target", blob.id());
    let tree = OwnedObject {
        kind: ObjectKind::Tree,
        data,
    };
    let first = commit(tree.id(), &[], b"first");
    let second = commit(tree.id(), &[first.id()], b"second");
    let branch = commit(tree.id(), &[first.id()], b"branch");
    let merge = commit(tree.id(), &[second.id(), branch.id()], b"merge");
    let fixture = Fixture::new(
        vec![blob, empty, tree, first, branch, merge.clone(), second],
        merge.id(),
    );
    let source = Streamed {
        source: &fixture,
        limit: 3,
    };
    let mut metadata = vec![None; fixture.object_count()];
    let mut by_id = vec![0; fixture.object_count()];
    let validated = ValidatedRepository::new(
        &source,
        &mut metadata,
        &mut by_id,
        &mut [0; 7],
        &mut [0; 128],
    )
    .unwrap();
    for index in 0..validated.object_count() {
        assert!(validated.object(index).is_none());
        assert_eq!(
            validated.find_index(fixture.object_id(index).unwrap()),
            Some(index)
        );
    }
    let directory =
        std::env::temp_dir().join(format!("git-zero-validation-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let git = || {
        let mut command = Command::new("git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&directory);
        command
    };
    assert!(
        git()
            .args(["init", "-q", "--bare", "--object-format=sha1"])
            .status()
            .unwrap()
            .success()
    );
    let mut bytes = vec![0; 8192];
    let mut output = SliceWriter::new(&mut bytes);
    pack::write_with_scratch(&validated, &mut [0; 1], &mut output).unwrap();
    let mut child = git()
        .args(["index-pack", "--strict", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(output.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        git()
            .args([
                "update-ref",
                "refs/heads/main",
                &validated.head().to_string()
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        git()
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .status()
            .unwrap()
            .success()
    );
    let result = git().args(["fsck", "--strict"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = git()
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"4\n");
}
