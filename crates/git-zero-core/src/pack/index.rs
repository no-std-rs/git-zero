//! Version-2 pack indexes built from caller-owned entry metadata.
use super::{IndexEntry, LooseEncoder, crc::Crc32, encode};
use crate::{Error, ObjectId, Repository, Result, io::Write, object::Hash};

/// Stream a stored-block pack and a version-2 index without buffering either.
/// `order` selects unique object indices or `None` selects the whole repository.
/// Metadata needs one slot per selected object; extra slots remain untouched.
/// Scratch must be nonempty for nonempty selections. The resulting pack matches
/// `write_selected_with_scratch`/`write_with_scratch` for every scratch size.
///
/// IDs, selection and metadata capacity are checked before output. The repository
/// must satisfy its stable, accurate-ID contract (or be validated beforehand).
/// Preflight IDs are retained, avoiding repeated provider hashing. Entries are
/// collected while encoding each body once, then sorted in place
/// without allocation in O(n log n) time. Index bytes follow after the pack's
/// trailer is written. On any error discard both outputs and used metadata;
/// publish them together only after success. Returns the pack checksum.
pub fn write_indexed_with_scratch(
    repository: &impl Repository,
    order: Option<&[usize]>,
    entries: &mut [IndexEntry],
    scratch: &mut [u8],
    pack_output: &mut impl Write,
    index_output: &mut impl Write,
) -> Result<ObjectId> {
    let entries = prepare_entries(repository, order, entries, scratch)?;
    let mut output = PackWriter {
        output: pack_output,
        hash: repository.object_format().hash(),
        offset: 0,
        crc: Crc32::new(),
    };
    output.write_all(b"PACK")?;
    output.write_all(&2_u32.to_be_bytes())?;
    output.write_all(&(entries.len() as u32).to_be_bytes())?;
    for (position, entry) in entries.iter_mut().enumerate() {
        let index = order.map_or(position, |order| order[position]);
        *entry = output.write_entry(repository, index, entry.id, scratch)?;
    }
    let checksum = output.hash.finish();
    output.output.write_all(checksum.as_bytes())?;
    entries.sort_unstable_by_key(|entry| entry.id);
    write_index(entries, checksum, index_output)?;
    Ok(checksum)
}

fn prepare_entries<'entries>(
    repository: &impl Repository,
    order: Option<&[usize]>,
    entries: &'entries mut [IndexEntry],
    scratch: &[u8],
) -> Result<&'entries mut [IndexEntry]> {
    if let Some(order) = order {
        encode::validate_order(repository, order)?;
    }
    let count = order.map_or(repository.object_count(), <[usize]>::len);
    u32::try_from(count).map_err(|_| Error::Unsupported)?;
    let entries = entries.get_mut(..count).ok_or(Error::BufferTooSmall)?;
    if count != 0 && scratch.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    for (position, entry) in entries.iter_mut().enumerate() {
        let index = order.map_or(position, |order| order[position]);
        let id = repository.object_id(index).ok_or(Error::ObjectNotFound)?;
        if id.format() != repository.object_format() {
            return Err(Error::InvalidObjectId);
        }
        repository.object_info(index)?;
        *entry = IndexEntry {
            id,
            // Retain caller order while sorting IDs for the uniqueness check.
            // The selected count above fits u32, so each position fits u64.
            offset: position as u64,
            crc32: 0,
        };
    }
    entries.sort_unstable_by_key(|entry| entry.id);
    if entries.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::InvalidObject);
    }
    entries.sort_unstable_by_key(|entry| entry.offset);

    Ok(entries)
}

/// Stream a Git version-2 index for already verified pack metadata. Supports
/// SHA-1/SHA-256, empty packs and 64-bit offsets. The caller supplies entries in
/// strictly increasing ID order and the corresponding complete pack checksum.
/// Malformed metadata fails before output; sink failures invalidate partial
/// output. This validates metadata shape, not pack bytes or object bodies.
/// The returned index checksum is also written as its trailer.
pub fn write_index(
    entries: &[IndexEntry],
    pack_checksum: ObjectId,
    output: &mut impl Write,
) -> Result<ObjectId> {
    u32::try_from(entries.len()).map_err(|_| Error::Unsupported)?;
    let format = pack_checksum.format();
    if entries
        .iter()
        .any(|entry| entry.id.format() != format || entry.offset < 12)
        || entries.windows(2).any(|pair| pair[0].id >= pair[1].id)
    {
        return Err(Error::InvalidObject);
    }
    // The high bit of each 32-bit large-offset reference is its tag.
    if entries
        .iter()
        .filter(|entry| entry.offset >= 0x8000_0000)
        .count() as u64
        > 0x8000_0000
    {
        return Err(Error::Unsupported);
    }
    let mut output = IndexWriter {
        output,
        hash: format.hash(),
    };
    output.write_all(b"\xfftOc")?;
    output.write_all(&2_u32.to_be_bytes())?;
    output.write_fanout(entries)?;
    for entry in entries {
        output.write_all(entry.id.as_bytes())?;
    }
    for entry in entries {
        output.write_all(&entry.crc32.to_be_bytes())?;
    }
    output.write_offsets(entries)?;
    output.write_all(pack_checksum.as_bytes())?;
    let checksum = output.hash.finish();
    output.output.write_all(checksum.as_bytes())?;
    Ok(checksum)
}

struct PackWriter<'a, W> {
    output: &'a mut W,
    hash: Hash,
    offset: u64,
    crc: Crc32,
}

impl<W: Write> PackWriter<'_, W> {
    fn write_entry(
        &mut self,
        repository: &impl Repository,
        index: usize,
        id: ObjectId,
        scratch: &mut [u8],
    ) -> Result<IndexEntry> {
        let info = repository.object_info(index)?;
        let offset = self.offset;
        self.crc = Crc32::new();
        encode::write_entry_header(info, self)?;
        let mut encoder = LooseEncoder::from_info(index, info, false)?;
        loop {
            let count = encoder.read(repository, scratch)?;
            if count == 0 {
                break;
            }
            self.write_all(&scratch[..count])?;
        }
        Ok(IndexEntry {
            id,
            offset,
            crc32: self.crc.value(),
        })
    }
}

impl<W: Write> Write for PackWriter<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::LimitExceeded)?;
        self.output.write_all(bytes)?;
        self.hash.update(bytes);
        self.crc.update(bytes);
        self.offset = end;
        Ok(())
    }
}

struct IndexWriter<'a, W> {
    output: &'a mut W,
    hash: Hash,
}

impl<W: Write> IndexWriter<'_, W> {
    fn write_fanout(&mut self, entries: &[IndexEntry]) -> Result<()> {
        let mut position = 0;
        for first in 0..=255_u8 {
            while position < entries.len() && entries[position].id.as_bytes()[0] <= first {
                position += 1;
            }
            self.write_all(&(position as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn write_offsets(&mut self, entries: &[IndexEntry]) -> Result<()> {
        let mut large = 0_u32;
        for entry in entries {
            let offset = if entry.offset < 0x8000_0000 {
                entry.offset as u32
            } else {
                let offset = 0x8000_0000 | large;
                large += 1;
                offset
            };
            self.write_all(&offset.to_be_bytes())?;
        }
        for entry in entries.iter().filter(|entry| entry.offset >= 0x8000_0000) {
            self.write_all(&entry.offset.to_be_bytes())?;
        }
        Ok(())
    }
}

impl<W: Write> Write for IndexWriter<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.output.write_all(bytes)?;
        self.hash.update(bytes);
        Ok(())
    }
}
