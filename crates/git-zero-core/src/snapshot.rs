use crate::names::{cmp_tree_names, validate_name};
use core::cmp::Ordering;

use crate::{
    Error, Object, ObjectId, ObjectKind, Repository, Result,
    io::{SliceWriter, Write},
    repository::validate_refs,
};

/// Maximum number of tree levels, including the root, to bound recursive stack use.
pub const MAX_TREE_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    /// A single root-level filename. Pass files in ascending byte order.
    pub name: &'a str,
    pub contents: &'a [u8],
}

/// Borrowed contents and mode of a tree entry. Symlink contents are target bytes.
#[derive(Clone, Copy, Debug)]
pub enum EntryKind<'a> {
    File(&'a [u8]),
    Executable(&'a [u8]),
    Symlink(&'a [u8]),
    Tree(&'a [Entry<'a>]),
}

impl EntryKind<'_> {
    fn mode(self) -> &'static [u8] {
        match self {
            Self::File(_) => b"100644",
            Self::Executable(_) => b"100755",
            Self::Symlink(_) => b"120000",
            Self::Tree(_) => b"40000",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    /// A single path component; each child tree has its own sorted entry slice.
    pub name: &'a str,
    pub kind: EntryKind<'a>,
}

impl Entry<'_> {
    /// Initializer for unused builder slots; the empty name is not a valid entry.
    pub const EMPTY: Self = Self {
        name: "",
        kind: EntryKind::File(&[]),
    };

    /// Git tree order: compare bytes with a trailing '/' for trees, NUL otherwise.
    pub fn cmp_name(&self, other: &Self) -> Ordering {
        cmp_tree_names(
            self.name.as_bytes(),
            matches!(self.kind, EntryKind::Tree(_)),
            other.name.as_bytes(),
            matches!(other.kind, EntryKind::Tree(_)),
        )
    }
}

/// One commit identity and its recorded time. The offset does not adjust seconds.
#[derive(Clone, Copy, Debug)]
pub struct CommitIdentity<'a> {
    pub name: &'a str,
    pub email: &'a str,
    /// Unix seconds in the range 0..=i64::MAX, serialized identically on all targets.
    pub timestamp: u64,
    /// A sign and four ASCII digits: hours 00..23, minutes 00..59.
    pub timezone: &'a str,
}

impl Default for CommitIdentity<'_> {
    fn default() -> Self {
        Self {
            name: "git-zero",
            email: "git-zero@example.invalid",
            timestamp: 0,
            timezone: "+0000",
        }
    }
}

/// Metadata for one root commit. Message bytes are preserved, including newlines.
/// Names and emails must be nonempty, with no control bytes or angle brackets;
/// messages may contain newlines but not NUL. Invalid metadata returns InvalidObject.
#[derive(Clone, Copy, Debug)]
pub struct CommitOptions<'a> {
    pub author: CommitIdentity<'a>,
    pub committer: CommitIdentity<'a>,
    pub message: &'a str,
}

impl Default for CommitOptions<'_> {
    fn default() -> Self {
        Self {
            author: CommitIdentity::default(),
            committer: CommitIdentity::default(),
            message: "In-memory snapshot\n",
        }
    }
}

impl CommitOptions<'_> {
    fn validate(self) -> Result<()> {
        for identity in [self.author, self.committer] {
            let timezone = identity.timezone.as_bytes();
            if [identity.name, identity.email].iter().any(|value| {
                value.trim().is_empty()
                    || value
                        .bytes()
                        .any(|byte| byte.is_ascii_control() || matches!(byte, b'<' | b'>'))
            }) || identity.timestamp > i64::MAX as u64
                || timezone.len() != 5
                || !matches!(timezone[0], b'+' | b'-')
                || !timezone[1..].iter().all(u8::is_ascii_digit)
                || (timezone[1] - b'0') * 10 + timezone[2] - b'0' > 23
                || (timezone[3] - b'0') * 10 + timezone[4] - b'0' > 59
            {
                return Err(Error::InvalidObject);
            }
        }
        if self.message.as_bytes().contains(&0) {
            return Err(Error::InvalidObject);
        }
        Ok(())
    }
}

/// Incrementally collect root entries in caller-owned slots, without allocation.
/// Push accepts any arrival order; borrowed subtrees must already be assembled.
/// Nested validation and tree/commit buffer sizing are checked when finishing.
pub struct SnapshotBuilder<'storage, 'data> {
    entries: &'storage mut [Entry<'data>],
    length: usize,
    tree_storage: &'storage mut [u8],
    commit_storage: &'storage mut [u8],
}

impl<'storage, 'data: 'storage> SnapshotBuilder<'storage, 'data> {
    pub fn new(
        entries: &'storage mut [Entry<'data>],
        tree_storage: &'storage mut [u8],
        commit_storage: &'storage mut [u8],
    ) -> Self {
        Self {
            entries,
            length: 0,
            tree_storage,
            commit_storage,
        }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Insert in Git tree order. Invalid names, duplicates (UnsortedFiles), and
    /// exhausted slots (BufferTooSmall) leave every occupied slot unchanged.
    pub fn push(&mut self, entry: Entry<'data>) -> Result<()> {
        validate_name(entry.name.as_bytes())?;
        let occupied = &self.entries[..self.length];
        if occupied.iter().any(|previous| previous.name == entry.name) {
            return Err(Error::UnsortedFiles);
        }
        if self.length == self.entries.len() {
            return Err(Error::BufferTooSmall);
        }
        let position = occupied
            .iter()
            .position(|previous| entry.cmp_name(previous).is_lt())
            .unwrap_or(self.length);
        self.entries
            .copy_within(position..self.length, position + 1);
        self.entries[position] = entry;
        self.length += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<Snapshot<'storage>> {
        self.finish_with_commit(CommitOptions::default())
    }

    /// Consume the builder and serialize metadata into commit_storage. Options
    /// need not outlive the snapshot. Discard partial tree/commit bytes on error.
    pub fn finish_with_commit(self, options: CommitOptions<'_>) -> Result<Snapshot<'storage>> {
        Snapshot::from_entries_with_commit(
            &self.entries[..self.length],
            self.tree_storage,
            self.commit_storage,
            options,
        )
    }
}

#[derive(Clone, Copy)]
enum Input<'a> {
    Files(&'a [File<'a>]),
    Entries(&'a [Entry<'a>]),
}

impl<'a> Input<'a> {
    fn len(self) -> usize {
        match self {
            Self::Files(files) => files.len(),
            Self::Entries(entries) => entries.len(),
        }
    }

    fn entries(self) -> impl Iterator<Item = Entry<'a>> {
        (0..self.len()).map(move |index| match self {
            Self::Files(files) => Entry {
                name: files[index].name,
                kind: EntryKind::File(files[index].contents),
            },
            Self::Entries(entries) => entries[index],
        })
    }

    fn blob_at(self, index: &mut usize) -> Option<&'a [u8]> {
        if let Self::Files(files) = self {
            return files.get(*index).map(|file| file.contents);
        }
        for entry in self.entries() {
            match entry.kind {
                EntryKind::Tree(children) => {
                    if let Some(blob) = Self::Entries(children).blob_at(index) {
                        return Some(blob);
                    }
                }
                EntryKind::File(data) | EntryKind::Executable(data) | EntryKind::Symlink(data) => {
                    if *index == 0 {
                        return Some(data);
                    }
                    *index -= 1;
                }
            }
        }
        None
    }
}

struct Blobs<'a> {
    input: Input<'a>,
    index: usize,
}

impl<'a> Iterator for Blobs<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let mut index = self.index;
        let result = self.input.blob_at(&mut index)?;
        self.index += 1;
        Some(result)
    }
}

/// Internal storage is one raw root tree for new, or length-prefixed trees in
/// preorder for from_entries. Prefixes never enter Git objects or their hashes.
struct Trees<'a> {
    bytes: &'a [u8],
    framed: bool,
    done: bool,
}

impl<'a> Iterator for Trees<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if !self.framed {
            self.done = true;
            return Some(self.bytes);
        }
        if self.bytes.is_empty() {
            return None;
        }
        let length = u32::from_le_bytes(self.bytes[..4].try_into().ok()?) as usize;
        let (tree, rest) = self.bytes[4..].split_at(length);
        self.bytes = rest;
        Some(tree)
    }
}

/// A tree hierarchy and one deterministic commit with no parents.
/// All data and storage are borrowed; construction never allocates.
pub struct Snapshot<'a> {
    input: Input<'a>,
    trees: &'a [u8],
    framed: bool,
    commit: &'a [u8],
    head: ObjectId,
    head_ref: &'a str,
    refs: &'a [&'a str],
}

impl<'a> Snapshot<'a> {
    fn blobs(&self) -> Blobs<'a> {
        Blobs {
            input: self.input,
            index: 0,
        }
    }

    fn trees(&self) -> Trees<'a> {
        Trees {
            bytes: self.trees,
            framed: self.framed,
            done: false,
        }
    }

    fn unique_blobs(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.blobs().enumerate().filter_map(|(index, data)| {
            (!self.blobs().take(index).any(|previous| previous == data)).then_some(data)
        })
    }

    fn unique_trees(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.trees().enumerate().filter_map(|(index, data)| {
            (!self.trees().take(index).any(|previous| previous == data)).then_some(data)
        })
    }

    /// Build a flat snapshot of regular files. Existing buffer sizing and object
    /// IDs are preserved; this constructor needs only the raw root tree's bytes.
    pub fn new(
        files: &'a [File<'a>],
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
    ) -> Result<Self> {
        Self::new_with_commit(
            files,
            tree_storage,
            commit_storage,
            CommitOptions::default(),
        )
    }

    /// Build a flat snapshot with custom root-commit metadata, using raw tree storage.
    pub fn new_with_commit(
        files: &'a [File<'a>],
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
        options: CommitOptions<'_>,
    ) -> Result<Self> {
        Self::build(
            Input::Files(files),
            tree_storage,
            commit_storage,
            false,
            options,
        )
    }

    /// Build nested trees with regular/executable files, symlinks, and empty trees.
    /// Entries at every level must be sorted using Entry::cmp_name, with no
    /// duplicate names. Storage needs the sum of every raw tree's size plus four
    /// bytes per tree, including duplicate/empty trees. Depth is limited by
    /// MAX_TREE_DEPTH; excess depth returns Unsupported. Discard partial
    /// storage after any error. Git checkout does not materialize empty trees.
    pub fn from_entries(
        entries: &'a [Entry<'a>],
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
    ) -> Result<Self> {
        Self::from_entries_with_commit(
            entries,
            tree_storage,
            commit_storage,
            CommitOptions::default(),
        )
    }

    /// Build nested trees with custom root-commit metadata. Tree storage and
    /// validation follow from_entries; options are copied into commit_storage.
    pub fn from_entries_with_commit(
        entries: &'a [Entry<'a>],
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
        options: CommitOptions<'_>,
    ) -> Result<Self> {
        Self::build(
            Input::Entries(entries),
            tree_storage,
            commit_storage,
            true,
            options,
        )
    }

    fn build(
        input: Input<'a>,
        tree_storage: &'a mut [u8],
        commit_storage: &'a mut [u8],
        framed: bool,
        options: CommitOptions<'_>,
    ) -> Result<Self> {
        options.validate()?;
        let length = write_tree(input, tree_storage, 0, framed)?;
        let trees = &tree_storage[..length];
        let root = Trees {
            bytes: trees,
            framed,
            done: false,
        }
        .next()
        .ok_or(Error::ObjectNotFound)?;
        let tree_id = Object {
            kind: ObjectKind::Tree,
            data: root,
        }
        .id();
        let mut commit = SliceWriter::new(commit_storage);
        commit.write_all(b"tree ")?;
        commit.write_all(&tree_id.to_hex())?;
        commit.write_all(b"\n")?;
        write_identity(&mut commit, b"author", options.author)?;
        write_identity(&mut commit, b"committer", options.committer)?;
        commit.write_all(b"\n")?;
        commit.write_all(options.message.as_bytes())?;
        let commit = commit.finish();
        let head = Object {
            kind: ObjectKind::Commit,
            data: commit,
        }
        .id();
        Ok(Self {
            input,
            trees,
            framed,
            commit,
            head,
            head_ref: "refs/heads/main",
            refs: &["refs/heads/main"],
        })
    }

    /// Configure the default branch and ref aliases for this snapshot's commit.
    /// Names must be fully qualified, byte-sorted, unique, and free of path
    /// conflicts (e.g. refs/heads/a and refs/heads/a/b). Include head_ref in
    /// the slice. Ref configuration does not change any object ID or cache key.
    pub fn with_refs(mut self, head_ref: &'a str, refs: &'a [&'a str]) -> Result<Self> {
        self.head_ref = head_ref;
        self.refs = refs;
        validate_refs(&self)?;
        Ok(self)
    }
}

fn write_identity(
    output: &mut SliceWriter<'_>,
    header: &[u8],
    identity: CommitIdentity<'_>,
) -> Result<()> {
    use core::fmt::Write as _;
    output.write_all(header)?;
    output.write_all(b" ")?;
    output.write_all(identity.name.as_bytes())?;
    output.write_all(b" <")?;
    output.write_all(identity.email.as_bytes())?;
    writeln!(output, "> {} {}", identity.timestamp, identity.timezone)
        .map_err(|_| Error::BufferTooSmall)
}

fn write_tree(input: Input<'_>, storage: &mut [u8], depth: usize, framed: bool) -> Result<usize> {
    if depth >= MAX_TREE_DEPTH {
        return Err(Error::Unsupported);
    }
    let mut length = 0_usize;
    let mut previous: Option<Entry<'_>> = None;
    for (index, entry) in input.entries().enumerate() {
        validate_name(entry.name.as_bytes())?;
        if previous.is_some_and(|previous| previous.cmp_name(&entry) != Ordering::Less)
            || input
                .entries()
                .take(index)
                .any(|previous| previous.name == entry.name)
        {
            return Err(Error::UnsortedFiles);
        }
        previous = Some(entry);
        length = length
            .checked_add(entry.kind.mode().len())
            .and_then(|length| length.checked_add(entry.name.len()))
            .and_then(|length| length.checked_add(22))
            .ok_or(Error::BufferTooSmall)?;
    }
    let prefix = if framed { 4 } else { 0 };
    let record_length = length.checked_add(prefix).ok_or(Error::BufferTooSmall)?;
    if record_length > storage.len() {
        return Err(Error::BufferTooSmall);
    }
    let (record, children_storage) = storage.split_at_mut(record_length);
    if framed {
        record[..4].copy_from_slice(
            &u32::try_from(length)
                .map_err(|_| Error::BufferTooSmall)?
                .to_le_bytes(),
        );
    }
    let mut tree = SliceWriter::new(&mut record[prefix..]);
    let mut children_length = 0;
    for entry in input.entries() {
        let object = match entry.kind {
            EntryKind::Tree(children) => {
                let storage = &mut children_storage[children_length..];
                let length = write_tree(Input::Entries(children), storage, depth + 1, true)?;
                children_length += length;
                Object {
                    kind: ObjectKind::Tree,
                    data: Trees {
                        bytes: &storage[..length],
                        framed: true,
                        done: false,
                    }
                    .next()
                    .ok_or(Error::ObjectNotFound)?,
                }
            }
            EntryKind::File(data) | EntryKind::Executable(data) | EntryKind::Symlink(data) => {
                Object {
                    kind: ObjectKind::Blob,
                    data,
                }
            }
        };
        tree.write_all(entry.kind.mode())?;
        tree.write_all(b" ")?;
        tree.write_all(entry.name.as_bytes())?;
        tree.write_all(&[0])?;
        tree.write_all(&object.id().0)?;
    }
    Ok(record_length + children_length)
}

impl Repository for Snapshot<'_> {
    fn head(&self) -> ObjectId {
        self.head
    }

    fn head_ref(&self) -> &str {
        self.head_ref
    }

    fn reference_count(&self) -> usize {
        self.refs.len()
    }

    fn reference_name(&self, index: usize) -> Option<&str> {
        self.refs.get(index).copied()
    }

    fn object_count(&self) -> usize {
        1 + self.unique_trees().count() + self.unique_blobs().count()
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        if index == 0 {
            return Some(Object {
                kind: ObjectKind::Commit,
                data: self.commit,
            });
        }
        if let Some(data) = self.unique_trees().nth(index - 1) {
            return Some(Object {
                kind: ObjectKind::Tree,
                data,
            });
        }
        self.unique_blobs()
            .nth(index - 1 - self.unique_trees().count())
            .map(|data| Object {
                kind: ObjectKind::Blob,
                data,
            })
    }
}
