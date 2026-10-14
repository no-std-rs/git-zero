#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, ObjectId, ObjectKind,
    object::{Commit, Tag, Tree, TreeEntry},
};

fn tree(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (mode, name) in entries {
        bytes.extend_from_slice(mode);
        bytes.push(b' ');
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(&[7; 20]);
    }
    bytes
}

#[test]
fn git_tree_order_modes_and_byte_names_use_borrowed_storage() {
    let bytes = tree(&[
        (b"100644", b"a.txt"),
        (b"40000", b"a"),
        (b"100755", b"bin"),
        (b"120000", b"link"),
        (b"160000", b"submodule"),
        (b"100644", b"\xff"),
    ]);
    allocations::without_allocations(|| {
        let parsed = Tree::parse(&bytes).unwrap();
        assert_eq!(parsed.entries().count(), 6);
        let mut offset = 0;
        for expected in parsed.entries() {
            assert_eq!(
                TreeEntry::read(&bytes, &mut offset).unwrap(),
                Some(expected)
            );
            assert_eq!(expected.id, ObjectId([7; 20]));
        }
        assert_eq!(offset, bytes.len());
        assert!(TreeEntry::read(&bytes, &mut offset).unwrap().is_none());
    });
    // A shared Git parser leaves a filesystem's component-size limit to its host.
    let long = vec![b'x'; 300];
    let bytes = tree(&[(b"100644", &long)]);
    assert_eq!(
        Tree::parse(&bytes)
            .unwrap()
            .entries()
            .next()
            .unwrap()
            .name
            .len(),
        300
    );
}

#[test]
fn malformed_trees_and_nonadjacent_file_directory_aliases_are_rejected() {
    for entries in [
        vec![(b"100644".as_slice(), b"..".as_slice())],
        vec![(b"100644".as_slice(), b"a/b".as_slice())],
        vec![(b"100644".as_slice(), b"".as_slice())],
        vec![(b"0100644".as_slice(), b"a".as_slice())],
        vec![(b"100644".as_slice(), b"z".as_slice()), (b"40000", b"a")],
        vec![(b"100644".as_slice(), b"a".as_slice()), (b"100755", b"a")],
        // These names are correctly ordered by Git, yet repeat the name "a".
        vec![
            (b"100644".as_slice(), b"a".as_slice()),
            (b"100644", b"a.txt"),
            (b"40000", b"a"),
        ],
    ] {
        assert!(Tree::parse(&tree(&entries)).is_err());
    }
    let valid = tree(&[(b"100644", b"a")]);
    for end in 1..valid.len() {
        assert!(Tree::parse(&valid[..end]).is_err());
    }
    for end in 1..valid.len() {
        let mut offset = 0;
        assert!(TreeEntry::read(&valid[..end], &mut offset).is_err());
        assert_eq!(offset, 0);
    }
    assert!(TreeEntry::read(&valid, &mut (valid.len() + 1)).is_err());
}

#[test]
fn commits_preserve_empty_binary_and_multiline_messages() {
    for message in [
        &b""[..],
        &b"subject\n\nbody\n"[..],
        &b"binary\0message\xff\n"[..],
    ] {
        let mut bytes =
            b"tree 0123456789abcdef0123456789abcdef01234567\nauthor example\n\n".to_vec();
        let offset = bytes.len();
        bytes.extend_from_slice(message);
        allocations::without_allocations(|| {
            let commit = Commit::parse(&bytes).unwrap();
            assert_eq!(commit.message(), message);
            assert_eq!(commit.message_offset(), offset);
            assert_eq!(&bytes[commit.message_offset()..], commit.message());
            assert_eq!(
                commit.tree(),
                ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap()
            );
        });
    }
    let id = ObjectId([9; 20]);
    let bytes = format!("tree {id}\nauthor test\n\nmessage\n");
    assert_eq!(Commit::root_tree(bytes.as_bytes()), Ok(id));
    for bad in [
        b"tree 123\n".as_slice(),
        b"tree zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n",
        b"blob x",
    ] {
        assert!(Commit::root_tree(bad).is_err());
    }
    assert_eq!(
        Commit::parse(b"tree 0123456789abcdef0123456789abcdef01234567\nno separator").unwrap_err(),
        Error::InvalidObject
    );
}

#[test]
fn annotated_tag_targets_require_canonical_ids_and_kinds() {
    for (kind, expected) in [
        ("commit", ObjectKind::Commit),
        ("tree", ObjectKind::Tree),
        ("blob", ObjectKind::Blob),
        ("tag", ObjectKind::Tag),
    ] {
        let bytes = format!(
            "object {}\ntype {kind}\ntag v1\ntagger example\n\nmessage\n",
            ObjectId([7; 20])
        );
        allocations::without_allocations(|| {
            let tag = Tag::parse(bytes.as_bytes()).unwrap();
            assert_eq!(tag.target, ObjectId([7; 20]));
            assert_eq!(tag.target_kind, expected);
        });
    }
    for bad in [
        b"object 123\ntype commit\ntag v1\n\n".as_slice(),
        b"object 0123456789abcdef0123456789abcdef01234567\ntype unknown\ntag v1\n\n",
        b"object 0123456789abcdef0123456789abcdef01234567\ntype commit\ntag \n\n",
    ] {
        assert!(Tag::parse(bad).is_err());
    }
}
