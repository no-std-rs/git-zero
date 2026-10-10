use crate::{
    Error, Object, ObjectId, ObjectKind, Repository, Result,
    io::{SliceWriter, Write},
};

#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    /// A single root-level filename. Pass files in ascending byte order.
    pub name: &'a str,
    pub contents: &'a [u8],
}

/// A root tree and one deterministic commit with no parents.
pub struct Snapshot<'a> {
    files: &'a [File<'a>],
    tree: &'a [u8],
    commit: &'a [u8],
    head: ObjectId,
}

impl<'a> Snapshot<'a> {
    fn unique_files(&self) -> impl Iterator<Item = &File<'a>> {
        self.files.iter().enumerate().filter_map(|(index, file)| {
            (!self.files[..index]
                .iter()
                .any(|previous| previous.contents == file.contents))
            .then_some(file)
        })
    }

    pub fn new(
        files: &'a [File<'a>],
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
    ) -> Result<Self> {
        let mut tree = SliceWriter::new(tree_storage);
        let mut previous: Option<&str> = None;
        for file in files {
            if file.name.is_empty()
                || matches!(file.name, "." | "..")
                || file.name.eq_ignore_ascii_case(".git")
                || file
                    .name
                    .bytes()
                    .any(|byte| matches!(byte, 0 | b'/' | b'\\'))
            {
                return Err(Error::InvalidFileName);
            }
            if previous.is_some_and(|name| name >= file.name) {
                return Err(Error::UnsortedFiles);
            }
            previous = Some(file.name);
            tree.write_all(b"100644 ")?;
            tree.write_all(file.name.as_bytes())?;
            tree.write_all(&[0])?;
            tree.write_all(
                &Object {
                    kind: ObjectKind::Blob,
                    data: file.contents,
                }
                .id()
                .0,
            )?;
        }
        let tree = tree.finish();
        let tree_id = Object {
            kind: ObjectKind::Tree,
            data: tree,
        }
        .id();
        let mut commit = SliceWriter::new(commit_storage);
        commit.write_all(b"tree ")?;
        commit.write_all(&tree_id.to_hex())?;
        commit.write_all(b"\nauthor git-zero <git-zero@example.invalid> 0 +0000\ncommitter git-zero <git-zero@example.invalid> 0 +0000\n\nIn-memory snapshot\n")?;
        let commit = commit.finish();
        let head = Object {
            kind: ObjectKind::Commit,
            data: commit,
        }
        .id();
        Ok(Self {
            files,
            tree,
            commit,
            head,
        })
    }
}

impl Repository for Snapshot<'_> {
    fn head(&self) -> ObjectId {
        self.head
    }

    fn object_count(&self) -> usize {
        self.unique_files().count() + 2
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        match index {
            0 => Some(Object {
                kind: ObjectKind::Commit,
                data: self.commit,
            }),
            1 => Some(Object {
                kind: ObjectKind::Tree,
                data: self.tree,
            }),
            _ => self.unique_files().nth(index - 2).map(|file| Object {
                kind: ObjectKind::Blob,
                data: file.contents,
            }),
        }
    }
}
