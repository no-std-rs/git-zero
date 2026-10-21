//! Bounded random access to authenticated, immutable Git version-2 indexes.
use super::IndexEntry;
use crate::{Error, ObjectFormat, ObjectId, Result};

/// An exact range to read from the same immutable index used for the header.
/// No read exceeds 32 bytes; hosts may cache/coalesce larger storage pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexRange {
    pub offset: u64,
    pub length: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupStatus {
    Read(IndexRange),
    Found { row: u32, entry: IndexEntry },
    Missing,
}

/// The 1032-byte fanout/header of a previously authenticated v2 index.
/// Parsing checks layout and fanout shape. It does NOT verify the full index
/// checksum, ID ordering, or its relationship to pack contents. Establish those
/// when importing/creating the index, before trusting lookup results or absence.
/// The header and every range reply must refer to that same immutable snapshot.
pub struct IndexHeader {
    fanout: [u32; 256],
    format: ObjectFormat,
    crc_start: u64,
    offset_start: u64,
    large_start: u64,
    large_count: u32,
    pack_end: u64,
}

impl IndexHeader {
    pub const BYTES: usize = 1032;

    pub fn parse(
        format: ObjectFormat,
        header: &[u8],
        index_bytes: u64,
        pack_bytes: u64,
    ) -> Result<Self> {
        if header.len() != Self::BYTES || &header[..8] != b"\xfftOc\0\0\0\x02" {
            return Err(Error::CorruptPack);
        }
        let mut fanout = [0; 256];
        let mut previous = 0;
        for (slot, bytes) in fanout.iter_mut().zip(header[8..].chunks_exact(4)) {
            *slot = number(bytes);
            if *slot < previous {
                return Err(Error::CorruptPack);
            }
            previous = *slot;
        }
        let count = u64::from(previous);
        let crc_start = Self::BYTES as u64 + count * format.id_length() as u64;
        let offset_start = crc_start + count * 4;
        let large_start = offset_start + count * 4;
        let table_bytes = index_bytes
            .checked_sub(large_start + 2 * format.id_length() as u64)
            .ok_or(Error::CorruptPack)?;
        if table_bytes % 8 != 0 || table_bytes / 8 > count || table_bytes / 8 > 0x8000_0000 {
            return Err(Error::CorruptPack);
        }
        let pack_end = pack_bytes
            .checked_sub(format.id_length() as u64)
            .filter(|end| *end >= 12)
            .ok_or(Error::CorruptPack)?;
        Ok(Self {
            fanout,
            format,
            crc_start,
            offset_start,
            large_start,
            large_count: (table_bytes / 8) as u32,
            pack_end,
        })
    }

    pub fn object_count(&self) -> u32 {
        self.fanout[255]
    }

    /// Start a fanout-limited binary search. The lookup owns only fixed state,
    /// so unrelated lookups can interleave storage operations without a lock.
    pub fn lookup(&self, id: ObjectId) -> Result<IndexLookup> {
        if id.format() != self.format {
            return Err(Error::InvalidObjectId);
        }
        let first = usize::from(id.as_bytes()[0]);
        let lower = if first == 0 {
            0
        } else {
            self.fanout[first - 1]
        };
        let upper = self.fanout[first];
        let mut lookup = IndexLookup {
            id,
            lower,
            upper,
            row: 0,
            crc32: 0,
            crc_start: self.crc_start,
            offset_start: self.offset_start,
            large_start: self.large_start,
            large_count: self.large_count,
            pack_end: self.pack_end,
            state: State::Missing,
        };
        lookup.search();
        Ok(lookup)
    }
}

#[derive(Clone, Copy)]
enum State {
    Id(IndexRange),
    Crc(IndexRange),
    Offset(IndexRange),
    Large(IndexRange),
    Found(IndexEntry),
    Missing,
    Failed(Error),
}

/// A resumable index lookup. Fetch `status()`'s range, then supply its exact
/// bytes to `update`. A storage error can be retried without advancing state.
/// Malformed replies are terminal. There is no allocation or provider callback.
pub struct IndexLookup {
    id: ObjectId,
    lower: u32,
    upper: u32,
    row: u32,
    crc32: u32,
    crc_start: u64,
    offset_start: u64,
    large_start: u64,
    large_count: u32,
    pack_end: u64,
    state: State,
}

impl IndexLookup {
    pub fn status(&self) -> Result<LookupStatus> {
        Ok(match self.state {
            State::Id(range) | State::Crc(range) | State::Offset(range) | State::Large(range) => {
                LookupStatus::Read(range)
            }
            State::Found(entry) => LookupStatus::Found {
                row: self.row,
                entry,
            },
            State::Missing => LookupStatus::Missing,
            State::Failed(error) => return Err(error),
        })
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<LookupStatus> {
        let result = self.reply(bytes).and_then(|()| self.status());
        if let Err(error) = result {
            self.state = State::Failed(error);
        }
        result
    }

    fn search(&mut self) {
        if self.lower == self.upper {
            self.state = State::Missing;
            return;
        }
        self.row = self.lower + (self.upper - self.lower) / 2;
        self.state = State::Id(IndexRange {
            offset: IndexHeader::BYTES as u64
                + u64::from(self.row) * self.id.format().id_length() as u64,
            length: self.id.format().id_length(),
        });
    }

    fn reply(&mut self, bytes: &[u8]) -> Result<()> {
        let LookupStatus::Read(range) = self.status()? else {
            return if bytes.is_empty() {
                Ok(())
            } else {
                Err(Error::CorruptPack)
            };
        };
        if bytes.len() != range.length {
            return Err(Error::CorruptPack);
        }
        match self.state {
            State::Id(_) => self.candidate(bytes)?,
            State::Crc(_) => {
                self.crc32 = number(bytes);
                self.state = State::Offset(IndexRange {
                    offset: self.offset_start + u64::from(self.row) * 4,
                    length: 4,
                });
            }
            State::Offset(_) => {
                let offset = number(bytes);
                if offset & 0x8000_0000 == 0 {
                    self.found(u64::from(offset))?;
                } else {
                    let index = offset & 0x7fff_ffff;
                    if index >= self.large_count {
                        return Err(Error::CorruptPack);
                    }
                    self.state = State::Large(IndexRange {
                        offset: self.large_start + u64::from(index) * 8,
                        length: 8,
                    });
                }
            }
            State::Large(_) => self.found(u64::from_be_bytes(
                bytes.try_into().map_err(|_| Error::CorruptPack)?,
            ))?,
            _ => unreachable!(),
        }
        Ok(())
    }

    fn candidate(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes[0] != self.id.as_bytes()[0] {
            return Err(Error::CorruptPack);
        }
        match bytes.cmp(self.id.as_bytes()) {
            core::cmp::Ordering::Less => {
                self.lower = self.row + 1;
                self.search();
            }
            core::cmp::Ordering::Greater => {
                self.upper = self.row;
                self.search();
            }
            core::cmp::Ordering::Equal => {
                self.state = State::Crc(IndexRange {
                    offset: self.crc_start + u64::from(self.row) * 4,
                    length: 4,
                });
            }
        }
        Ok(())
    }

    fn found(&mut self, offset: u64) -> Result<()> {
        if offset < 12 || offset >= self.pack_end {
            return Err(Error::CorruptPack);
        }
        self.state = State::Found(IndexEntry {
            id: self.id,
            offset,
            crc32: self.crc32,
        });
        Ok(())
    }
}

fn number(bytes: &[u8]) -> u32 {
    // Internal callers validate width before reaching this helper.
    u32::from_be_bytes(bytes.try_into().expect("four-byte index field"))
}
