//! Pack assembly using previously authenticated compressed object entries.
use super::{IndexEntry, crc::Crc32, encode::write_typed_header};
use crate::{
    Error, ObjectFormat, ObjectId, ObjectKind, Result,
    io::{SliceWriter, Write},
    object::Hash,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressedKind {
    Object(ObjectKind),
    /// Reference deltas permit arbitrary output ordering and pack offsets.
    /// Convert OFS bases to their verified object IDs before starting an entry.
    Delta {
        base_id: ObjectId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompressedEntry {
    pub id: ObjectId,
    pub kind: CompressedKind,
    /// Inflated object size, or inflated instruction size for a delta.
    pub inflated_size: u64,
    /// Exactly one complete zlib stream, excluding the original entry header.
    pub compressed_size: u64,
}

struct Active {
    id: ObjectId,
    offset: u64,
    remaining: u64,
    crc: Crc32,
}

/// Assemble a version-2 pack without inflating/recompressing stored entries.
/// All state is inline; each sink is supplied per call so a host can await
/// storage/network operations between bounded writes. Preserve produced bytes.
/// Any error is terminal and invalidates the entire partial output.
///
/// This is an encoding API, not a validator. The caller MUST authenticate the
/// source streams and metadata, select unique IDs and an authorized object
/// closure, and provide every delta base either in the pack or in the receiver
/// (only when thin packs are allowed). Zlib bytes are copied unchanged. New
/// headers, entry CRCs and the pack checksum reflect the output representation.
pub struct PackComposer {
    format: ObjectFormat,
    remaining: u32,
    limit: u64,
    position: u64,
    hash: Hash,
    active: Option<Active>,
    checksum: Option<ObjectId>,
    failure: Option<Error>,
}

impl PackComposer {
    pub fn new(
        format: ObjectFormat,
        count: u32,
        max_pack_bytes: u64,
        output: &mut impl Write,
    ) -> Result<Self> {
        if max_pack_bytes < 12 + format.id_length() as u64 {
            return Err(Error::LimitExceeded);
        }
        let mut header = [0; 12];
        header[..8].copy_from_slice(b"PACK\0\0\0\x02");
        header[8..].copy_from_slice(&count.to_be_bytes());
        output.write_all(&header)?;
        let mut hash = format.hash();
        hash.update(&header);
        Ok(Self {
            format,
            remaining: count,
            limit: max_pack_bytes,
            position: 12,
            hash,
            active: None,
            checksum: None,
            failure: None,
        })
    }

    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn checksum(&self) -> Option<ObjectId> {
        self.checksum
    }

    pub fn start_entry(&mut self, entry: CompressedEntry, output: &mut impl Write) -> Result<()> {
        let result = self.begin(entry, output);
        self.record(result)
    }

    pub fn write_compressed(&mut self, bytes: &[u8], output: &mut impl Write) -> Result<()> {
        let result = self.copy(bytes, output);
        self.record(result)
    }

    /// Close the current entry after its exact compressed length was supplied.
    /// The returned metadata describes the newly encoded pack, not its source.
    pub fn finish_entry(&mut self) -> Result<IndexEntry> {
        let result = self.close_entry();
        self.record(result)
    }

    /// Write the checksum only after all declared entries are complete.
    /// Successful repeated calls return the checksum without writing again.
    pub fn finish(&mut self, output: &mut impl Write) -> Result<ObjectId> {
        let result = self.close(output);
        self.record(result)
    }

    fn ready(&self) -> Result<()> {
        self.failure.map_or(Ok(()), Err)
    }

    fn record<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error) = result {
            self.failure = Some(error);
            self.checksum = None;
        }
        result
    }

    fn begin(&mut self, entry: CompressedEntry, output: &mut impl Write) -> Result<()> {
        self.ready()?;
        if self.active.is_some() || self.remaining == 0 || self.checksum.is_some() {
            return Err(Error::InvalidRequest);
        }
        if entry.id.format() != self.format {
            return Err(Error::InvalidObjectId);
        }
        if entry.compressed_size < 8 {
            return Err(Error::InvalidObject);
        }
        let mut storage = [0; 64];
        let mut header = SliceWriter::new(&mut storage);
        match entry.kind {
            CompressedKind::Object(kind) => {
                write_typed_header(kind as u8, entry.inflated_size, &mut header)?
            }
            CompressedKind::Delta { base_id } => {
                if base_id.format() != self.format {
                    return Err(Error::InvalidObjectId);
                }
                write_typed_header(7, entry.inflated_size, &mut header)?;
                header.write_all(base_id.as_bytes())?;
            }
        }
        let bytes = header.finish();
        let end = self
            .position
            .checked_add(bytes.len() as u64)
            .and_then(|end| end.checked_add(entry.compressed_size))
            .and_then(|end| end.checked_add(self.format.id_length() as u64))
            .ok_or(Error::LimitExceeded)?;
        if end > self.limit {
            return Err(Error::LimitExceeded);
        }
        output.write_all(bytes)?;
        self.hash.update(bytes);
        let mut crc = Crc32::new();
        crc.update(bytes);
        self.active = Some(Active {
            id: entry.id,
            offset: self.position,
            remaining: entry.compressed_size,
            crc,
        });
        self.position += bytes.len() as u64;
        Ok(())
    }

    fn copy(&mut self, bytes: &[u8], output: &mut impl Write) -> Result<()> {
        self.ready()?;
        let active = self.active.as_mut().ok_or(Error::InvalidRequest)?;
        if bytes.len() as u64 > active.remaining {
            return Err(Error::InvalidObject);
        }
        output.write_all(bytes)?;
        self.hash.update(bytes);
        active.crc.update(bytes);
        active.remaining -= bytes.len() as u64;
        self.position += bytes.len() as u64;
        Ok(())
    }

    fn close_entry(&mut self) -> Result<IndexEntry> {
        self.ready()?;
        let active = self.active.as_ref().ok_or(Error::InvalidRequest)?;
        if active.remaining != 0 {
            return Err(Error::InvalidObject);
        }
        let entry = IndexEntry {
            id: active.id,
            offset: active.offset,
            crc32: active.crc.value(),
        };
        self.remaining -= 1;
        self.active = None;
        Ok(entry)
    }

    fn close(&mut self, output: &mut impl Write) -> Result<ObjectId> {
        self.ready()?;
        if let Some(id) = self.checksum {
            return Ok(id);
        }
        if self.remaining != 0 || self.active.is_some() {
            return Err(Error::InvalidObject);
        }
        let id = self.hash.clone().finish();
        output.write_all(id.as_bytes())?;
        self.position += id.as_bytes().len() as u64;
        self.checksum = Some(id);
        Ok(id)
    }
}
