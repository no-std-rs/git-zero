//! Borrowed structural views of commit, tree and annotated-tag bodies.
use crate::{Error, ObjectId, ObjectKind, Result, snapshot::cmp_tree_names};

/// A commit's root tree and exact message bytes. This checks the root header
/// and header/message separator, not identity semantics or graph reachability.
#[derive(Clone, Copy, Debug)]
pub struct Commit<'a> {
    tree: ObjectId,
    message: &'a [u8],
    message_offset: usize,
}

impl<'a> Commit<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let tree = Self::root_tree(data)?;
        let separator = data
            .windows(2)
            .position(|bytes| bytes == b"\n\n")
            .ok_or(Error::InvalidObject)?;
        Ok(Self {
            tree,
            message: &data[separator + 2..],
            message_offset: separator + 2,
        })
    }

    /// Read just the first, canonical root-tree header.
    pub fn root_tree(data: &[u8]) -> Result<ObjectId> {
        let line = data.get(..46).ok_or(Error::InvalidObject)?;
        if !line.starts_with(b"tree ") || line[45] != b'\n' {
            return Err(Error::InvalidObject);
        }
        ObjectId::from_hex(&line[5..45])
    }

    pub fn tree(&self) -> ObjectId {
        self.tree
    }
    pub fn message(&self) -> &'a [u8] {
        self.message
    }
    pub fn message_offset(&self) -> usize {
        self.message_offset
    }
}

/// One Git tree entry. Names are byte strings; filesystem-specific name limits
/// and policies belong to the host. Modes retain Git's original octal values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeEntry<'a> {
    pub name: &'a [u8],
    pub mode: u32,
    pub id: ObjectId,
}

impl<'a> TreeEntry<'a> {
    /// Read at a byte cursor, leaving it unchanged on error. This validates one
    /// entry's mode, name and framing; use Tree::parse to check the whole order.
    pub fn read(data: &'a [u8], offset: &mut usize) -> Result<Option<Self>> {
        let tail = data.get(*offset..).ok_or(Error::InvalidObject)?;
        if tail.is_empty() {
            return Ok(None);
        }
        let space = tail
            .iter()
            .position(|&b| b == b' ')
            .ok_or(Error::InvalidObject)?;
        let mode = match &tail[..space] {
            b"40000" => 0o040000,
            b"100644" => 0o100644,
            b"100755" => 0o100755,
            b"120000" => 0o120000,
            b"160000" => 0o160000,
            _ => return Err(Error::InvalidObject),
        };
        let rest = &tail[space + 1..];
        let nul = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::InvalidObject)?;
        let name = &rest[..nul];
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(Error::InvalidFileName);
        }
        let id = ObjectId(
            rest.get(nul + 1..nul + 21)
                .ok_or(Error::InvalidObject)?
                .try_into()
                .map_err(|_| Error::InvalidObject)?,
        );
        *offset += space + 1 + nul + 21;
        Ok(Some(Self { name, mode, id }))
    }
}

/// A structurally valid tree, including Git ordering and unique byte names.
/// Validation uses constant storage. File/directory aliases can be separated
/// by other names in Git order, so checking them may reread earlier entries.
#[derive(Clone, Copy, Debug)]
pub struct Tree<'a> {
    data: &'a [u8],
}

impl<'a> Tree<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut offset = 0;
        let mut previous: Option<TreeEntry<'_>> = None;
        loop {
            let start = offset;
            let Some(entry) = TreeEntry::read(data, &mut offset)? else {
                break;
            };
            if let Some(previous) = previous {
                if cmp_tree_names(
                    previous.name,
                    previous.mode == 0o040000,
                    entry.name,
                    entry.mode == 0o040000,
                ) != core::cmp::Ordering::Less
                {
                    return Err(Error::UnsortedFiles);
                }
            }
            if entry.mode == 0o040000 {
                let mut earlier = 0;
                while earlier < start {
                    if TreeEntry::read(data, &mut earlier)?
                        .is_some_and(|other| other.name == entry.name)
                    {
                        return Err(Error::UnsortedFiles);
                    }
                }
            }
            previous = Some(entry);
        }
        Ok(Self { data })
    }

    pub fn entries(&self) -> TreeEntries<'a> {
        TreeEntries {
            data: self.data,
            offset: 0,
        }
    }
}

pub struct TreeEntries<'a> {
    data: &'a [u8],
    offset: usize,
}
impl<'a> Iterator for TreeEntries<'a> {
    type Item = TreeEntry<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        // Only a fully validated, immutable Tree can construct this iterator.
        TreeEntry::read(self.data, &mut self.offset).expect("validated tree")
    }
}

/// The target and declared target kind of an annotated tag. Graph validation
/// must additionally check that the actual target exists and has this kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tag {
    pub target: ObjectId,
    pub target_kind: ObjectKind,
}

impl Tag {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut lines = data.split(|&byte| byte == b'\n');
        let object = lines
            .next()
            .and_then(|line| line.strip_prefix(b"object "))
            .ok_or(Error::InvalidObject)?;
        let target = ObjectId::from_hex(object)?;
        let target_kind = match lines.next() {
            Some(b"type commit") => ObjectKind::Commit,
            Some(b"type tree") => ObjectKind::Tree,
            Some(b"type blob") => ObjectKind::Blob,
            Some(b"type tag") => ObjectKind::Tag,
            _ => return Err(Error::InvalidObject),
        };
        let name = lines
            .next()
            .and_then(|line| line.strip_prefix(b"tag "))
            .ok_or(Error::InvalidObject)?;
        if name.is_empty() || !lines.any(|line| line.is_empty()) {
            return Err(Error::InvalidObject);
        }
        Ok(Self {
            target,
            target_kind,
        })
    }
}
