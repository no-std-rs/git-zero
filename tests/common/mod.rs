use git_zero::{Entry, EntryKind};

pub const CHILDREN: &[Entry<'static>] = &[
    Entry {
        name: "run",
        kind: EntryKind::Executable(b"#!/bin/sh\nprintf 'snapshot\\n'\n"),
    },
    Entry {
        name: "target",
        kind: EntryKind::Symlink(b"run"),
    },
];

pub const ENTRIES: &[Entry<'static>] = &[
    Entry {
        name: "a.c",
        kind: EntryKind::File(b"#!/bin/sh\nprintf 'snapshot\\n'\n"),
    },
    Entry {
        name: "a",
        kind: EntryKind::Tree(CHILDREN),
    },
    Entry {
        name: "b",
        kind: EntryKind::Tree(CHILDREN),
    },
    Entry {
        name: "empty-again",
        kind: EntryKind::Tree(&[]),
    },
    Entry {
        name: "empty",
        kind: EntryKind::Tree(&[]),
    },
];

pub const HEAD_REF: &str = "refs/heads/releases/current";
pub const REFS: &[&str] = &["refs/heads/alternate", HEAD_REF, "refs/tags/snapshot-v1"];
