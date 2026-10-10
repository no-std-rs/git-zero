mod common;

use git_zero::{
    Entry, EntryKind, Error, File, ObjectKind, Repository, Snapshot,
    client::{self, Transport},
    http::{self, Request},
    io::SliceWriter,
    pack, pktline,
    repository::validate_ref_name,
    snapshot::MAX_TREE_DEPTH,
};

#[test]
fn nested_snapshot_deduplicates_objects_and_preserves_flat_ids() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let repo = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit).unwrap();
    // One commit, root/shared/empty trees, and two blobs shared across modes.
    assert_eq!(repo.object_count(), 6);
    let objects: Vec<_> = (0..repo.object_count())
        .map(|index| repo.object(index).unwrap())
        .collect();
    assert_eq!(objects[0].id(), repo.head());
    assert_eq!(objects[1].kind, ObjectKind::Tree);
    assert_eq!(
        objects
            .iter()
            .filter(|object| object.kind == ObjectKind::Tree)
            .count(),
        3
    );
    assert_eq!(
        repo.object(repo.object_count()).map(|object| object.id()),
        None
    );
    for object in &objects {
        assert_eq!(repo.find(object.id()).unwrap().data, object.data);
    }
    assert!(objects.iter().any(|object| {
        object.kind == ObjectKind::Tree
            && object.id().to_string() == "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    }));

    let files = [File {
        name: "regular",
        contents: b"hello\n",
    }];
    let entries = [Entry {
        name: "regular",
        kind: EntryKind::File(b"hello\n"),
    }];
    let mut flat_tree = [0; 35]; // Exact raw tree size, with no framing overhead.
    let mut flat_commit = [0; 512];
    let flat = Snapshot::new(&files, &mut flat_tree, &mut flat_commit).unwrap();
    let mut nested_tree = [0; 39];
    let mut nested_commit = [0; 512];
    let nested = Snapshot::from_entries(&entries, &mut nested_tree, &mut nested_commit).unwrap();
    assert_eq!(nested.head(), flat.head());
    assert_eq!(nested.object(1).unwrap().data, flat.object(1).unwrap().data);
    let mut short = [0; 38];
    assert!(matches!(
        Snapshot::from_entries(&entries, &mut short, &mut commit),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        Snapshot::from_entries(common::ENTRIES, &mut trees, &mut []),
        Err(Error::BufferTooSmall)
    ));
}

#[test]
fn nested_snapshot_checks_git_order_duplicates_and_child_names() {
    let file = Entry {
        name: "a.c",
        kind: EntryKind::File(b""),
    };
    let tree = Entry {
        name: "a",
        kind: EntryKind::Tree(&[]),
    };
    assert!(file.cmp_name(&tree).is_lt());
    let mut storage = [0; 1024];
    let mut commit = [0; 512];
    assert!(Snapshot::from_entries(&[file, tree], &mut storage, &mut commit).is_ok());
    assert!(matches!(
        Snapshot::from_entries(&[tree, file], &mut storage, &mut commit),
        Err(Error::UnsortedFiles)
    ));
    // File/directory duplicates can be nonadjacent in Git's mode-aware order.
    let duplicate = Entry {
        name: "a",
        kind: EntryKind::File(b""),
    };
    assert!(matches!(
        Snapshot::from_entries(&[duplicate, file, tree], &mut storage, &mut commit),
        Err(Error::UnsortedFiles)
    ));
    for name in ["", ".", "..", ".git", ".GIT", "a/b", "a\\b", "a\0b"] {
        let children = [Entry {
            name,
            kind: EntryKind::File(b""),
        }];
        let entries = [Entry {
            name: "valid",
            kind: EntryKind::Tree(&children),
        }];
        assert!(matches!(
            Snapshot::from_entries(&entries, &mut storage, &mut commit),
            Err(Error::InvalidFileName)
        ));
    }
    let unsorted_children = [file, duplicate];
    let entries = [Entry {
        name: "valid",
        kind: EntryKind::Tree(&unsorted_children),
    }];
    assert!(matches!(
        Snapshot::from_entries(&entries, &mut storage, &mut commit),
        Err(Error::UnsortedFiles)
    ));
}

#[test]
fn nested_snapshot_bounds_depth_and_child_storage() {
    fn check_chain(
        depth: usize,
        children: &[Entry<'_>],
        storage: &mut [u8],
        commit: &mut [u8],
    ) -> git_zero::Result<()> {
        if depth == 0 {
            return Snapshot::from_entries(children, storage, commit).map(|_| ());
        }
        let entries = [Entry {
            name: "dir",
            kind: EntryKind::Tree(children),
        }];
        check_chain(depth - 1, &entries, storage, commit)
    }
    let mut storage = [0; 4096];
    let mut commit = [0; 512];
    assert!(check_chain(MAX_TREE_DEPTH - 1, &[], &mut storage, &mut commit).is_ok());
    assert!(matches!(
        check_chain(MAX_TREE_DEPTH, &[], &mut storage, &mut commit),
        Err(Error::Unsupported)
    ));
    let entries = [Entry {
        name: "empty",
        kind: EntryKind::Tree(&[]),
    }];
    // Parent record fits; the child empty-tree record does not.
    let mut short = [0; 36];
    assert!(matches!(
        Snapshot::from_entries(&entries, &mut short, &mut commit),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        Snapshot::from_entries(&[], &mut [], &mut commit),
        Err(Error::BufferTooSmall)
    ));
    let mut empty = [0; 4];
    assert_eq!(
        Snapshot::from_entries(&[], &mut empty, &mut commit)
            .unwrap()
            .object_count(),
        2
    );
}

#[test]
fn ref_validation_rejects_invalid_names_and_configuration() {
    for name in [
        "main",
        "HEAD",
        "refs/",
        "refs//main",
        "refs/heads/.hidden",
        "refs/heads/a.lock",
        "refs/heads/a..b",
        "refs/heads/a@{b",
        "refs/heads/a b",
        "refs/heads/a\nb",
        "refs/heads/a\u{7f}b",
        "refs/heads/a~b",
        "refs/heads/a^b",
        "refs/heads/a:b",
        "refs/heads/a?b",
        "refs/heads/a*b",
        "refs/heads/a[b",
        "refs/heads/a\\b",
        "refs/heads/a.",
    ] {
        assert_eq!(
            validate_ref_name(name),
            Err(Error::InvalidReference),
            "{name:?}"
        );
    }
    for name in [
        "refs/heads/main",
        "refs/heads/release/v1",
        "refs/tags/v1",
        "refs/custom/é",
    ] {
        validate_ref_name(name).unwrap();
    }
    let mut trees = [0; 4];
    let mut commit = [0; 512];
    for (head, refs) in [
        ("refs/heads/main", &[][..]),
        ("refs/tags/v1", &["refs/tags/v1"][..]),
        (
            "refs/heads/main",
            &["refs/heads/main", "refs/heads/main"][..],
        ),
        ("refs/heads/main", &["refs/tags/v1", "refs/heads/main"][..]),
        (
            "refs/heads/a",
            &["refs/heads/a", "refs/heads/a-b", "refs/heads/a/child"][..],
        ),
        (
            "refs/heads/main",
            &["refs/heads/main", "refs/tags/bad.lock"][..],
        ),
    ] {
        let repo = Snapshot::from_entries(&[], &mut trees, &mut commit).unwrap();
        assert!(matches!(
            repo.with_refs(head, refs),
            Err(Error::InvalidReference)
        ));
    }
}

struct Loopback<'a> {
    repo: &'a Snapshot<'a>,
    dumb: bool,
}

impl Transport for Loopback<'_> {
    fn request(
        &mut self,
        mut request: Request<'_>,
        storage: &mut [u8],
    ) -> git_zero::Result<(u16, usize)> {
        if self.dumb {
            request.query = None;
        }
        let mut output = SliceWriter::new(storage);
        let response = http::serve(self.repo, request, &mut output)?;
        Ok((response.status, output.len()))
    }
}

#[test]
fn configured_refs_drive_both_http_protocols_without_changing_ids() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let repo = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit).unwrap();
    let id = repo.head();
    let repo = repo.with_refs(common::HEAD_REF, common::REFS).unwrap();
    assert_eq!(repo.head(), id);
    assert_eq!(repo.reference_name(common::REFS.len()), None);
    for dumb in [false, true] {
        let mut transport = Loopback { repo: &repo, dumb };
        let mut discovery_storage = [0; 4096];
        let discovery = client::discover(&mut transport, &mut discovery_storage).unwrap();
        let refs: Vec<_> = discovery
            .references()
            .map(|reference| reference.unwrap())
            .collect();
        let advertised_names: Vec<_> = refs.iter().map(|reference| reference.name).collect();
        let expected_names: Vec<_> = if dumb {
            common::REFS.to_vec()
        } else {
            std::iter::once("HEAD")
                .chain(common::REFS.iter().copied())
                .collect()
        };
        assert_eq!(advertised_names, expected_names);
        assert!(refs.iter().all(|reference| reference.id == id));
        let mut response = [0; 4096];
        assert_eq!(
            client::read_head(&mut transport, &discovery, &mut response).unwrap(),
            id
        );
        for (path, name) in [
            ("HEAD", "HEAD"),
            ("refs/heads/alternate", common::REFS[0]),
            ("refs/heads/releases/current%23%25%c3%a9+", common::HEAD_REF),
            ("refs/tags/snapshot%2523", common::REFS[2]),
        ] {
            let (status, length) = transport
                .request(
                    Request {
                        method: "GET",
                        path,
                        query: None,
                        body: &[],
                    },
                    &mut response,
                )
                .unwrap();
            assert_eq!(status, 200);
            let expected = if name == "HEAD" {
                format!("ref: {}\n", common::HEAD_REF)
            } else {
                format!("{id}\n")
            };
            assert_eq!(&response[..length], expected.as_bytes());
        }
        for path in [
            "refs/heads/main",
            "refs/heads/releases/current%23%25%c3%a9+%",
            "refs/heads/releases/current%23%GG%c3%a9+",
            "refs/tags/snapshot%23",
        ] {
            let (status, _) = transport
                .request(
                    Request {
                        method: "GET",
                        path,
                        query: None,
                        body: &[],
                    },
                    &mut response,
                )
                .unwrap();
            assert_eq!(status, 404, "{path}");
        }
        let mut request = [0; 128];
        let bytes = client::fetch_pack(&mut transport, id, &mut request, &mut response).unwrap();
        pack::validate(bytes).unwrap();
        assert_eq!(&bytes[8..12], &6_u32.to_be_bytes());
    }
}

#[test]
fn ref_lengths_fit_smart_advertisements() {
    // HEAD includes its symref capability; alias packets have 17 fewer bytes.
    let head = format!("refs/heads/{}", "x".repeat(65_457 - "refs/heads/".len()));
    let alias = format!("refs/tags/{}", "x".repeat(65_474 - "refs/tags/".len()));
    let refs = [head.as_str(), alias.as_str()];
    let mut trees = [0; 4];
    let mut commit = [0; 512];
    let repo = Snapshot::from_entries(&[], &mut trees, &mut commit)
        .unwrap()
        .with_refs(&head, &refs)
        .unwrap();
    let mut transport = Loopback {
        repo: &repo,
        dumb: false,
    };
    let mut storage = vec![0; 3 * pktline::MAX_LENGTH + 128];
    let discovery = client::discover(&mut transport, &mut storage).unwrap();
    let names: Vec<_> = discovery
        .references()
        .map(|reference| reference.unwrap().name)
        .collect();
    assert_eq!(names, ["HEAD", head.as_str(), alias.as_str()]);

    let oversized_head = format!("{head}x");
    let oversized_alias = format!("{alias}x");
    for (head, refs) in [
        (
            oversized_head.as_str(),
            [oversized_head.as_str(), alias.as_str()],
        ),
        (head.as_str(), [head.as_str(), oversized_alias.as_str()]),
    ] {
        let mut trees = [0; 4];
        let mut commit = [0; 512];
        let repo = Snapshot::from_entries(&[], &mut trees, &mut commit).unwrap();
        assert!(matches!(
            repo.with_refs(head, &refs),
            Err(Error::InvalidReference)
        ));
    }
}

#[test]
fn long_refs_do_not_require_a_fixed_packet_scratch_buffer() {
    let name = format!("refs/heads/{}", "x".repeat(1024));
    let refs = [name.as_str()];
    let mut trees = [0; 4];
    let mut commit = [0; 512];
    let repo = Snapshot::from_entries(&[], &mut trees, &mut commit)
        .unwrap()
        .with_refs(&name, &refs)
        .unwrap();
    let mut storage = [0; 4096];
    let mut transport = Loopback {
        repo: &repo,
        dumb: false,
    };
    let discovery = client::discover(&mut transport, &mut storage).unwrap();
    assert_eq!(discovery.references().nth(1).unwrap().unwrap().name, name);
    let mut output = SliceWriter::new(&mut storage);
    assert_eq!(
        pktline::write_parts(&mut output, &[&[0; pktline::MAX_LENGTH - 4], b"x"]),
        Err(Error::InvalidPacket)
    );
    assert!(output.is_empty());
}
