//! Resumable Git delta instructions, independent of base/result storage.
use super::DeltaLimits;
use crate::{Error, Result};

/// One instruction or boundary. Execute each copy/insert before resuming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaStatus<'input> {
    NeedInput,
    Header { base_size: u64, result_size: u64 },
    Copy { offset: u64, length: u64 },
    Insert(&'input [u8]),
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeltaProgress<'input> {
    pub consumed: usize,
    pub status: DeltaStatus<'input>,
}

#[derive(Clone, Copy)]
enum State {
    Base,
    Size,
    Opcode,
    Copy {
        flags: u8,
        index: u8,
        offset: u32,
        length: u32,
    },
    Insert(u8),
    Done,
    Failed(Error),
}

/// Parse fragmented, already inflated delta bytes with fixed inline state.
/// The trusted base must be canonical and completely resolved at `base_depth`.
/// Copies use 64-bit ranges so the host can await bounded storage reads between
/// events. Inserts borrow the current input, including partial instructions.
///
/// Output is provisional: the caller must execute all events, hash the result
/// with its inherited object kind, and validate the enclosing pack and graph
/// before publishing refs. `position` counts emitted bytes, not successful I/O.
pub struct DeltaDecoder {
    state: State,
    limits: DeltaLimits,
    base_size: u64,
    result_size: u64,
    position: u64,
    instructions: u64,
    variable: u64,
    shift: u32,
    depth: u32,
}

impl DeltaDecoder {
    pub fn new(base_size: u64, base_depth: u32, limits: DeltaLimits) -> Result<Self> {
        if base_depth >= limits.max_depth {
            return Err(Error::LimitExceeded);
        }
        Ok(Self {
            state: State::Base,
            limits,
            base_size,
            result_size: 0,
            position: 0,
            instructions: 0,
            variable: 0,
            shift: 0,
            depth: base_depth + 1,
        })
    }

    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Return at most one event. Resubmit the unconsumed suffix, retaining its
    /// EOF flag; `end_of_input` closes the delta, not merely this input chunk.
    /// Failures are terminal. Exact EOF is required, including an empty call
    /// after the last instruction if its event consumed the final input byte.
    pub fn update<'input>(
        &mut self,
        input: &'input [u8],
        end_of_input: bool,
    ) -> Result<DeltaProgress<'input>> {
        let result = self.advance(input, end_of_input);
        if let Err(error) = result {
            self.state = State::Failed(error);
        }
        result
    }

    pub fn finish(&self) -> Result<()> {
        match self.state {
            State::Done => Ok(()),
            State::Failed(error) => Err(error),
            _ => Err(Error::CorruptPack),
        }
    }

    fn advance<'input>(&mut self, input: &'input [u8], eof: bool) -> Result<DeltaProgress<'input>> {
        let mut consumed = 0;
        loop {
            let status = match self.state {
                State::Failed(error) => return Err(error),
                State::Done => {
                    if !input.is_empty() {
                        return Err(Error::CorruptPack);
                    }
                    Some(DeltaStatus::Finished)
                }
                State::Opcode if self.position == self.result_size => {
                    if consumed != input.len() {
                        return Err(Error::CorruptPack);
                    }
                    if eof {
                        self.state = State::Done;
                        Some(DeltaStatus::Finished)
                    } else {
                        Some(DeltaStatus::NeedInput)
                    }
                }
                State::Copy {
                    index: 7,
                    offset,
                    length,
                    ..
                } => Some(self.copy(offset, length)?),
                State::Copy {
                    flags,
                    index,
                    offset,
                    length,
                } if flags & (1 << index) == 0 => {
                    self.state = State::Copy {
                        flags,
                        index: index + 1,
                        offset,
                        length,
                    };
                    None
                }
                _ if consumed == input.len() => {
                    if eof {
                        return Err(Error::CorruptPack);
                    }
                    Some(DeltaStatus::NeedInput)
                }
                State::Base | State::Size => {
                    let byte = input[consumed];
                    consumed += 1;
                    self.size_byte(byte)?
                }
                State::Opcode => {
                    self.opcode(input[consumed])?;
                    consumed += 1;
                    None
                }
                State::Copy {
                    flags,
                    index,
                    mut offset,
                    mut length,
                } => {
                    let byte = u32::from(input[consumed]);
                    consumed += 1;
                    if index < 4 {
                        offset |= byte << (index * 8);
                    } else {
                        length |= byte << ((index - 4) * 8);
                    }
                    self.state = State::Copy {
                        flags,
                        index: index + 1,
                        offset,
                        length,
                    };
                    None
                }
                State::Insert(remaining) => {
                    let length = usize::from(remaining).min(input.len() - consumed);
                    let bytes = &input[consumed..consumed + length];
                    consumed += length;
                    self.emit(length as u64)?;
                    self.state = if length == usize::from(remaining) {
                        State::Opcode
                    } else {
                        State::Insert(remaining - length as u8)
                    };
                    Some(DeltaStatus::Insert(bytes))
                }
            };
            if let Some(status) = status {
                return Ok(DeltaProgress { consumed, status });
            }
        }
    }

    fn size_byte(&mut self, byte: u8) -> Result<Option<DeltaStatus<'static>>> {
        if self.shift >= 64 || u64::from(byte & 127) > (u64::MAX >> self.shift) {
            return Err(Error::CorruptPack);
        }
        self.variable |= u64::from(byte & 127) << self.shift;
        self.shift += 7;
        if byte & 128 != 0 {
            return Ok(None);
        }
        let value = self.variable;
        self.variable = 0;
        self.shift = 0;
        if matches!(self.state, State::Base) {
            if value != self.base_size {
                return Err(Error::CorruptPack);
            }
            self.state = State::Size;
            return Ok(None);
        }
        if value > self.limits.max_output {
            return Err(Error::LimitExceeded);
        }
        self.result_size = value;
        self.state = State::Opcode;
        Ok(Some(DeltaStatus::Header {
            base_size: self.base_size,
            result_size: value,
        }))
    }

    fn opcode(&mut self, opcode: u8) -> Result<()> {
        if self.instructions == self.limits.max_instructions {
            return Err(Error::LimitExceeded);
        }
        self.instructions += 1;
        self.state = match opcode {
            0 => return Err(Error::CorruptPack),
            1..=127 => {
                if u64::from(opcode) > self.result_size - self.position {
                    return Err(Error::CorruptPack);
                }
                State::Insert(opcode)
            }
            _ => State::Copy {
                flags: opcode,
                index: 0,
                offset: 0,
                length: 0,
            },
        };
        Ok(())
    }

    fn copy(&mut self, offset: u32, length: u32) -> Result<DeltaStatus<'static>> {
        let length = if length == 0 {
            65536
        } else {
            u64::from(length)
        };
        let offset = u64::from(offset);
        if offset + length > self.base_size {
            return Err(Error::CorruptPack);
        }
        self.emit(length)?;
        self.state = State::Opcode;
        Ok(DeltaStatus::Copy { offset, length })
    }

    fn emit(&mut self, length: u64) -> Result<()> {
        if length > self.result_size - self.position {
            return Err(Error::CorruptPack);
        }
        self.position += length;
        Ok(())
    }
}
