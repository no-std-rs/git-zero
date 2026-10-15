//! Borrowed file contents and bounded checkout traversal for caller-owned stores.
//!
//! Sources supply hash-verified objects. A mutable source can evict its previous
//! object on every lookup: traversal retains IDs and offsets, never object slices.
use crate::{
    Error, Object, ObjectFormat, ObjectId, ObjectKind, Result,
    object::{Commit, Tag, TreeEntry},
};

/// Canonical Git modes; host permissions and submodule mounting are host policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Directory,
    File,
    Executable,
    Symlink,
    Gitlink,
}

impl Mode {
    pub fn from_raw(mode: u32) -> Result<Self> {
        match mode {
            0o040000 => Ok(Self::Directory),
            0o100644 => Ok(Self::File),
            0o100755 => Ok(Self::Executable),
            0o120000 => Ok(Self::Symlink),
            0o160000 => Ok(Self::Gitlink),
            _ => Err(Error::InvalidObject),
        }
    }

    pub const fn raw(self) -> u32 {
        match self {
            Self::Directory => 0o040000,
            Self::File => 0o100644,
            Self::Executable => 0o100755,
            Self::Symlink => 0o120000,
            Self::Gitlink => 0o160000,
        }
    }

    /// File and symlink contents are exact Git blob bytes, with no text or
    /// newline conversion. Symlink targets retain non-UTF-8 and relative bytes;
    /// embedded NUL is invalid. Empty-target/path-size rules belong to the host.
    pub fn contents(self, object: Object<'_>) -> Result<&[u8]> {
        if !matches!(self, Self::File | Self::Executable | Self::Symlink) {
            return Err(Error::Unsupported);
        }
        if object.kind != ObjectKind::Blob || (self == Self::Symlink && object.data.contains(&0)) {
            return Err(Error::InvalidObject);
        }
        Ok(object.data)
    }
}

/// Owned metadata survives eviction of the parent tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: ObjectId,
    pub mode: Mode,
}

/// Find one byte-string component in a previously validated tree. This checks
/// framing and duplicate matches in one linear pass. Cache insertion should use
/// `object::Tree::parse_with_format` for complete ordering/alias validation.
pub fn lookup(data: &[u8], format: ObjectFormat, name: &[u8]) -> Result<Option<Entry>> {
    if name.is_empty() || matches!(name, b"." | b"..") || name.iter().any(|b| matches!(b, 0 | b'/'))
    {
        return Err(Error::InvalidFileName);
    }
    let mut offset = 0;
    let mut found = None;
    while let Some(entry) = TreeEntry::read_with_format(data, &mut offset, format)? {
        if entry.name == name {
            if found.is_some() {
                return Err(Error::InvalidObject);
            }
            found = Some(Entry {
                id: entry.id,
                mode: Mode::from_raw(entry.mode)?,
            });
        }
    }
    Ok(found)
}

/// An immutable object view is valid until the next mutable source access.
pub trait ObjectSource {
    fn object(&mut self, id: ObjectId) -> Result<Object<'_>>;
}

/// Exact commit-message location within a source object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Message {
    pub id: ObjectId,
    pub offset: usize,
    pub length: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Root {
    pub entry: Entry,
    pub message: Option<Message>,
}

/// Peel at most `max_tags` annotated tags, check each declared target's actual
/// type and format, then resolve a commit's tree (or a direct tree/blob ref).
/// A tag cycle exhausts the explicit bound. No recursion or allocation is used.
pub fn resolve_root(
    source: &mut impl ObjectSource,
    mut id: ObjectId,
    max_tags: u32,
) -> Result<Root> {
    let format = id.format();
    let mut expected = None;
    let mut message = None;
    let mut tags = 0;
    loop {
        let object = source.object(id)?;
        if expected.is_some_and(|kind| kind != object.kind) {
            return Err(Error::InvalidObject);
        }
        match object.kind {
            ObjectKind::Tag => {
                if tags == max_tags {
                    return Err(Error::LimitExceeded);
                }
                let tag = Tag::parse(object.data)?;
                if tag.target.format() != format {
                    return Err(Error::InvalidObjectId);
                }
                id = tag.target;
                expected = Some(tag.target_kind);
                tags += 1;
            }
            ObjectKind::Commit => {
                let commit = Commit::parse_with_format(object.data, format)?;
                message = Some(Message {
                    id,
                    offset: commit.message_offset(),
                    length: commit.message().len(),
                });
                id = commit.tree();
                expected = Some(ObjectKind::Tree);
            }
            ObjectKind::Tree | ObjectKind::Blob => {
                return Ok(Root {
                    entry: Entry {
                        id,
                        mode: if object.kind == ObjectKind::Tree {
                            Mode::Directory
                        } else {
                            Mode::File
                        },
                    },
                    message,
                });
            }
        }
    }
}
