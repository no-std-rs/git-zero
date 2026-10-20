#[path = "common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, ObjectFormat, ObjectId, ObjectInfo, ObjectKind,
    object::{LinkDecoder, LinkLimits, LinkRelation, LinkStatus},
};

const LIMITS: LinkLimits = LinkLimits {
    max_body_bytes: u64::MAX,
    max_links: 1000,
};

#[derive(Debug, PartialEq)]
enum Event {
    Link(ObjectId, ObjectKind, LinkRelation),
    Tree(ObjectId, u32, Vec<u8>, Option<ObjectKind>, bool),
}

fn id(format: ObjectFormat, byte: u8) -> ObjectId {
    ObjectId::from_bytes(format, &[byte; 32][..format.id_length()]).unwrap()
}

fn scan(
    body: &[u8],
    kind: ObjectKind,
    format: ObjectFormat,
    chunk: usize,
    name_capacity: usize,
    limits: LinkLimits,
) -> git_zero::Result<Vec<Event>> {
    let mut names = vec![0; name_capacity * 2];
    let info = ObjectInfo {
        kind,
        size: body.len() as u64,
    };
    let mut decoder = LinkDecoder::new(info, format, &mut names, limits)?;
    let mut events = Vec::new();
    let mut offset = 0;
    for _ in 0..body.len() * 2 + 3 {
        let end = (offset + chunk).min(body.len());
        let progress = match decoder.update(&body[offset..end], end == body.len()) {
            Ok(progress) => progress,
            Err(error) => {
                assert_eq!(decoder.finish(), Err(error));
                assert_eq!(decoder.update(&[], true).unwrap_err(), error);
                return Err(error);
            }
        };
        assert!(progress.consumed <= end - offset);
        offset += progress.consumed;
        match progress.status {
            LinkStatus::Link { id, kind, relation } => events.push(Event::Link(id, kind, relation)),
            LinkStatus::TreeEntry {
                id,
                mode,
                name,
                kind,
                canonical_mode,
            } => events.push(Event::Tree(id, mode, name.to_vec(), kind, canonical_mode)),
            LinkStatus::NeedInput => assert_eq!(offset, end),
            LinkStatus::Finished => {
                assert_eq!(offset, body.len());
                decoder.finish().unwrap();
                assert_eq!(
                    decoder.update(&[], true).unwrap().status,
                    LinkStatus::Finished
                );
                assert_eq!(
                    decoder.update(b"x", true).unwrap_err(),
                    Error::InvalidObject
                );
                return Ok(events);
            }
        }
        assert_eq!(decoder.position(), offset as u64);
    }
    panic!("link scanner failed to make bounded progress");
}

#[test]
fn fragmented_commit_parents_tag_targets_and_opaque_headers_and_messages() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (tree, parent, target) = (id(format, 1), id(format, 2), id(format, 3));
        let commit = format!(
            "tree {tree}\nparent {parent}\nparent {target}\nauthor {}\ncommitter X\ngpgsig long\n {}\n\nmessage\0opaque",
            "A".repeat(1000),
            "B".repeat(1000)
        );
        for chunk in [1, 2, 7, commit.len()] {
            assert_eq!(
                scan(
                    commit.as_bytes(),
                    ObjectKind::Commit,
                    format,
                    chunk,
                    0,
                    LIMITS
                )
                .unwrap(),
                vec![
                    Event::Link(tree, ObjectKind::Tree, LinkRelation::RootTree),
                    Event::Link(parent, ObjectKind::Commit, LinkRelation::Parent),
                    Event::Link(target, ObjectKind::Commit, LinkRelation::Parent)
                ]
            );
        }
        for kind in [
            ObjectKind::Blob,
            ObjectKind::Tree,
            ObjectKind::Tag,
            ObjectKind::Commit,
        ] {
            let tag = format!(
                "object {target}\ntype {}\ntag {}\ntagger X\n\nmessage\0",
                kind.name(),
                "v".repeat(1000)
            );
            for chunk in [1, 11, tag.len()] {
                assert_eq!(
                    scan(tag.as_bytes(), ObjectKind::Tag, format, chunk, 0, LIMITS).unwrap(),
                    vec![Event::Link(target, kind, LinkRelation::TagTarget)]
                );
            }
        }
        // Git's parent list is contiguous immediately after the root tree.
        let commit = format!("tree {tree}\nauthor X\nparent {parent}\n\n");
        assert_eq!(
            scan(commit.as_bytes(), ObjectKind::Commit, format, 1, 0, LIMITS).unwrap(),
            vec![Event::Link(tree, ObjectKind::Tree, LinkRelation::RootTree)]
        );
    }
}

fn tree(entries: &[(&[u8], &[u8], ObjectId)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(mode, name, id) in entries {
        body.extend_from_slice(mode);
        body.push(b' ');
        body.extend_from_slice(name);
        body.push(0);
        body.extend_from_slice(id.as_bytes());
    }
    body
}

#[test]
fn trees_preserve_byte_names_gitlinks_and_historical_modes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let target = id(format, 1);
        let body = tree(&[
            (b"100664", b"a", target),
            (b"040000", b"b", target),
            (b"100755", b"c", target),
            (b"160000", b"d", target),
            (b"120000", b"\xff", target),
        ]);
        let expected = vec![
            Event::Tree(
                target,
                0o100664,
                b"a".to_vec(),
                Some(ObjectKind::Blob),
                false,
            ),
            Event::Tree(
                target,
                0o040000,
                b"b".to_vec(),
                Some(ObjectKind::Tree),
                false,
            ),
            Event::Tree(
                target,
                0o100755,
                b"c".to_vec(),
                Some(ObjectKind::Blob),
                true,
            ),
            Event::Tree(target, 0o160000, b"d".to_vec(), None, true),
            Event::Tree(
                target,
                0o120000,
                b"\xff".to_vec(),
                Some(ObjectKind::Blob),
                true,
            ),
        ];
        for chunk in 1..=body.len() {
            assert_eq!(
                scan(&body, ObjectKind::Tree, format, chunk, 1, LIMITS).unwrap(),
                expected
            );
        }
        assert!(
            scan(b"", ObjectKind::Tree, format, 1, 0, LIMITS)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn tree_order_and_framing_failures_are_terminal() {
    let format = ObjectFormat::Sha1;
    let target = id(format, 1);
    for entries in [
        vec![
            (b"100644".as_slice(), b"b".as_slice(), target),
            (b"100644", b"a", target),
        ],
        vec![(b"100644".as_slice(), b"a".as_slice(), target); 2],
    ] {
        assert_eq!(
            scan(&tree(&entries), ObjectKind::Tree, format, 1, 8, LIMITS),
            Err(Error::UnsortedFiles)
        );
    }
    for mode in [
        b"".as_slice(),
        b"100648",
        b"99999",
        b"777777777777777777777777777",
        b"20000",
    ] {
        assert_eq!(
            scan(
                &tree(&[(mode, b"a", target)]),
                ObjectKind::Tree,
                format,
                1,
                8,
                LIMITS
            ),
            Err(Error::InvalidObject)
        );
    }
    for name in [b"".as_slice(), b".", b"..", b"a/b"] {
        assert_eq!(
            scan(
                &tree(&[(b"100644", name, target)]),
                ObjectKind::Tree,
                format,
                1,
                8,
                LIMITS
            ),
            Err(Error::InvalidFileName)
        );
    }
    let body = tree(&[(b"100644", b"long-name", target)]);
    assert_eq!(
        scan(&body, ObjectKind::Tree, format, 1, 8, LIMITS),
        Err(Error::BufferTooSmall)
    );
    // The scanner emits these names; the host must reject the file/directory
    // alias in its unique-name index, as documented by the API contract.
    let aliases = tree(&[
        (b"100644", b"foo", target),
        (b"100644", b"foo.bar", target),
        (b"40000", b"foo", target),
    ]);
    assert_eq!(
        scan(&aliases, ObjectKind::Tree, format, 1, 8, LIMITS)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn null_tree_ids_are_invalid_even_for_external_gitlinks() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for mode in [b"100644".as_slice(), b"40000", b"120000", b"160000"] {
            let body = tree(&[(mode, b"external", format.zero_id())]);
            for chunk in [1, 7, body.len()] {
                assert_eq!(
                    scan(&body, ObjectKind::Tree, format, chunk, 16, LIMITS),
                    Err(Error::InvalidObject)
                );
            }
        }
    }
}

#[test]
fn truncation_header_shape_and_independent_limits() {
    let format = ObjectFormat::Sha256;
    let target = id(format, 1);
    let commit = format!("tree {target}\nparent {target}\nauthor X\ncommitter X\n\n");
    let tag = format!("object {target}\ntype commit\ntag v1\n\n");
    let tree = tree(&[(b"100644", b"a", target)]);
    for (kind, body) in [
        (ObjectKind::Commit, commit.as_bytes()),
        (ObjectKind::Tag, tag.as_bytes()),
        (ObjectKind::Tree, &tree),
    ] {
        for end in 0..body.len() {
            let mut names = [0; 16];
            let mut decoder = LinkDecoder::new(
                ObjectInfo {
                    kind,
                    size: body.len() as u64,
                },
                format,
                &mut names,
                LIMITS,
            )
            .unwrap();
            let mut offset = 0;
            loop {
                match decoder.update(&body[offset..end], true) {
                    Ok(progress) => offset += progress.consumed,
                    Err(error) => {
                        assert_eq!(error, Error::InvalidObject);
                        break;
                    }
                }
            }
        }
        assert_eq!(
            scan(
                body,
                kind,
                format,
                1,
                8,
                LinkLimits {
                    max_body_bytes: body.len() as u64 - 1,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded)
        );
        assert_eq!(
            scan(
                body,
                kind,
                format,
                1,
                8,
                LinkLimits {
                    max_links: 0,
                    ..LIMITS
                }
            ),
            Err(Error::LimitExceeded)
        );
    }
    for bytes in [
        b"tree bad\n\n".as_slice(),
        b"tree 0000\n\n",
        b"\n",
        b"tree x\0\n\n",
    ] {
        assert_eq!(
            scan(bytes, ObjectKind::Commit, format, 1, 0, LIMITS),
            Err(Error::InvalidObject)
        );
    }
    for suffix in [
        "type unknown\ntag v1\n\n",
        "type blob\ntag \n\n",
        "type blob\nwrong v1\n\n",
    ] {
        let body = format!("object {target}\n{suffix}");
        assert_eq!(
            scan(body.as_bytes(), ObjectKind::Tag, format, 1, 0, LIMITS),
            Err(Error::InvalidObject)
        );
    }
}

#[test]
fn names_and_inline_state_do_not_allocate_or_retain_opaque_bodies() {
    let format = ObjectFormat::Sha1;
    let body = tree(&[(b"100644", b"name", id(format, 1))]);
    let mut names = [0xa5; 9];
    allocations::without_allocations(|| {
        let mut decoder = LinkDecoder::new(
            ObjectInfo {
                kind: ObjectKind::Tree,
                size: body.len() as u64,
            },
            format,
            &mut names[..8],
            LIMITS,
        )
        .unwrap();
        assert!(core::mem::size_of_val(&decoder) < 512);
        let event = decoder.update(&body, true).unwrap();
        assert!(matches!(
            event.status,
            LinkStatus::TreeEntry { name: b"name", .. }
        ));
        assert_eq!(
            decoder.update(&[], false).unwrap().status,
            LinkStatus::NeedInput
        );
        assert_eq!(decoder.finish(), Err(Error::InvalidObject));
        assert_eq!(
            decoder.update(&[], true).unwrap().status,
            LinkStatus::Finished
        );
        decoder.finish().unwrap();
    });
    assert_eq!(names[8], 0xa5);
    allocations::without_allocations(|| {
        let mut decoder = LinkDecoder::new(
            ObjectInfo {
                kind: ObjectKind::Blob,
                size: 65536,
            },
            format,
            &mut [],
            LIMITS,
        )
        .unwrap();
        assert_eq!(
            decoder.update(&[0; 65536], true).unwrap().status,
            LinkStatus::Finished
        );
        decoder.finish().unwrap();
        let mut decoder = LinkDecoder::new(
            ObjectInfo {
                kind: ObjectKind::Blob,
                size: u64::MAX,
            },
            format,
            &mut [],
            LIMITS,
        )
        .unwrap();
        assert_eq!(
            decoder.update(b"abc", false).unwrap().status,
            LinkStatus::NeedInput
        );
        assert_eq!(decoder.position(), 3);
        assert_eq!(decoder.update(&[], true).unwrap_err(), Error::InvalidObject);
    });
}
