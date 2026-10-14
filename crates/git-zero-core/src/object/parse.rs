//! Borrowed structural views of commit, tree and annotated-tag bodies.
use crate::{Error, ObjectFormat, ObjectId, ObjectKind, Result, names::cmp_tree_names};

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
        Self::parse_with_format(data, ObjectFormat::Sha1)
    }
    pub fn parse_with_format(data: &'a [u8], format: ObjectFormat) -> Result<Self> {
        let tree = Self::root_tree_with_format(data, format)?;
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
        Self::root_tree_with_format(data, ObjectFormat::Sha1)
    }
    pub fn root_tree_with_format(data: &[u8], format: ObjectFormat) -> Result<ObjectId> {
        let end = 5 + format.hex_length();
        let line = data.get(..end + 1).ok_or(Error::InvalidObject)?;
        if !line.starts_with(b"tree ") || line[end] != b'\n' {
            return Err(Error::InvalidObject);
        }
        ObjectId::from_hex(&line[5..end])
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
        Self::read_with_format(data, offset, ObjectFormat::Sha1)
    }
    pub fn read_with_format(
        data: &'a [u8],
        offset: &mut usize,
        format: ObjectFormat,
    ) -> Result<Option<Self>> {
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
        let id_length = format.id_length();
        let id = ObjectId::from_bytes(
            format,
            rest.get(nul + 1..nul + 1 + id_length)
                .ok_or(Error::InvalidObject)?,
        );
        let id = id?;
        *offset += space + 1 + nul + 1 + id_length;
        Ok(Some(Self { name, mode, id }))
    }
}

/// A structurally valid tree, including Git ordering and unique byte names.
/// Validation uses constant storage. File/directory aliases can be separated
/// by other names in Git order, so checking them may reread earlier entries.
/// Every read, including these rescans, consumes an encoded-byte work budget.
#[derive(Clone, Copy, Debug)]
pub struct Tree<'a> {
    data: &'a [u8],
    format: ObjectFormat,
}

impl<'a> Tree<'a> {
    /// Validate using a 64 MiB encoded-byte work budget. Hosts importing multiple
    /// trees can share a different budget through parse_with_budget.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Self::parse_with_format(data, ObjectFormat::Sha1)
    }
    pub fn parse_with_format(data: &'a [u8], format: ObjectFormat) -> Result<Self> {
        Self::parse_with_budget_and_format(data, &mut (64 * 1024 * 1024), format)
    }

    /// Charge encoded bytes read, including duplicate-name rescans, against a
    /// caller-owned budget. LimitExceeded stops before reading beyond it. Budget
    /// already spent is retained on error; the tree remains borrowed/immutable.
    pub fn parse_with_budget(data: &'a [u8], budget: &mut u64) -> Result<Self> {
        Self::parse_with_budget_and_format(data, budget, ObjectFormat::Sha1)
    }
    pub fn parse_with_budget_and_format(
        data: &'a [u8],
        budget: &mut u64,
        format: ObjectFormat,
    ) -> Result<Self> {
        let mut offset = 0;
        let mut previous: Option<TreeEntry<'_>> = None;
        let mut last_file_end = 0;
        while let Some(entry) = read_budgeted(data, &mut offset, budget, format)? {
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
                // Directories cannot alias each other after the order check.
                // Only the prefix containing earlier files needs a rescan.
                while earlier < last_file_end {
                    if read_budgeted(data, &mut earlier, budget, format)?
                        .is_some_and(|other| other.name == entry.name)
                    {
                        return Err(Error::UnsortedFiles);
                    }
                }
            } else {
                last_file_end = offset;
            }
            previous = Some(entry);
        }
        Ok(Self { data, format })
    }

    pub fn entries(&self) -> TreeEntries<'a> {
        TreeEntries {
            data: self.data,
            offset: 0,
            format: self.format,
        }
    }
}

fn read_budgeted<'a>(
    data: &'a [u8],
    offset: &mut usize,
    budget: &mut u64,
    format: ObjectFormat,
) -> Result<Option<TreeEntry<'a>>> {
    if *offset == data.len() {
        return Ok(None);
    }
    if *budget == 0 {
        return Err(Error::LimitExceeded);
    }
    let start = *offset;
    let available = usize::try_from(*budget).unwrap_or(usize::MAX);
    let end = start.saturating_add(available).min(data.len());
    let result = TreeEntry::read_with_format(&data[..end], offset, format);
    if result.is_err() && end < data.len() {
        return Err(Error::LimitExceeded);
    }
    *budget -= (*offset - start) as u64;
    result
}

pub struct TreeEntries<'a> {
    data: &'a [u8],
    offset: usize,
    format: ObjectFormat,
}
impl<'a> Iterator for TreeEntries<'a> {
    type Item = TreeEntry<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        // Only a fully validated, immutable Tree can construct this iterator.
        TreeEntry::read_with_format(self.data, &mut self.offset, self.format)
            .expect("validated tree")
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
