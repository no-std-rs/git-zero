//! Complete, caller-owned pack materialization with constant-stack indexing.
use super::{DeltaLimits, EntryKind, Reader, apply_delta};
use crate::{
    Error, Object, ObjectId, ObjectKind, Result,
    object::Tree,
    zlib::{InflateLimits, WINDOW_SIZE},
};

/// Storage for one imported object. Initialize with Default; only materialize
/// populates entries. The C representation lets adapters allocate this storage
/// without introducing an allocator into the core.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MaterializedEntry {
    id: [u8; 20],
    kind: u32,
    pack_offset: u64,
    offset: usize,
    length: usize,
    depth: u32,
}

impl MaterializedEntry {
    pub fn id(&self) -> ObjectId {
        ObjectId::Sha1(self.id)
    }
    pub fn kind(&self) -> Result<ObjectKind> {
        match self.kind {
            1 => Ok(ObjectKind::Commit),
            2 => Ok(ObjectKind::Tree),
            3 => Ok(ObjectKind::Blob),
            4 => Ok(ObjectKind::Tag),
            _ => Err(Error::InvalidObject),
        }
    }
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn length(&self) -> usize {
        self.length
    }
    pub fn delta_depth(&self) -> u32 {
        self.depth
    }
    pub fn pack_offset(&self) -> u64 {
        self.pack_offset
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MaterializeLimits {
    pub inflate: InflateLimits,
    pub delta: DeltaLimits,
    pub max_passes: u32,
    /// Counts every inflated entry on each pass, even already-resolved entries.
    pub max_decoded_bytes_per_pass: u64,
    /// Maximum index slots inspected for delta bases across all passes.
    pub max_base_probes: u64,
    /// Encoded tree bytes read, including alias rescans, across the whole import.
    pub max_tree_validation_bytes: u64,
}

/// An immutable, ID-sorted index and materialized bodies. The input pack and
/// decoding scratch are not retained. This validates objects and tree structure,
/// not graph closure, refs or commit/tag header semantics.
pub struct MaterializedPack<'a> {
    index: &'a [MaterializedEntry],
    arena: &'a [u8],
}

impl<'a> MaterializedPack<'a> {
    pub fn entries(&self) -> &'a [MaterializedEntry] {
        self.index
    }
    pub fn bytes_used(&self) -> usize {
        self.arena.len()
    }
    pub fn object(&self, position: usize) -> Option<Object<'a>> {
        let entry = self.index.get(position)?;
        Some(Object {
            kind: entry.kind().ok()?,
            data: self
                .arena
                .get(entry.offset..entry.offset.checked_add(entry.length)?)?,
        })
    }
    pub fn find(&self, id: ObjectId) -> Option<Object<'a>> {
        let entry = find_materialized(self.index, id)?;
        Some(Object {
            kind: entry.kind().ok()?,
            data: self
                .arena
                .get(entry.offset..entry.offset.checked_add(entry.length)?)?,
        })
    }
}

/// Find metadata in the sorted index produced by materialize. Adapters retaining
/// the raw index must keep it immutable. Default/unpopulated entries are omitted.
pub fn find_materialized(index: &[MaterializedEntry], id: ObjectId) -> Option<&MaterializedEntry> {
    let ObjectId::Sha1(id) = id else {
        return None;
    };
    let position = index.binary_search_by_key(&id, |entry| entry.id).ok()?;
    let entry = &index[position];
    (entry.kind != 0).then_some(entry)
}

/// Import a checksummed, non-thin SHA-1 pack into caller-owned index/body storage.
/// Index needs object_count slots; excess slots remain untouched. Each pass
/// resets the reader and can resolve backward OFS or forward REF deltas against
/// earlier materialized objects. Depth, instruction, inflation, pass and work
/// limits are explicit; missing/external bases fail rather than doing I/O.
///
/// The ID index uses iterative heap sort, with constant stack space. Resolution
/// charges every metadata probe and tree-validation read against explicit work
/// budgets, and may reinflate entries across passes. Discard partial storage on
/// error; a retry reinitializes the index. No allocation, recursion or platform
/// I/O is used.
///
/// ```
/// # #[cfg(feature = "write")]
/// # fn example() -> git_zero_core::Result<()> {
/// use git_zero_core::{Snapshot, ObjectKind, object::Commit, io::SliceWriter, pack,
///     zlib::{InflateLimits, WINDOW_SIZE}};
/// let mut tree = [0; 256];
/// let mut commit = [0; 512];
/// let source = Snapshot::new(&[], &mut tree, &mut commit)?;
/// let mut wire = [0; 2048];
/// let mut output = SliceWriter::new(&mut wire);
/// pack::write(&source, &mut output)?;
/// let mut index = [pack::MaterializedEntry::default(); 2];
/// let mut arena = [0; 1024];
/// let mut scratch = [0; 512];
/// let mut window = [0; WINDOW_SIZE];
/// let limits = pack::MaterializeLimits {
///     inflate: InflateLimits { max_output: 512, max_ratio: 1024, ratio_slack: 64 },
///     delta: pack::DeltaLimits { max_output: 512, max_instructions: 512, max_depth: 64 },
///     max_passes: 65,
///     max_decoded_bytes_per_pass: 1024,
///     max_base_probes: 4096,
///     max_tree_validation_bytes: 4096,
/// };
/// let objects = pack::materialize(output.as_bytes(), &mut index, &mut arena,
///     &mut scratch, &mut window, limits)?;
/// let head = objects.entries().iter().find(|entry| entry.kind() == Ok(ObjectKind::Commit)).unwrap();
/// let commit = Commit::parse(objects.find(head.id()).unwrap().data)?;
/// assert_eq!(objects.find(commit.tree()).unwrap().kind, ObjectKind::Tree);
/// # Ok(()) }
/// # #[cfg(feature = "write")]
/// # example().unwrap();
/// ```
pub fn materialize<'a>(
    pack: &[u8],
    index: &'a mut [MaterializedEntry],
    arena: &'a mut [u8],
    scratch: &mut [u8],
    window: &mut [u8; WINDOW_SIZE],
    limits: MaterializeLimits,
) -> Result<MaterializedPack<'a>> {
    let count = Reader::new(pack)?.object_count() as usize;
    let index = index.get_mut(..count).ok_or(Error::BufferTooSmall)?;
    index.fill(MaterializedEntry::default());
    if count == 0 {
        return Ok(MaterializedPack {
            index,
            arena: &arena[..0],
        });
    }
    let mut used = 0;
    let mut remaining = count;
    let mut base_probes = limits.max_base_probes;
    let mut tree_bytes = limits.max_tree_validation_bytes;
    for _ in 0..limits.max_passes {
        let mut reader = Reader::new(pack)?;
        let mut progress = false;
        let mut decoded_bytes = 0_u64;
        for i in 0..count {
            decoded_bytes =
                decoded_bytes.saturating_add(reader.next_entry_size()?.ok_or(Error::CorruptPack)?);
            if decoded_bytes > limits.max_decoded_bytes_per_pass {
                return Err(Error::LimitExceeded);
            }
            let entry = reader
                .read_entry(scratch, window, limits.inflate)?
                .ok_or(Error::CorruptPack)?;
            if index[i].kind != 0 {
                continue;
            }
            let mut base = None;
            if !matches!(entry.kind(), EntryKind::Object(_)) && remaining != count {
                for candidate in index.iter() {
                    base_probes = base_probes.checked_sub(1).ok_or(Error::LimitExceeded)?;
                    let matches = match entry.kind() {
                        EntryKind::OffsetDelta { base_offset } => {
                            candidate.pack_offset == base_offset
                        }
                        EntryKind::ReferenceDelta { base_id } => {
                            candidate.id.as_slice() == base_id.as_bytes()
                        }
                        EntryKind::Object(_) => false,
                    };
                    if candidate.kind != 0 && matches {
                        base = Some(*candidate);
                        break;
                    }
                }
            }
            if !matches!(entry.kind(), EntryKind::Object(_)) && base.is_none() {
                continue;
            }
            let (previous, output) = arena.split_at_mut(used);
            let (object, depth) = if let Some(base) = base {
                if base.depth >= limits.delta.max_depth {
                    return Err(Error::LimitExceeded);
                }
                let end = base
                    .offset
                    .checked_add(base.length)
                    .ok_or(Error::CorruptPack)?;
                let body = previous.get(base.offset..end).ok_or(Error::CorruptPack)?;
                (
                    apply_delta(
                        Object {
                            kind: base.kind()?,
                            data: body,
                        },
                        entry.data(),
                        output,
                        limits.delta,
                    )?,
                    base.depth + 1,
                )
            } else {
                let EntryKind::Object(kind) = entry.kind() else {
                    return Err(Error::CorruptPack);
                };
                let output = output
                    .get_mut(..entry.data().len())
                    .ok_or(Error::BufferTooSmall)?;
                output.copy_from_slice(entry.data());
                (Object { kind, data: output }, 0)
            };
            if object.kind == ObjectKind::Tree {
                Tree::parse_with_budget(object.data, &mut tree_bytes)?;
            }
            let ObjectId::Sha1(id) = object.id() else {
                return Err(Error::Unsupported);
            };
            index[i] = MaterializedEntry {
                id,
                kind: object.kind as u32,
                pack_offset: entry.offset(),
                offset: used,
                length: object.data.len(),
                depth,
            };
            used += object.data.len();
            remaining -= 1;
            progress = true;
        }
        reader.finish()?;
        if remaining == 0 {
            sort_index(index);
            return Ok(MaterializedPack {
                index,
                arena: &arena[..used],
            });
        }
        if !progress {
            return Err(Error::CorruptPack);
        }
    }
    Err(Error::LimitExceeded)
}

fn sort_index(index: &mut [MaterializedEntry]) {
    fn sift(index: &mut [MaterializedEntry], mut root: usize, length: usize) {
        loop {
            let left = root * 2 + 1;
            if left >= length {
                return;
            }
            let larger = if left + 1 < length && index[left].id < index[left + 1].id {
                left + 1
            } else {
                left
            };
            if index[root].id >= index[larger].id {
                return;
            }
            index.swap(root, larger);
            root = larger;
        }
    }
    for root in (0..index.len() / 2).rev() {
        sift(index, root, index.len());
    }
    for end in (1..index.len()).rev() {
        index.swap(0, end);
        sift(index, 0, end);
    }
}
