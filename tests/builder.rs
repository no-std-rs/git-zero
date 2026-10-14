mod common;

use git_zero::{
    CommitIdentity, CommitOptions, Entry, EntryKind, Error, File, Repository, Snapshot,
    SnapshotBuilder, io::SliceWriter, pack,
};
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[test]
fn arrival_order_preserves_existing_objects_and_default_commit() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let expected = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit).unwrap();
    for order in [[0, 1, 2, 3, 4], [4, 3, 2, 1, 0], [2, 0, 4, 1, 3]] {
        let mut slots = [Entry::EMPTY; 8];
        let mut trees = [0; 1024];
        let mut commit = [0; 512];
        let mut builder = SnapshotBuilder::new(&mut slots, &mut trees, &mut commit);
        for index in order {
            builder.push(common::ENTRIES[index]).unwrap();
        }
        assert_eq!(builder.len(), common::ENTRIES.len());
        let actual = builder.finish().unwrap();
        assert_eq!(actual.head(), expected.head());
        assert_eq!(actual.object_count(), expected.object_count());
        for index in 0..expected.object_count() {
            let actual = actual.object(index).unwrap();
            let expected = expected.object(index).unwrap();
            assert_eq!(actual.id(), expected.id());
            assert_eq!(actual.data, expected.data);
        }
    }
}

#[test]
fn rejected_insertions_leave_occupied_slots_unchanged() {
    let mut slots = [Entry::EMPTY; 2];
    let mut trees = [0; 128];
    let mut commit = [0; 512];
    {
        let mut builder = SnapshotBuilder::new(&mut slots, &mut trees, &mut commit);
        assert!(builder.is_empty());
        builder.push(common::ENTRIES[0]).unwrap(); // a.c sorts before directory a.
        builder.push(common::ENTRIES[1]).unwrap();
        assert_eq!(
            builder.push(Entry {
                name: "a",
                kind: EntryKind::File(b"other")
            }),
            Err(Error::UnsortedFiles)
        );
        assert_eq!(builder.push(Entry::EMPTY), Err(Error::InvalidFileName));
        assert_eq!(builder.push(common::ENTRIES[2]), Err(Error::BufferTooSmall));
        assert_eq!(builder.len(), 2);
    }
    assert_eq!(slots.map(|entry| entry.name), ["a.c", "a"]);
    assert!(
        matches!(slots[1].kind, EntryKind::Tree(children) if core::ptr::eq(children, common::CHILDREN))
    );
}

#[test]
fn empty_capacity_and_finish_buffers_have_explicit_bounds() {
    let mut trees = [0; 4];
    let mut commit = [0; 512];
    let mut builder = SnapshotBuilder::new(&mut [], &mut trees, &mut commit);
    assert_eq!(builder.push(common::ENTRIES[0]), Err(Error::BufferTooSmall));
    let snapshot = builder.finish().unwrap();
    let commit_length = snapshot.object(0).unwrap().data.len();
    assert_eq!(snapshot.object_count(), 2);
    assert!(
        SnapshotBuilder::new(&mut [], &mut trees, &mut commit[..commit_length])
            .finish()
            .is_ok()
    );
    assert!(matches!(
        SnapshotBuilder::new(&mut [], &mut trees, &mut commit[..commit_length - 1]).finish(),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        SnapshotBuilder::new(&mut [], &mut trees[..3], &mut commit).finish(),
        Err(Error::BufferTooSmall)
    ));
}

#[test]
fn borrowed_children_are_validated_at_finish() {
    let children = [common::ENTRIES[1], common::ENTRIES[0]];
    let mut slots = [Entry::EMPTY; 1];
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let mut builder = SnapshotBuilder::new(&mut slots, &mut trees, &mut commit);
    builder
        .push(Entry {
            name: "parent",
            kind: EntryKind::Tree(&children),
        })
        .unwrap();
    assert!(matches!(builder.finish(), Err(Error::UnsortedFiles)));
}

#[test]
fn custom_metadata_is_copied_and_messages_are_exact() {
    let files = [File {
        name: "a",
        contents: b"content",
    }];
    for message in ["", "first\n\nsecond <paragraph>", "with final newline\n"] {
        let mut trees = [0; 29];
        let mut commit = [0; 512];
        let snapshot = {
            let name = String::from("Zoë Example");
            let author = CommitIdentity {
                name: &name,
                email: "zoe@example.invalid",
                timestamp: 4_400_000_000,
                timezone: "+0545",
            };
            let committer = CommitIdentity {
                timestamp: i64::MAX as u64,
                timezone: "-0000",
                ..CommitIdentity::default()
            };
            Snapshot::new_with_commit(
                &files,
                &mut trees,
                &mut commit,
                CommitOptions {
                    author,
                    committer,
                    message,
                },
            )
            .unwrap()
        };
        let bytes = snapshot.object(0).unwrap().data;
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.contains("\nauthor Zoë Example <zoe@example.invalid> 4400000000 +0545\n"));
        assert!(text.contains(
            "\ncommitter git-zero <git-zero@example.invalid> 9223372036854775807 -0000\n\n"
        ));
        assert_eq!(text.split_once("\n\n").unwrap().1, message);
        // Flat construction still needs no four-byte tree prefix.
        assert_eq!(snapshot.object(1).unwrap().data.len(), 29);
    }
}

#[test]
fn malformed_metadata_fails_before_modifying_tree_storage() {
    let defaults = CommitIdentity::default();
    let invalid = [
        CommitIdentity {
            name: "",
            ..defaults
        },
        CommitIdentity {
            name: "  ",
            ..defaults
        },
        CommitIdentity {
            name: "name\nauthor forged",
            ..defaults
        },
        CommitIdentity {
            name: "name\r",
            ..defaults
        },
        CommitIdentity {
            name: "<name>",
            ..defaults
        },
        CommitIdentity {
            email: "",
            ..defaults
        },
        CommitIdentity {
            email: "email\0",
            ..defaults
        },
        CommitIdentity {
            email: "email\t",
            ..defaults
        },
        CommitIdentity {
            email: "email>",
            ..defaults
        },
        CommitIdentity {
            timestamp: i64::MAX as u64 + 1,
            ..defaults
        },
    ];
    let options = CommitOptions::default();
    let mut trees = [0x55; 4];
    let mut commit = [0; 512];
    for identity in invalid {
        for custom in [
            CommitOptions {
                author: identity,
                ..options
            },
            CommitOptions {
                committer: identity,
                ..options
            },
        ] {
            assert!(matches!(
                Snapshot::from_entries_with_commit(&[], &mut trees, &mut commit, custom),
                Err(Error::InvalidObject)
            ));
            assert_eq!(trees, [0x55; 4]);
        }
    }
    for timezone in [
        "", "+000", "00000", "+00000", "+2400", "-0060", "+ab00", "+0\n00",
    ] {
        let custom = CommitOptions {
            author: CommitIdentity {
                timezone,
                ..defaults
            },
            ..options
        };
        assert!(matches!(
            Snapshot::from_entries_with_commit(&[], &mut trees, &mut commit, custom),
            Err(Error::InvalidObject)
        ));
    }
    assert!(matches!(
        Snapshot::from_entries_with_commit(
            &[],
            &mut trees,
            &mut commit,
            CommitOptions {
                message: "nul\0message",
                ..options
            }
        ),
        Err(Error::InvalidObject)
    ));
}

#[test]
fn git_strictly_accepts_a_builder_snapshot_with_custom_commit() {
    let directory = std::env::temp_dir().join(format!("git-zero-builder-{}", std::process::id()));
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
    let mut slots = [Entry::EMPTY; 5];
    let mut trees = [0; 1024];
    let mut commit = [0; 1024];
    let mut builder = SnapshotBuilder::new(&mut slots, &mut trees, &mut commit);
    for entry in common::ENTRIES.iter().rev() {
        builder.push(*entry).unwrap();
    }
    let author = CommitIdentity {
        name: "Example Author",
        email: "author@example.invalid",
        timestamp: 4_400_000_000,
        timezone: "+0545",
    };
    let committer = CommitIdentity {
        timestamp: 4_400_000_001,
        timezone: "-0230",
        ..CommitIdentity::default()
    };
    let snapshot = builder
        .finish_with_commit(CommitOptions {
            author,
            committer,
            message: "Custom snapshot\n\nExact body",
        })
        .unwrap()
        .with_refs(common::HEAD_REF, common::REFS)
        .unwrap();
    assert!(
        git()
            .args(["init", "-q", "--bare", "--object-format=sha1"])
            .status()
            .unwrap()
            .success()
    );
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    pack::write(&snapshot, &mut output).unwrap();
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
                &snapshot.head().to_string()
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(git().args(["fsck", "--strict"]).status().unwrap().success());
    let contents = git()
        .args(["cat-file", "commit", &snapshot.head().to_string()])
        .output()
        .unwrap();
    assert!(contents.status.success());
    assert_eq!(contents.stdout, snapshot.object(0).unwrap().data);
}
