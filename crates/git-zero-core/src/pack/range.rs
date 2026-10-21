//! Resumable delta reconstruction with external metadata and range reads.
use super::{DeltaDecoder, DeltaLimits, DeltaStatus};
use crate::{Error, ObjectFormat, ObjectId, ObjectInfo, Result};

/// Canonical object identity or a host-local, unpublished staging record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKey {
    Id(ObjectId),
    Staged(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectRange {
    All,
    Bytes { offset: u64, length: u64 },
}

/// A delta inherits its base kind. Base metadata must already be authenticated;
/// the root may be staged if the caller requests All and verifies its hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeRepresentation {
    Plain,
    Delta {
        base: ObjectKey,
        base_info: ObjectInfo,
        base_depth: u32,
        instructions_size: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeObject {
    pub info: ObjectInfo,
    pub representation: RangeRepresentation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeLimits {
    pub max_output: u64,
    /// Total representation bytes consumed, across nested/repeated reads.
    /// Decompression and storage page reads have separate host budgets.
    pub max_work_bytes: u64,
    pub max_instructions: u64,
    pub max_depth: u32,
    pub max_read_bytes: usize,
}

/// Read inflated plain body bytes or inflated delta instructions. The host can
/// obtain these through EntryDecoder, a bounded cache, or immutable body storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeInput {
    Plain,
    Delta,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeRequest {
    Describe(ObjectKey),
    Read {
        object: ObjectKey,
        kind: RangeInput,
        offset: u64,
        length: usize,
    },
    NeedOutput,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeProgress {
    pub consumed: usize,
    pub written: usize,
    pub request: RangeRequest,
}

/// One caller-owned recursion slot. It contains no input/output storage.
/// Initialize arrays with `[const { RangeFrame::empty() }; N]`.
pub struct RangeFrame {
    key: ObjectKey,
    range: ObjectRange,
    expected: Option<(ObjectInfo, u32)>,
    object: Option<RangeObject>,
    delta: Option<DeltaDecoder>,
    cursor: u64,
    start: u64,
    end: u64,
    remaining: u64,
    complete: bool,
}

impl RangeFrame {
    pub const fn empty() -> Self {
        Self {
            key: ObjectKey::Staged(0),
            range: ObjectRange::All,
            expected: None,
            object: None,
            delta: None,
            cursor: 0,
            start: 0,
            end: 0,
            remaining: 0,
            complete: false,
        }
    }

    fn pending(key: ObjectKey, range: ObjectRange, expected: Option<(ObjectInfo, u32)>) -> Self {
        Self {
            key,
            range,
            expected,
            ..Self::empty()
        }
    }

    fn describe(
        &mut self,
        object: RangeObject,
        format: ObjectFormat,
        limits: RangeLimits,
        root: bool,
    ) -> Result<u64> {
        let depth = object_depth(object)?;
        if depth > limits.max_depth {
            return Err(Error::LimitExceeded);
        }
        if self
            .expected
            .is_some_and(|expected| expected != (object.info, depth))
        {
            return Err(Error::InvalidObject);
        }
        let (start, length) = match self.range {
            ObjectRange::All => (0, object.info.size),
            ObjectRange::Bytes { offset, length } => (offset, length),
        };
        let end = start
            .checked_add(length)
            .filter(|end| *end <= object.info.size)
            .ok_or(Error::InvalidObject)?;
        if root && length > limits.max_output {
            return Err(Error::LimitExceeded);
        }
        self.start = start;
        self.end = end;
        self.remaining = length;
        match object.representation {
            RangeRepresentation::Plain => self.cursor = start,
            RangeRepresentation::Delta {
                base,
                base_info,
                base_depth,
                ..
            } => {
                check_key(format, base)?;
                if base_info.kind != object.info.kind {
                    return Err(Error::InvalidObject);
                }
                self.delta = Some(DeltaDecoder::new(
                    base_info.size,
                    base_depth,
                    DeltaLimits {
                        max_output: object.info.size,
                        max_instructions: limits.max_instructions,
                        max_depth: limits.max_depth,
                    },
                )?);
            }
        }
        self.object = Some(object);
        Ok(length)
    }

    fn next_read(
        &mut self,
        max_read_bytes: usize,
        remaining_work: u64,
    ) -> Result<Option<RangeRequest>> {
        let Some(object) = self.object else {
            return Ok(Some(RangeRequest::Describe(self.key)));
        };
        let (kind, remaining) = match object.representation {
            RangeRepresentation::Plain => (RangeInput::Plain, self.remaining),
            RangeRepresentation::Delta {
                instructions_size, ..
            } => {
                if self.cursor == instructions_size {
                    let delta = self.delta.as_mut().ok_or(Error::InvalidObject)?;
                    let step = delta.update(&[], true)?;
                    if step.status != DeltaStatus::Finished || self.remaining != 0 {
                        return Err(Error::CorruptPack);
                    }
                    delta.finish()?;
                    self.complete = true;
                    return Ok(None);
                }
                (RangeInput::Delta, instructions_size - self.cursor)
            }
        };
        if remaining_work == 0 {
            return Err(Error::LimitExceeded);
        }
        let length = remaining.min(max_read_bytes as u64).min(remaining_work) as usize;
        Ok(Some(RangeRequest::Read {
            object: self.key,
            kind,
            offset: self.cursor,
            length,
        }))
    }

    fn done(&self) -> bool {
        self.complete
            || self.object.is_some_and(|object| {
                self.remaining == 0
                    && (matches!(object.representation, RangeRepresentation::Plain)
                        || self.end < object.info.size)
            })
    }
}

/// Reconstruct a canonical object or range without retaining a base/result or
/// a repository-sized index. Supply metadata and representation reads requested
/// by `request`; every read is resumable across storage awaits and output limits.
/// Input is never retained, including when a copy descends into another delta.
/// Unconsumed bytes can be resubmitted or refetched at the next requested offset.
///
/// Metadata/read replies must come from one immutable, authenticated snapshot.
/// Bases must already have verified canonical IDs, kinds, sizes and delta depth.
/// For a new staged root, request All, execute every read, hash emitted bytes
/// with ObjectDigest, and verify the pack and graph before publication. Partial
/// reads of existing objects can stop before the end of trusted instructions.
/// The host authenticates underlying storage; this API does not hash replies.
/// Caching inflated instruction pages and canonical base ranges avoids repeated
/// decompression. Frames and all budgets are supplied by the caller.
pub struct RangeResolver<'frames> {
    frames: &'frames mut [RangeFrame],
    active: usize,
    format: ObjectFormat,
    limits: RangeLimits,
    written: u64,
    work: u64,
    expected: Option<u64>,
    finished: bool,
    failure: Option<Error>,
}

impl<'frames> RangeResolver<'frames> {
    pub fn new(
        format: ObjectFormat,
        key: ObjectKey,
        range: ObjectRange,
        frames: &'frames mut [RangeFrame],
        limits: RangeLimits,
    ) -> Result<Self> {
        check_key(format, key)?;
        if frames.is_empty() {
            return Err(Error::BufferTooSmall);
        }
        if limits.max_read_bytes == 0 {
            return Err(Error::InvalidRequest);
        }
        if let ObjectRange::Bytes { offset, length } = range {
            offset.checked_add(length).ok_or(Error::InvalidObject)?;
            if length > limits.max_output {
                return Err(Error::LimitExceeded);
            }
        }
        frames[0] = RangeFrame::pending(key, range, None);
        Ok(Self {
            frames,
            active: 0,
            format,
            limits,
            written: 0,
            work: 0,
            expected: None,
            finished: false,
            failure: None,
        })
    }

    pub fn position(&self) -> u64 {
        self.written
    }
    pub fn work_bytes(&self) -> u64 {
        self.work
    }

    /// A repeated request remains valid until its reply. Backend failures can
    /// be retried without advancing. Malformed replies/budget errors are terminal.
    pub fn request(&mut self) -> Result<RangeRequest> {
        let result = self.next();
        self.record(result)
    }

    pub fn describe(&mut self, object: RangeObject) -> Result<()> {
        let result = self.accept_object(object);
        self.record(result)
    }

    /// Supply bytes for the current read. At most the requested range and
    /// output capacity are consumed; preserve the suffix. Empty output returns
    /// NeedOutput without advancing. Empty input for a nonempty read is corrupt.
    pub fn update(&mut self, input: &[u8], output: &mut [u8]) -> Result<RangeProgress> {
        let result = self.read(input, output);
        self.record(result)
    }

    pub fn finish(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.finished && self.expected == Some(self.written) {
            Ok(())
        } else {
            Err(Error::CorruptPack)
        }
    }

    fn record<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn next(&mut self) -> Result<RangeRequest> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.finished {
            return Ok(RangeRequest::Finished);
        }
        loop {
            let frame = &mut self.frames[self.active];
            if frame.done() {
                if self.active == 0 {
                    if self.expected != Some(self.written) {
                        return Err(Error::CorruptPack);
                    }
                    self.finished = true;
                    return Ok(RangeRequest::Finished);
                }
                self.active -= 1;
                continue;
            }
            if let Some(request) = frame.next_read(
                self.limits.max_read_bytes,
                self.limits.max_work_bytes - self.work,
            )? {
                return Ok(request);
            }
        }
    }

    fn accept_object(&mut self, object: RangeObject) -> Result<()> {
        if !matches!(self.next()?, RangeRequest::Describe(_)) {
            return Err(Error::InvalidRequest);
        }
        let length = self.frames[self.active].describe(
            object,
            self.format,
            self.limits,
            self.active == 0,
        )?;
        if self.active == 0 {
            self.expected = Some(length);
        }
        Ok(())
    }

    fn read(&mut self, input: &[u8], output: &mut [u8]) -> Result<RangeProgress> {
        let request = self.next()?;
        let RangeRequest::Read { kind, length, .. } = request else {
            if request == RangeRequest::Finished && input.is_empty() {
                return Ok(RangeProgress {
                    consumed: 0,
                    written: 0,
                    request,
                });
            }
            return Err(Error::InvalidRequest);
        };
        if output.is_empty() {
            return Ok(RangeProgress {
                consumed: 0,
                written: 0,
                request: RangeRequest::NeedOutput,
            });
        }
        if input.is_empty() {
            return Err(Error::CorruptPack);
        }
        let take = input.len().min(output.len()).min(length);
        let (consumed, written) = match kind {
            RangeInput::Plain => {
                output[..take].copy_from_slice(&input[..take]);
                let frame = &mut self.frames[self.active];
                frame.cursor += take as u64;
                frame.remaining -= take as u64;
                (take, take)
            }
            RangeInput::Delta => self.instructions(&input[..take], output)?,
        };
        self.work += consumed as u64;
        self.written += written as u64;
        Ok(RangeProgress {
            consumed,
            written,
            request: self.next()?,
        })
    }

    fn instructions(&mut self, input: &[u8], output: &mut [u8]) -> Result<(usize, usize)> {
        let frame = &mut self.frames[self.active];
        let object = frame.object.ok_or(Error::InvalidObject)?;
        let RangeRepresentation::Delta {
            base,
            base_info,
            base_depth,
            instructions_size,
        } = object.representation
        else {
            return Err(Error::InvalidObject);
        };
        let delta = frame.delta.as_mut().ok_or(Error::InvalidObject)?;
        let before = delta.position();
        let step = delta.update(
            input,
            frame.cursor + input.len() as u64 == instructions_size,
        )?;
        frame.cursor += step.consumed as u64;
        let mut written = 0;
        match step.status {
            DeltaStatus::Header { result_size, .. } => {
                if result_size != object.info.size {
                    return Err(Error::CorruptPack);
                }
            }
            DeltaStatus::Insert(bytes) => {
                if let Some((start, length)) =
                    overlap(before, bytes.len() as u64, frame.start, frame.end)
                {
                    written = length as usize;
                    let start = start as usize;
                    output[..written].copy_from_slice(&bytes[start..start + written]);
                    frame.remaining -= length;
                }
            }
            DeltaStatus::Copy { offset, length } => {
                if let Some((start, length)) = overlap(before, length, frame.start, frame.end) {
                    frame.remaining -= length;
                    if self.active + 1 == self.frames.len() {
                        return Err(Error::BufferTooSmall);
                    }
                    self.active += 1;
                    self.frames[self.active] = RangeFrame::pending(
                        base,
                        ObjectRange::Bytes {
                            offset: offset + start,
                            length,
                        },
                        Some((base_info, base_depth)),
                    );
                }
            }
            DeltaStatus::Finished => {
                if frame.remaining != 0 {
                    return Err(Error::CorruptPack);
                }
                frame.complete = true;
            }
            DeltaStatus::NeedInput => {}
        }
        Ok((step.consumed, written))
    }
}

fn overlap(position: u64, length: u64, start: u64, end: u64) -> Option<(u64, u64)> {
    let first = position.max(start);
    let last = (position + length).min(end);
    (first < last).then_some((first - position, last.saturating_sub(first)))
}

fn check_key(format: ObjectFormat, key: ObjectKey) -> Result<()> {
    if matches!(key, ObjectKey::Id(id) if id.format() != format) {
        Err(Error::InvalidObjectId)
    } else {
        Ok(())
    }
}

fn object_depth(object: RangeObject) -> Result<u32> {
    match object.representation {
        RangeRepresentation::Plain => Ok(0),
        RangeRepresentation::Delta { base_depth, .. } => {
            base_depth.checked_add(1).ok_or(Error::LimitExceeded)
        }
    }
}
