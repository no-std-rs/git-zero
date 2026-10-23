//! Pull-based version-2 indexes over immutable, sorted entry metadata.
use super::{IndexEntry, index::validate_entries};
use crate::{Error, ObjectId, Result, object::Hash};

#[derive(Clone, Copy)]
enum Phase {
    Header,
    Fanout,
    Ids,
    Crcs,
    Offsets,
    LargeOffsets,
    PackChecksum,
    Trailer,
    Done,
}

/// An allocation-free cursor for the same bytes as `write_index`. Metadata must
/// remain immutable until EOF. Each pull copies at most one field (32 bytes),
/// regardless of the supplied buffer size; zero means complete, never pending.
/// Construction checks metadata before output. It does not authenticate a pack.
pub struct IndexEncoder<'a> {
    entries: &'a [IndexEntry],
    pack_checksum: ObjectId,
    hash: Hash,
    phase: Phase,
    next: usize,
    fanout_end: usize,
    large: u32,
    pending: [u8; 32],
    position: usize,
    length: usize,
    size: u64,
}

impl<'a> IndexEncoder<'a> {
    pub fn new(entries: &'a [IndexEntry], pack_checksum: ObjectId) -> Result<Self> {
        validate_entries(entries, pack_checksum)?;
        let large = entries
            .iter()
            .filter(|entry| entry.offset >= 0x8000_0000)
            .count();
        let size = 1032_u64
            .checked_add((entries.len() as u64) * (pack_checksum.format().id_length() as u64 + 8))
            .and_then(|size| size.checked_add(large as u64 * 8))
            .and_then(|size| size.checked_add(2 * pack_checksum.format().id_length() as u64))
            .ok_or(Error::LimitExceeded)?;
        Ok(Self {
            entries,
            pack_checksum,
            hash: pack_checksum.format().hash(),
            phase: Phase::Header,
            next: 0,
            fanout_end: 0,
            large: 0,
            pending: [0; 32],
            position: 0,
            length: 0,
            size,
        })
    }

    /// Complete wire length, including both checksums and any 64-bit offsets.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Empty buffers fail without advancing an active cursor. EOF is repeatable.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        if self.position == self.length {
            if matches!(self.phase, Phase::Done) {
                return Ok(0);
            }
            if output.is_empty() {
                return Err(Error::BufferTooSmall);
            }
            self.refill();
        }
        if self.position == self.length {
            return Ok(0);
        }
        if output.is_empty() {
            return Err(Error::BufferTooSmall);
        }
        let count = output.len().min(self.length - self.position);
        output[..count].copy_from_slice(&self.pending[self.position..self.position + count]);
        if !matches!(self.phase, Phase::Done) {
            self.hash.update(&output[..count]);
        }
        self.position += count;
        Ok(count)
    }

    fn field(&mut self, bytes: &[u8]) {
        self.pending[..bytes.len()].copy_from_slice(bytes);
        self.position = 0;
        self.length = bytes.len();
    }

    fn advance(&mut self, phase: Phase) {
        self.phase = phase;
        self.next = 0;
    }

    fn refill(&mut self) {
        loop {
            match self.phase {
                Phase::Header => {
                    self.field(b"\xfftOc\0\0\0\x02");
                    self.advance(Phase::Fanout);
                }
                Phase::Fanout if self.next < 256 => {
                    while self.fanout_end < self.entries.len()
                        && self.entries[self.fanout_end].id.as_bytes()[0] as usize <= self.next
                    {
                        self.fanout_end += 1;
                    }
                    self.field(&(self.fanout_end as u32).to_be_bytes());
                    self.next += 1;
                }
                Phase::Fanout => {
                    self.advance(Phase::Ids);
                    continue;
                }
                Phase::Ids if self.next < self.entries.len() => {
                    self.field(self.entries[self.next].id.as_bytes());
                    self.next += 1;
                }
                Phase::Ids => {
                    self.advance(Phase::Crcs);
                    continue;
                }
                Phase::Crcs if self.next < self.entries.len() => {
                    self.field(&self.entries[self.next].crc32.to_be_bytes());
                    self.next += 1;
                }
                Phase::Crcs => {
                    self.advance(Phase::Offsets);
                    continue;
                }
                Phase::Offsets if self.next < self.entries.len() => {
                    let offset = self.entries[self.next].offset;
                    let value = if offset < 0x8000_0000 {
                        offset as u32
                    } else {
                        let value = 0x8000_0000 | self.large;
                        self.large += 1;
                        value
                    };
                    self.field(&value.to_be_bytes());
                    self.next += 1;
                }
                Phase::Offsets => {
                    self.advance(Phase::LargeOffsets);
                    continue;
                }
                Phase::LargeOffsets => {
                    while self.next < self.entries.len()
                        && self.entries[self.next].offset < 0x8000_0000
                    {
                        self.next += 1;
                    }
                    if self.next == self.entries.len() {
                        self.advance(Phase::PackChecksum);
                        continue;
                    }
                    self.field(&self.entries[self.next].offset.to_be_bytes());
                    self.next += 1;
                }
                Phase::PackChecksum => {
                    let checksum = self.pack_checksum;
                    self.field(checksum.as_bytes());
                    self.advance(Phase::Trailer);
                }
                Phase::Trailer => {
                    self.field(self.hash.clone().finish().as_bytes());
                    self.advance(Phase::Done);
                }
                Phase::Done => {
                    self.position = 0;
                    self.length = 0;
                }
            }
            return;
        }
    }
}
