//! Bounded reads of inflated ranges within an authenticated packed entry.
use super::{DecodeStatus, EntryDecoder};
use crate::{
    Error, ObjectFormat, Result,
    zlib::{InflateLimits, WINDOW_SIZE},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryRangeStatus {
    /// Internal progress was made, possibly without consuming/emitting bytes.
    Continue,
    NeedInput,
    NeedOutput,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryRangeProgress {
    pub consumed: usize,
    pub written: usize,
    pub status: EntryRangeStatus,
}

/// Read plain body or delta-instruction bytes from a previously authenticated
/// packed entry. Supply packed bytes starting at `entry_offset`. Inflation skips
/// the prefix using caller scratch, then emits only the requested inflated range.
/// One update performs at most one inflater step. Scratch retains pending bytes
/// across output backpressure; no whole entry/body/pack is retained.
///
/// Completion can precede the entry's end. It does NOT verify the remaining
/// compressed stream, entry CRC, pack checksum or canonical object identity.
/// Authenticate the source and index before partial reads. For new packs first
/// run Decoder to complete intake validation. Hosts may cache inflated pages to
/// avoid repeated prefix inflation when serving RangeResolver requests.
pub struct EntryRangeDecoder<'window, 'scratch> {
    decoder: EntryDecoder<'window>,
    scratch: &'scratch mut [u8],
    range: ByteRange,
    end: u64,
    inflated: u64,
    emitted: u64,
    pending: usize,
    pending_end: usize,
    header_seen: bool,
    finished: bool,
    failure: Option<Error>,
}

impl<'window, 'scratch> EntryRangeDecoder<'window, 'scratch> {
    pub fn new(
        format: ObjectFormat,
        entry_offset: u64,
        window: &'window mut [u8; WINDOW_SIZE],
        max_entry_bytes: u64,
        inflate: InflateLimits,
        range: ByteRange,
        scratch: &'scratch mut [u8],
    ) -> Result<Self> {
        let end = range
            .offset
            .checked_add(range.length)
            .ok_or(Error::InvalidObject)?;
        if scratch.is_empty() && range.length != 0 {
            return Err(Error::BufferTooSmall);
        }
        Ok(Self {
            decoder: EntryDecoder::new(format, entry_offset, window, max_entry_bytes, inflate)?,
            scratch,
            range,
            end,
            inflated: 0,
            emitted: 0,
            pending: 0,
            pending_end: 0,
            header_seen: false,
            finished: false,
            failure: None,
        })
    }

    pub fn wire_position(&self) -> u64 {
        self.decoder.position()
    }
    pub fn inflated_position(&self) -> u64 {
        self.inflated
    }
    pub fn position(&self) -> u64 {
        self.emitted
    }

    /// Preserve packed input not consumed and output not yet sent. Finished
    /// calls repeat without consuming following entries. Errors are terminal.
    pub fn update(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        end_of_input: bool,
    ) -> Result<EntryRangeProgress> {
        let result = self.advance(input, output, end_of_input);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    pub fn finish(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.finished {
            Ok(())
        } else {
            Err(Error::CorruptPack)
        }
    }

    fn advance(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        eof: bool,
    ) -> Result<EntryRangeProgress> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.finished {
            return Ok(EntryRangeProgress {
                consumed: 0,
                written: 0,
                status: EntryRangeStatus::Finished,
            });
        }
        if self.pending < self.pending_end {
            return Ok(self.drain(0, output));
        }
        // An empty range still reads its header to check the requested bound.
        if output.is_empty() && self.range.length != 0 {
            return Ok(EntryRangeProgress {
                consumed: 0,
                written: 0,
                status: EntryRangeStatus::NeedOutput,
            });
        }
        let progress = self.decoder.update(input, self.scratch, eof)?;
        if let DecodeStatus::EntryStart(header) = progress.status {
            if self.end > header.size {
                return Err(Error::InvalidObject);
            }
            self.header_seen = true;
            if self.range.length == 0 {
                self.finished = true;
            }
        }
        let before = self.inflated;
        self.inflated += progress.written as u64;
        let first = before.max(self.range.offset);
        let last = self.inflated.min(self.end);
        if first < last {
            self.pending = (first - before) as usize;
            self.pending_end = (last - before) as usize;
            return Ok(self.drain(progress.consumed, output));
        }
        let status = if self.finished {
            EntryRangeStatus::Finished
        } else {
            match progress.status {
                DecodeStatus::NeedInput => EntryRangeStatus::NeedInput,
                DecodeStatus::EntryEnd { .. } => return Err(Error::CorruptPack),
                _ => EntryRangeStatus::Continue,
            }
        };
        Ok(EntryRangeProgress {
            consumed: progress.consumed,
            written: 0,
            status,
        })
    }

    fn drain(&mut self, consumed: usize, output: &mut [u8]) -> EntryRangeProgress {
        let written = (self.pending_end - self.pending).min(output.len());
        output[..written].copy_from_slice(&self.scratch[self.pending..self.pending + written]);
        self.pending += written;
        self.emitted += written as u64;
        self.finished = self.header_seen && self.emitted == self.range.length;
        let status = if self.finished {
            EntryRangeStatus::Finished
        } else if self.pending < self.pending_end {
            EntryRangeStatus::NeedOutput
        } else {
            EntryRangeStatus::Continue
        };
        EntryRangeProgress {
            consumed,
            written,
            status,
        }
    }
}
