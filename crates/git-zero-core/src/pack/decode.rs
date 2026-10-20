//! Bounded SHA-1/SHA-256 pack consumption, based on Git's pack specification:
//! https://git-scm.com/docs/pack-format. No recursion or allocation is used.
use super::crc::crc32;
use crate::{
    Error, Object, ObjectFormat, ObjectId, ObjectKind, Result,
    zlib::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Object(ObjectKind),
    OffsetDelta { base_offset: u64 },
    ReferenceDelta { base_id: ObjectId },
}

/// Pack-relative position, representation, and inflated length of one entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryHeader {
    pub offset: u64,
    pub kind: EntryKind,
    pub size: u64,
}

/// One checksummed/inflated entry. Delta data is an instruction stream rather
/// than a Git object body; resolve it against an authenticated materialized base.
pub struct Entry<'a> {
    offset: u64,
    end_offset: u64,
    crc: u32,
    kind: EntryKind,
    data: &'a [u8],
    pack_id: ObjectId,
}

impl<'a> Entry<'a> {
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
    pub fn crc32(&self) -> u32 {
        self.crc
    }
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Compare the entry's wire CRC with a trusted pack-index CRC. Packs do not
    /// carry per-entry CRCs themselves; the complete pack hash is also checked.
    pub fn verify_crc32(&self, expected: u32) -> Result<()> {
        if self.crc == expected {
            Ok(())
        } else {
            Err(Error::CorruptPack)
        }
    }

    /// Materialize metadata/ID for a non-delta entry without copying its body.
    pub fn object(&self) -> Option<ResolvedObject<'a>> {
        let EntryKind::Object(kind) = self.kind else {
            return None;
        };
        Some(ResolvedObject::new(
            Object {
                kind,
                data: self.data,
            },
            Some((self.pack_id, self.offset)),
            0,
            self.pack_id.format(),
        ))
    }

    /// Resolve one delta using a previously resolved entry or an external base.
    /// Offset bases must come from this pack at exactly the advertised offset;
    /// reference bases must have exactly the advertised canonical object ID.
    /// Depth is carried by resolved values, and output/instruction/depth budgets
    /// are checked before returning an object. Discard staging output on error.
    pub fn resolve_delta<'out>(
        &self,
        base: &ResolvedObject<'_>,
        output: &'out mut [u8],
        limits: DeltaLimits,
    ) -> Result<ResolvedObject<'out>> {
        match self.kind {
            EntryKind::Object(_) => return Err(Error::InvalidObject),
            EntryKind::OffsetDelta { base_offset } => {
                if base.location != Some((self.pack_id, base_offset)) {
                    return Err(Error::ObjectNotFound);
                }
            }
            EntryKind::ReferenceDelta { base_id } => {
                if base.id != base_id {
                    return Err(Error::ObjectNotFound);
                }
            }
        }
        if base.depth >= limits.max_depth {
            return Err(Error::LimitExceeded);
        }
        let object = apply_delta(base.object, self.data, output, limits)?;
        Ok(ResolvedObject::new(
            object,
            Some((self.pack_id, self.offset)),
            base.depth + 1,
            self.pack_id.format(),
        ))
    }
}

/// A canonical body with a computed ID and tracked delta depth. A decoded entry
/// retains its pack/offset identity, preventing cross-pack offset-base confusion.
pub struct ResolvedObject<'a> {
    object: Object<'a>,
    id: ObjectId,
    location: Option<(ObjectId, u64)>,
    depth: u32,
}

impl<'a> ResolvedObject<'a> {
    fn new(
        object: Object<'a>,
        location: Option<(ObjectId, u64)>,
        depth: u32,
        format: ObjectFormat,
    ) -> Self {
        Self {
            id: object.id_with_format(format),
            object,
            location,
            depth,
        }
    }
    /// Use a canonical object from host storage as a reference-delta base.
    pub fn external(object: Object<'a>) -> Self {
        Self::external_with_format(object, ObjectFormat::Sha1)
    }
    pub fn external_with_format(object: Object<'a>, format: ObjectFormat) -> Self {
        Self::new(object, None, 0, format)
    }
    pub fn object(&self) -> Object<'a> {
        self.object
    }
    pub fn id(&self) -> ObjectId {
        self.id
    }
    pub fn offset(&self) -> Option<u64> {
        self.location.map(|(_, offset)| offset)
    }
    pub fn delta_depth(&self) -> u32 {
        self.depth
    }
    pub fn verify_id(&self, expected: ObjectId) -> Result<()> {
        if self.id == expected {
            Ok(())
        } else {
            Err(Error::CorruptPack)
        }
    }
}

/// Reader over one complete downloaded pack. Construction verifies its
/// framing and hash; entries are then inflated/length-checked one at a time.
/// Each call borrows caller staging storage and a reusable 32 KiB zlib window.
/// Call `finish` (or read through `None`) to verify count and exact pack closure.
/// Forward reference deltas require host base lookup; thin-pack bases may be
/// supplied as `ResolvedObject::external`. No pack-index file parser is implied.
pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    count: u32,
    remaining: u32,
    pack_id: ObjectId,
}

impl<'a> Reader<'a> {
    pub fn new(pack: &'a [u8]) -> Result<Self> {
        Self::new_with_format(pack, ObjectFormat::Sha1)
    }
    pub fn new_with_format(pack: &'a [u8], format: ObjectFormat) -> Result<Self> {
        super::validate_with_format(pack, format)?;
        let end = pack.len() - format.id_length();
        let count = u32::from_be_bytes(pack[8..12].try_into().map_err(|_| Error::CorruptPack)?);
        let pack_id = ObjectId::from_bytes(format, &pack[end..]).map_err(|_| Error::CorruptPack)?;
        if count == 0 && end != 12 {
            return Err(Error::CorruptPack);
        }
        Ok(Self {
            bytes: &pack[..end],
            position: 12,
            count,
            remaining: count,
            pack_id,
        })
    }

    pub fn object_count(&self) -> u32 {
        self.count
    }
    pub fn remaining(&self) -> u32 {
        self.remaining
    }
    pub fn position(&self) -> u64 {
        self.position as u64
    }
    pub fn pack_id(&self) -> ObjectId {
        self.pack_id
    }

    /// Inspect the next entry's declared inflated body/instruction size without
    /// advancing. Hosts must apply their own budget before allocating storage.
    /// The declaration is checked against the actual stream by `read_entry`.
    pub fn next_entry_size(&self) -> Result<Option<u64>> {
        Ok(self.entry_header()?.map(|(_, size, _)| size))
    }

    /// Read the next entry. Declared size is the inflated body/instruction size.
    /// A staging-capacity or budget error preserves the cursor for retry.
    /// Other errors also preserve the cursor; staging bytes may have changed.
    pub fn read_entry<'out>(
        &mut self,
        output: &'out mut [u8],
        window: &mut [u8; WINDOW_SIZE],
        limits: InflateLimits,
    ) -> Result<Option<Entry<'out>>> {
        let Some((cursor, size, kind)) = self.entry_header()? else {
            return Ok(None);
        };
        let start = self.position;
        if size > limits.max_output {
            return Err(Error::LimitExceeded);
        }
        let size = usize::try_from(size).map_err(|_| Error::BufferTooSmall)?;
        let output = output.get_mut(..size).ok_or(Error::BufferTooSmall)?;
        let progress = Inflater::new(window, limits)
            .update(&self.bytes[cursor..], output, true)
            .map_err(|error| match error {
                Error::LimitExceeded => error,
                _ => Error::CorruptPack,
            })?;
        if progress.status != InflateStatus::Finished || progress.written != size {
            return Err(Error::CorruptPack);
        }
        let end = cursor + progress.consumed;
        if self.remaining == 1 && end != self.bytes.len() {
            return Err(Error::CorruptPack);
        }
        let crc = crc32(&self.bytes[start..end]);
        self.position = end;
        self.remaining -= 1;
        Ok(Some(Entry {
            offset: start as u64,
            end_offset: end as u64,
            crc,
            kind,
            data: output,
            pack_id: self.pack_id,
        }))
    }

    fn entry_header(&self) -> Result<Option<(usize, u64, EntryKind)>> {
        if self.remaining == 0 {
            self.finish()?;
            return Ok(None);
        }
        let (length, header) = parse_entry_header(
            &self.bytes[self.position..],
            self.position as u64,
            self.pack_id.format(),
        )
        .map_err(|error| {
            if error == Error::BufferTooSmall {
                Error::CorruptPack
            } else {
                error
            }
        })?;
        Ok(Some((self.position + length, header.size, header.kind)))
    }

    pub fn finish(&self) -> Result<()> {
        if self.remaining == 0 && self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::CorruptPack)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaLimits {
    pub max_output: u64,
    pub max_instructions: u64,
    /// Zero rejects every delta; one permits a delta against a plain base.
    pub max_depth: u32,
}

/// Apply Git delta instructions to a canonical base, inheriting its object kind.
/// Both size headers, source/target ranges, instruction count and exact closure
/// are validated. The returned body is exposed only on success; staging output
/// may have changed on error. This function applies one level; `resolve_delta`
/// additionally tracks and bounds chain depth across resolved entries.
pub fn apply_delta<'a>(
    base: Object<'_>,
    mut delta: &[u8],
    output: &'a mut [u8],
    limits: DeltaLimits,
) -> Result<Object<'a>> {
    if limits.max_depth == 0 {
        return Err(Error::LimitExceeded);
    }
    let base_size = variable(&mut delta)?;
    let result_size = variable(&mut delta)?;
    if base_size != base.data.len() as u64 {
        return Err(Error::CorruptPack);
    }
    if result_size > limits.max_output {
        return Err(Error::LimitExceeded);
    }
    let size = usize::try_from(result_size).map_err(|_| Error::BufferTooSmall)?;
    let output = output.get_mut(..size).ok_or(Error::BufferTooSmall)?;
    let (mut written, mut instructions) = (0_usize, 0_u64);
    while !delta.is_empty() {
        if instructions == limits.max_instructions {
            return Err(Error::LimitExceeded);
        }
        instructions += 1;
        let opcode = take_byte(&mut delta)?;
        let bytes = match opcode {
            0 => return Err(Error::CorruptPack),
            1..=127 => {
                let size = usize::from(opcode);
                let bytes = delta.get(..size).ok_or(Error::CorruptPack)?;
                delta = &delta[size..];
                bytes
            }
            _ => delta_copy(base.data, opcode, &mut delta)?,
        };
        let end = written.checked_add(bytes.len()).ok_or(Error::CorruptPack)?;
        output
            .get_mut(written..end)
            .ok_or(Error::CorruptPack)?
            .copy_from_slice(bytes);
        written = end;
    }
    if written != output.len() {
        return Err(Error::CorruptPack);
    }
    Ok(Object {
        kind: base.kind,
        data: output,
    })
}

// Shared by the complete-pack reader and incremental intake. A missing byte
// is distinct from malformed syntax so only the incremental caller can wait.
pub(super) fn parse_entry_header(
    bytes: &[u8],
    offset: u64,
    format: ObjectFormat,
) -> Result<(usize, EntryHeader)> {
    let mut cursor = 0;
    let first = byte(bytes, &mut cursor)?;
    let mut size = u64::from(first & 15);
    let mut last = first;
    let mut shift = 4;
    while last & 128 != 0 {
        last = byte(bytes, &mut cursor)?;
        size |= chunk(u64::from(last & 127), shift)?;
        shift += 7;
    }
    let kind = match (first >> 4) & 7 {
        1 => EntryKind::Object(ObjectKind::Commit),
        2 => EntryKind::Object(ObjectKind::Tree),
        3 => EntryKind::Object(ObjectKind::Blob),
        4 => EntryKind::Object(ObjectKind::Tag),
        6 => EntryKind::OffsetDelta {
            base_offset: offset_base(bytes, &mut cursor, offset)?,
        },
        7 => {
            let end = cursor + format.id_length();
            let id = bytes.get(cursor..end).ok_or(Error::BufferTooSmall)?;
            cursor = end;
            EntryKind::ReferenceDelta {
                base_id: ObjectId::from_bytes(format, id)?,
            }
        }
        _ => return Err(Error::CorruptPack),
    };
    Ok((cursor, EntryHeader { offset, kind, size }))
}

fn offset_base(bytes: &[u8], cursor: &mut usize, start: u64) -> Result<u64> {
    let mut last = byte(bytes, cursor)?;
    let mut distance = u64::from(last & 127);
    while last & 128 != 0 {
        last = byte(bytes, cursor)?;
        distance = distance
            .checked_add(1)
            .and_then(|n| n.checked_mul(128))
            .and_then(|n| n.checked_add(u64::from(last & 127)))
            .ok_or(Error::CorruptPack)?;
    }
    let base = start.checked_sub(distance).ok_or(Error::CorruptPack)?;
    if distance == 0 || base < 12 {
        return Err(Error::CorruptPack);
    }
    Ok(base)
}

fn delta_copy<'a>(base: &'a [u8], opcode: u8, delta: &mut &[u8]) -> Result<&'a [u8]> {
    let offset = sparse_parameter(opcode, 4, delta)?;
    let count = sparse_parameter(opcode >> 4, 3, delta)?;
    let count = if count == 0 { 65536 } else { count };
    let offset = usize::try_from(offset).map_err(|_| Error::CorruptPack)?;
    let end = offset
        .checked_add(count as usize)
        .ok_or(Error::CorruptPack)?;
    base.get(offset..end).ok_or(Error::CorruptPack)
}

/// Copy operands omit bytes whose corresponding opcode bit is clear.
#[inline]
fn sparse_parameter(mut flags: u8, width: u32, bytes: &mut &[u8]) -> Result<u32> {
    let mut value = 0;
    for index in 0..width {
        if flags & 1 != 0 {
            value |= u32::from(take_byte(bytes)?) << (index * 8);
        }
        flags >>= 1;
    }
    Ok(value)
}

fn byte(bytes: &[u8], cursor: &mut usize) -> Result<u8> {
    let result = bytes.get(*cursor).copied().ok_or(Error::BufferTooSmall)?;
    *cursor += 1;
    Ok(result)
}

fn take_byte(bytes: &mut &[u8]) -> Result<u8> {
    let (&first, remaining) = bytes.split_first().ok_or(Error::CorruptPack)?;
    *bytes = remaining;
    Ok(first)
}

fn chunk(value: u64, shift: u32) -> Result<u64> {
    if shift >= 64 || value > u64::MAX >> shift {
        return Err(Error::CorruptPack);
    }
    Ok(value << shift)
}

fn variable(bytes: &mut &[u8]) -> Result<u64> {
    let (mut result, mut shift) = (0, 0);
    loop {
        let byte = take_byte(bytes)?;
        result |= chunk(u64::from(byte & 127), shift)?;
        if byte & 128 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}
