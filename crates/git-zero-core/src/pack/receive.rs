//! Incremental pack intake. All emitted bytes and metadata are provisional
//! until the pack trailer is verified. The host owns quarantine and publication.
use super::{EntryHeader, EntryKind, crc::Crc32, decode::parse_entry_header};
use crate::{
    Error, ObjectDigest, ObjectFormat, ObjectId, ObjectInfo, Result,
    object::Hash,
    zlib::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};

/// Independent wire, object-count, and per-entry expansion budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    pub max_pack_bytes: u64,
    pub max_objects: u32,
    /// Bounds inflated object bodies or delta instruction streams, not the
    /// eventual result of applying a delta. Hosts must bound delta resolution.
    pub inflate: InflateLimits,
}

/// Events stop decoding before the next entry. Hosts can stage body fragments
/// and persist metadata without retaining an object or an object-count array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeStatus {
    NeedInput,
    NeedOutput,
    Header {
        version: u32,
        objects: u32,
    },
    EntryStart(EntryHeader),
    EntryEnd {
        end_offset: u64,
        crc32: u32,
        /// Canonical ID for a plain object. Delta IDs require base resolution.
        object_id: Option<ObjectId>,
    },
    Finished(ObjectId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeProgress {
    pub consumed: usize,
    pub written: usize,
    pub status: DecodeStatus,
}

#[derive(Clone, Copy)]
enum Phase {
    Header,
    Entry,
    Body(EntryHeader),
    Trailer,
    Done(ObjectId),
}

/// Push-driven SHA-1/SHA-256 pack decoder with fixed inline state and one
/// caller-owned 32 KiB inflate window. Neither input nor output is retained.
/// Supports plain, offset-delta and reference-delta entries, including thin
/// packs; checking base existence and graph connectivity is a host operation.
///
/// Preserve the unconsumed input suffix between calls. `EntryEnd` can include
/// the final body fragment in `written`; persist it before handling the event.
/// Every error is terminal, and all previously staged data must be discarded.
/// No ref may be published before `Finished` and host object/graph validation.
pub struct Decoder<'a> {
    inflater: Inflater<'a>,
    format: ObjectFormat,
    limits: DecodeLimits,
    phase: Phase,
    header: [u8; 64],
    header_len: usize,
    offset: u64,
    entry_offset: u64,
    remaining: u32,
    hash: Hash,
    crc: Crc32,
    object_hash: Option<ObjectDigest>,
    failure: Option<Error>,
}

impl<'a> Decoder<'a> {
    pub fn new(
        format: ObjectFormat,
        window: &'a mut [u8; WINDOW_SIZE],
        limits: DecodeLimits,
    ) -> Self {
        Self {
            inflater: Inflater::new(window, limits.inflate),
            format,
            limits,
            phase: Phase::Header,
            header: [0; 64],
            header_len: 0,
            offset: 0,
            entry_offset: 12,
            remaining: 0,
            hash: format.hash(),
            crc: Crc32::new(),
            object_hash: None,
            failure: None,
        }
    }

    pub fn position(&self) -> u64 {
        self.offset
    }

    /// `end_of_input` asserts that this slice contains the final available
    /// bytes. Events and output exhaustion can return before consuming it all;
    /// resubmit the remaining suffix with the same flag. A request for more
    /// input at EOF is an error. Bytes after the trailer are always rejected.
    pub fn update(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        end_of_input: bool,
    ) -> Result<DecodeProgress> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.step(input, output).and_then(|progress| {
            if end_of_input && progress.status == DecodeStatus::NeedInput {
                Err(Error::CorruptPack)
            } else {
                Ok(progress)
            }
        });
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    /// Require a verified trailer. An early call does not invalidate decoding;
    /// the host can continue supplying data. Transport EOF remains the host's
    /// responsibility, including checking for data after a verified trailer.
    pub fn finish(&self) -> Result<ObjectId> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        match self.phase {
            Phase::Done(id) => Ok(id),
            _ => Err(Error::CorruptPack),
        }
    }

    fn step(&mut self, input: &[u8], output: &mut [u8]) -> Result<DecodeProgress> {
        let mut consumed = 0;
        let status = loop {
            match self.phase {
                Phase::Done(id) => {
                    if consumed != input.len() {
                        return Err(Error::CorruptPack);
                    }
                    break DecodeStatus::Finished(id);
                }
                Phase::Body(entry) => return self.body(input, output, entry),
                Phase::Header | Phase::Entry | Phase::Trailer => {
                    let Some(&byte) = input.get(consumed) else {
                        break DecodeStatus::NeedInput;
                    };
                    self.account(&[byte], !matches!(self.phase, Phase::Trailer))?;
                    consumed += 1;
                    *self
                        .header
                        .get_mut(self.header_len)
                        .ok_or(Error::CorruptPack)? = byte;
                    self.header_len += 1;
                    if let Some(status) = self.header_byte()? {
                        break status;
                    }
                }
            }
        };
        Ok(DecodeProgress {
            consumed,
            written: 0,
            status,
        })
    }

    fn account(&mut self, bytes: &[u8], hashed: bool) -> Result<()> {
        let end = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::LimitExceeded)?;
        if end > self.limits.max_pack_bytes {
            return Err(Error::LimitExceeded);
        }
        self.offset = end;
        if hashed {
            self.hash.update(bytes);
        }
        if matches!(self.phase, Phase::Entry | Phase::Body(_)) {
            self.crc.update(bytes);
        }
        Ok(())
    }

    fn header_byte(&mut self) -> Result<Option<DecodeStatus>> {
        match self.phase {
            Phase::Header if self.header_len == 12 => self.pack_header().map(Some),
            Phase::Entry => self.start_entry(),
            Phase::Trailer if self.header_len == self.format.id_length() => {
                self.verify_trailer()?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn pack_header(&mut self) -> Result<DecodeStatus> {
        let version = u32::from_be_bytes(self.header[4..8].try_into().unwrap());
        if &self.header[..4] != b"PACK" || !matches!(version, 2 | 3) {
            return Err(Error::CorruptPack);
        }
        self.remaining = u32::from_be_bytes(self.header[8..12].try_into().unwrap());
        if self.remaining > self.limits.max_objects {
            return Err(Error::LimitExceeded);
        }
        self.next();
        Ok(DecodeStatus::Header {
            version,
            objects: self.remaining,
        })
    }

    fn start_entry(&mut self) -> Result<Option<DecodeStatus>> {
        let Some(entry) = entry_header(
            &self.header[..self.header_len],
            self.entry_offset,
            self.format,
        )?
        else {
            return Ok(None);
        };
        if entry.size > self.limits.inflate.max_output {
            return Err(Error::LimitExceeded);
        }
        self.inflater.reset(InflateLimits {
            max_output: entry.size,
            ..self.limits.inflate
        });
        self.object_hash = match entry.kind {
            EntryKind::Object(kind) => Some(ObjectDigest::new(
                ObjectInfo {
                    kind,
                    size: entry.size,
                },
                self.format,
            )),
            _ => None,
        };
        self.header_len = 0;
        self.phase = Phase::Body(entry);
        Ok(Some(DecodeStatus::EntryStart(entry)))
    }

    fn verify_trailer(&mut self) -> Result<()> {
        let id = self.hash.clone().finish();
        if id.as_bytes() != &self.header[..self.header_len] {
            return Err(Error::CorruptPack);
        }
        self.phase = Phase::Done(id);
        Ok(())
    }

    fn next(&mut self) {
        self.phase = if self.remaining == 0 {
            Phase::Trailer
        } else {
            Phase::Entry
        };
        self.header_len = 0;
        self.entry_offset = self.offset;
        self.crc = Crc32::new();
    }

    fn body(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        entry: EntryHeader,
    ) -> Result<DecodeProgress> {
        let allowance = (self.limits.max_pack_bytes - self.offset).min(input.len() as u64) as usize;
        let input = &input[..allowance];
        let progress = self
            .inflater
            .update(input, output, false)
            .map_err(|error| {
                if error == Error::LimitExceeded {
                    error
                } else {
                    Error::CorruptPack
                }
            })?;
        self.account(&input[..progress.consumed], true)?;
        if progress.status == InflateStatus::NeedInput && self.offset == self.limits.max_pack_bytes
        {
            return Err(Error::LimitExceeded);
        }
        if let Some(hash) = &mut self.object_hash {
            hash.update(&output[..progress.written])
                .map_err(|_| Error::CorruptPack)?;
        }
        let status = match progress.status {
            InflateStatus::NeedInput => DecodeStatus::NeedInput,
            InflateStatus::NeedOutput => DecodeStatus::NeedOutput,
            InflateStatus::Finished => {
                if self.inflater.total_output() != entry.size {
                    return Err(Error::CorruptPack);
                }
                let event = DecodeStatus::EntryEnd {
                    end_offset: self.offset,
                    crc32: self.crc.value(),
                    object_id: self
                        .object_hash
                        .take()
                        .map(|hash| hash.finish())
                        .transpose()
                        .map_err(|_| Error::CorruptPack)?,
                };
                self.remaining -= 1;
                self.next();
                event
            }
        };
        Ok(DecodeProgress {
            consumed: progress.consumed,
            written: progress.written,
            status,
        })
    }
}

fn entry_header(bytes: &[u8], offset: u64, format: ObjectFormat) -> Result<Option<EntryHeader>> {
    match parse_entry_header(bytes, offset, format) {
        Ok((_, header)) => Ok(Some(header)),
        Err(Error::BufferTooSmall) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_budget_bounds_inflater_work_on_empty_nonfinal_blocks() {
        let mut window = [0; WINDOW_SIZE];
        let limits = DecodeLimits {
            max_pack_bytes: 16,
            max_objects: 1,
            inflate: InflateLimits {
                max_output: 0,
                max_ratio: 1,
                ratio_slack: 0,
            },
        };
        let mut decoder = Decoder::new(ObjectFormat::Sha1, &mut window, limits);
        decoder
            .update(b"PACK\0\0\0\x02\0\0\0\x01", &mut [], false)
            .unwrap();
        decoder.update(&[0x30], &mut [], false).unwrap();
        let mut body = [0; 511];
        body[..2].copy_from_slice(&[0x78, 0x01]);
        for block in body[2..502].chunks_exact_mut(5) {
            block.copy_from_slice(&[0, 0, 0, 255, 255]);
        }
        body[502..].copy_from_slice(&[1, 0, 0, 255, 255, 0, 0, 0, 1]);
        assert_eq!(
            decoder.update(&body, &mut [], false),
            Err(Error::LimitExceeded)
        );
        assert!(decoder.inflater.total_input() <= 3);
        assert_eq!(
            decoder.update(&[], &mut [], true),
            Err(Error::LimitExceeded)
        );
    }

    #[test]
    fn entry_sizes_and_delta_offsets_are_not_limited_to_32_bits() {
        let header = [0xbf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0f];
        let entry = entry_header(&header, 12, ObjectFormat::Sha1)
            .unwrap()
            .unwrap();
        assert_eq!(entry.size, u64::MAX);
        let distance = u64::from(u32::MAX) + 129;
        let mut encoded = [0; 11];
        let mut cursor = encoded.len() - 1;
        let mut value = distance;
        encoded[cursor] = (value & 127) as u8;
        value >>= 7;
        while value != 0 {
            value -= 1;
            cursor -= 1;
            encoded[cursor] = 128 | (value & 127) as u8;
            value >>= 7;
        }
        cursor -= 1;
        encoded[cursor] = 0x60;
        let offset = distance + 12;
        let entry = entry_header(&encoded[cursor..], offset, ObjectFormat::Sha1)
            .unwrap()
            .unwrap();
        assert_eq!(entry.offset, offset);
        assert_eq!(entry.kind, EntryKind::OffsetDelta { base_offset: 12 });
        assert_eq!(
            entry_header(&encoded[cursor..], distance + 11, ObjectFormat::Sha1),
            Err(Error::CorruptPack)
        );
    }
}
