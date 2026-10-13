mod huffman;

use super::adler::Adler32;
use crate::{Error, Result};
use huffman::{Bits, Huffman, Symbol};

// A missing value retains the reader's partial state for the next input chunk.
macro_rules! ready {
    ($value:expr, $pending:expr) => {
        match $value? {
            Some(value) => value,
            None => return Ok($pending),
        }
    };
}

/// DEFLATE's maximum back-reference distance. Each decoder borrows one window.
pub const WINDOW_SIZE: usize = 32_768;

const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

// RFC 1951 length/distance bases and the number of following extra bits.
// Tables keep parameter decoding independent of the Huffman symbol reader.
const LENGTH_CODES: [(u16, u8); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];
const DISTANCE_CODES: [(u16, u8); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

/// Budgets for one zlib stream, including its decompressed loose-object header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InflateLimits {
    pub max_output: u64,
    /// Before each output byte, enforce
    /// `output <= consumed_input * max_ratio + ratio_slack` using saturating math.
    /// Input counts only bytes consumed from this stream, including its wrapper.
    pub max_ratio: u32,
    /// An explicit allowance for highly compressible prefixes. Zero is permitted.
    pub ratio_slack: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InflateStatus {
    /// All supplied input was consumed; provide more, preserving this decoder.
    NeedInput,
    /// A pending byte needs output space; resubmit any unconsumed input suffix.
    NeedOutput,
    /// The final block and Adler-32 trailer have been validated.
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InflateProgress {
    pub consumed: usize,
    pub written: usize,
    pub status: InflateStatus,
}

/// Incremental zlib decoder with no allocation or dependency on `std`/`alloc`.
///
/// The caller owns a 32 KiB history window, this state (less than 2 KiB), and
/// arbitrarily sized input/output chunks. Stored, fixed and dynamic Huffman
/// blocks are supported. Preset dictionaries return [`Error::Unsupported`].
/// Output is provisional until [`InflateStatus::Finished`]; discard it on error.
/// Inflation does not validate Git object headers, lengths or IDs.
pub struct Inflater<'a> {
    window: &'a mut [u8; WINDOW_SIZE],
    position: usize,
    window_limit: usize,
    limits: InflateLimits,
    bits: Bits,
    symbol: Symbol,
    literal: Huffman<288, 256>,
    distance: Huffman<32, 32>,
    code: Huffman<19, 16>,
    lengths: [u8; 320],
    dynamic: Dynamic,
    state: State,
    final_block: bool,
    total_output: u64,
    adler: Adler32,
    failure: Option<Error>,
}

impl<'a> Inflater<'a> {
    pub fn new(window: &'a mut [u8; WINDOW_SIZE], limits: InflateLimits) -> Self {
        Self {
            window,
            position: 0,
            window_limit: WINDOW_SIZE,
            limits,
            bits: Bits::default(),
            symbol: Symbol::default(),
            literal: Huffman::new(),
            distance: Huffman::new(),
            code: Huffman::new(),
            lengths: [0; 320],
            dynamic: Dynamic::default(),
            state: State::Header,
            final_block: false,
            total_output: 0,
            adler: Adler32::new(),
            failure: None,
        }
    }

    /// Start an independent stream. Old history is inaccessible: each distance
    /// must refer to bytes emitted by the new stream before it can be read.
    pub fn reset(&mut self, limits: InflateLimits) {
        self.position = 0;
        self.window_limit = WINDOW_SIZE;
        self.limits = limits;
        self.bits = Bits::default();
        self.symbol = Symbol::default();
        self.dynamic = Dynamic::default();
        self.state = State::Header;
        self.final_block = false;
        self.total_output = 0;
        self.adler = Adler32::new();
        self.failure = None;
    }

    pub fn total_input(&self) -> u64 {
        self.bits.total
    }

    pub fn total_output(&self) -> u64 {
        self.total_output
    }

    /// Consume only one stream, leaving subsequent pack entries/trailing bytes
    /// untouched. Empty chunks are accepted. `end_of_input` means this slice
    /// contains the last available bytes: a need for further input is truncation,
    /// while a need for more output remains resumable with the unconsumed suffix.
    ///
    /// Errors are terminal until `reset`; output from the failing call may have
    /// been modified and must be discarded. Limits are checked before emitting
    /// each byte and do not depend on input/output chunk boundaries.
    pub fn update(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        end_of_input: bool,
    ) -> Result<InflateProgress> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let mut remaining = input;
        let result = self
            .run(&mut remaining, output)
            .and_then(|(written, status)| {
                if end_of_input && status == InflateStatus::NeedInput {
                    return Err(Error::InvalidZlib);
                }
                Ok(InflateProgress {
                    consumed: input.len() - remaining.len(),
                    written,
                    status,
                })
            });
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn run(&mut self, input: &mut &[u8], output: &mut [u8]) -> Result<(usize, InflateStatus)> {
        let mut written = 0;
        // An unavailable bit/symbol leaves its partial state intact.
        macro_rules! bits {
            ($count:expr) => {
                ready!(
                    self.bits.read($count, input),
                    (written, InflateStatus::NeedInput)
                )
            };
        }
        macro_rules! symbol {
            ($table:ident) => {
                ready!(
                    self.symbol.read(&self.$table, &mut self.bits, input),
                    (written, InflateStatus::NeedInput)
                )
            };
        }
        loop {
            match self.state {
                State::Header => {
                    self.window_limit = zlib_window(bits!(16))?;
                    self.state = State::Block;
                }
                State::Block => {
                    let header = bits!(3);
                    self.begin_block(header)?;
                }
                State::StoredLength => self.state = State::StoredComplement(bits!(16) as u16),
                State::StoredComplement(length) => {
                    if bits!(16) as u16 != !length {
                        return Err(Error::InvalidZlib);
                    }
                    self.state = State::Stored(length);
                }
                State::Stored(0) => self.end_block(),
                State::Stored(remaining) => {
                    let count = self.copy_stored(input, &mut output[written..], remaining);
                    if count != 0 {
                        written += count;
                        continue;
                    }
                    // Preserve the pending byte and failure position even when
                    // a run ends at an output, input or budget boundary.
                    let byte = bits!(8) as u8;
                    self.state = State::Literal {
                        byte,
                        stored_remaining: Some(remaining - 1),
                    };
                }
                State::Dynamic | State::CodeLengths { .. } | State::Lengths | State::Repeat(_) => {
                    if !self.read_dynamic_tables(input)? {
                        return Ok((written, InflateStatus::NeedInput));
                    }
                }
                State::Compressed => {
                    let value = symbol!(literal);
                    self.state = match value {
                        0..=255 => State::Literal {
                            byte: value as u8,
                            stored_remaining: None,
                        },
                        256 => {
                            self.end_block();
                            continue;
                        }
                        257..=285 => State::Length(value - 257),
                        _ => return Err(Error::InvalidZlib),
                    };
                }
                State::Literal {
                    byte,
                    stored_remaining,
                } => {
                    if written == output.len() {
                        return Ok((written, InflateStatus::NeedOutput));
                    }
                    self.emit(byte, &mut output[written])?;
                    written += 1;
                    self.state = stored_remaining.map_or(State::Compressed, State::Stored);
                }
                State::Length(index) => {
                    let (base, extra) = LENGTH_CODES[usize::from(index)];
                    self.state = State::Distance(base + bits!(extra) as u16);
                }
                State::Distance(length) => {
                    let index = symbol!(distance);
                    if usize::from(index) >= DISTANCE_CODES.len() {
                        return Err(Error::InvalidZlib);
                    }
                    self.state = State::DistanceExtra { length, index };
                }
                State::DistanceExtra { length, index } => {
                    let (base, extra) = DISTANCE_CODES[usize::from(index)];
                    let distance = base + bits!(extra) as u16;
                    if usize::from(distance) > self.window_limit
                        || u64::from(distance) > self.total_output
                    {
                        return Err(Error::InvalidZlib);
                    }
                    self.state = State::Copy {
                        remaining: length,
                        distance,
                    };
                }
                State::Copy {
                    remaining,
                    distance,
                } => {
                    if written == output.len() {
                        return Ok((written, InflateStatus::NeedOutput));
                    }
                    written +=
                        self.copy_back_reference(&mut output[written..], remaining, distance)?;
                }
                State::Trailer { read, value } => {
                    let value = (value << 8) | bits!(8);
                    if read == 3 {
                        let actual = self.adler.value();
                        if value != actual {
                            return Err(Error::InvalidZlib);
                        }
                        self.state = State::Done;
                    } else {
                        self.state = State::Trailer {
                            read: read + 1,
                            value,
                        };
                    }
                }
                State::Done => return Ok((written, InflateStatus::Finished)),
            }
        }
    }

    fn begin_block(&mut self, header: u32) -> Result<()> {
        self.final_block = header & 1 != 0;
        self.state = match header >> 1 {
            0 => {
                self.bits.align();
                State::StoredLength
            }
            1 => {
                self.fixed_tables()?;
                State::Compressed
            }
            2 => State::Dynamic,
            _ => return Err(Error::InvalidZlib),
        };
        Ok(())
    }

    /// Advance one dynamic-table phase, retaining partial bits/symbols when
    /// more input is needed. Keeping table construction separate makes the
    /// object-emission loop independent of the code-length grammar.
    fn read_dynamic_tables(&mut self, input: &mut &[u8]) -> Result<bool> {
        macro_rules! bits {
            ($count:expr) => {
                ready!(self.bits.read($count, input), false)
            };
        }
        macro_rules! symbol {
            ($table:ident) => {
                ready!(self.symbol.read(&self.$table, &mut self.bits, input), false)
            };
        }
        match self.state {
            State::Dynamic => {
                let header = bits!(14);
                let literal = (header & 31) as u16 + 257;
                if literal > 286 {
                    return Err(Error::InvalidZlib);
                }
                let distance = ((header >> 5) & 31) as u16 + 1;
                let count = ((header >> 10) & 15) as u8 + 4;
                self.lengths.fill(0);
                self.dynamic = Dynamic {
                    next: 0,
                    literal,
                    total: literal + distance,
                };
                self.state = State::CodeLengths { next: 0, count };
            }
            State::CodeLengths { next, count } => {
                self.lengths[CODE_LENGTH_ORDER[usize::from(next)]] = bits!(3) as u8;
                self.state = if next + 1 == count {
                    self.code.build(&self.lengths[..19], false, false)?;
                    State::Lengths
                } else {
                    State::CodeLengths {
                        next: next + 1,
                        count,
                    }
                };
            }
            State::Lengths => {
                let next = self.dynamic.next;
                let value = symbol!(code);
                if value <= 15 {
                    self.lengths[usize::from(next)] = value as u8;
                    self.finish_lengths(1)?;
                } else {
                    if value == 16 && next == 0 {
                        return Err(Error::InvalidZlib);
                    }
                    self.state = State::Repeat(value);
                }
            }
            State::Repeat(value) => {
                let next = self.dynamic.next;
                let (extra, base, length) = match value {
                    16 => (2, 3, self.lengths[usize::from(next - 1)]),
                    17 => (3, 3, 0),
                    18 => (7, 11, 0),
                    _ => return Err(Error::InvalidZlib),
                };
                let count = bits!(extra) as u16 + base;
                if count > self.dynamic.total - next {
                    return Err(Error::InvalidZlib);
                }
                self.lengths[usize::from(next)..usize::from(next + count)].fill(length);
                self.finish_lengths(count)?;
            }
            _ => unreachable!("only dynamic-table phases enter this helper"),
        }
        Ok(true)
    }

    #[inline]
    fn copy_stored(&mut self, input: &mut &[u8], output: &mut [u8], remaining: u16) -> usize {
        // Each stored byte consumes one input byte before its limit
        // check. With a positive integer ratio, an allowed first
        // byte guarantees the rest of this run's ratio checks.
        let next_input = self.bits.total.saturating_add(1);
        let ratio_limit = next_input
            .saturating_mul(u64::from(self.limits.max_ratio))
            .saturating_add(self.limits.ratio_slack);
        if self.bits.count != 0 || self.total_output >= ratio_limit {
            return 0;
        }
        let budget = if self.limits.max_ratio == 0 {
            self.limits.max_output.min(self.limits.ratio_slack)
        } else {
            self.limits.max_output
        };
        let count = usize::from(remaining)
            .min(input.len())
            .min(output.len())
            .min(WINDOW_SIZE)
            .min(
                budget
                    .saturating_sub(self.total_output)
                    .min(u16::MAX as u64) as usize,
            )
            .min((u64::MAX - self.bits.total).min(u16::MAX as u64) as usize);
        if count == 0 {
            return 0;
        }
        output[..count].copy_from_slice(&input[..count]);
        self.bits.total += count as u64;
        *input = &input[count..];
        self.emit_slice(&output[..count]);
        self.state = State::Stored(remaining - count as u16);
        count
    }

    #[inline]
    fn copy_back_reference(
        &mut self,
        output: &mut [u8],
        remaining: u16,
        distance: u16,
    ) -> Result<usize> {
        let count = usize::from(remaining)
            .min(output.len())
            .min(self.output_budget().min(u16::MAX as u64) as usize);
        if count == 0 {
            return Err(Error::LimitExceeded);
        }
        let target = &mut output[..count];
        let distance = usize::from(distance);
        let position = (self.position + WINDOW_SIZE - distance) & (WINDOW_SIZE - 1);
        let seed = count.min(distance);
        let first = seed.min(WINDOW_SIZE - position);
        target[..first].copy_from_slice(&self.window[position..position + first]);
        target[first..seed].copy_from_slice(&self.window[..seed - first]);
        // Expand overlapping copies from already produced bytes,
        // doubling the initialized prefix on each iteration.
        let mut initialized = seed;
        while initialized < count {
            let next = initialized.min(count - initialized);
            target.copy_within(..next, initialized);
            initialized += next;
        }
        self.emit_slice(target);
        self.state = if remaining == count as u16 {
            State::Compressed
        } else {
            State::Copy {
                remaining: remaining - count as u16,
                distance: distance as u16,
            }
        };
        Ok(count)
    }

    fn output_budget(&self) -> u64 {
        let ratio_limit = self
            .bits
            .total
            .saturating_mul(u64::from(self.limits.max_ratio))
            .saturating_add(self.limits.ratio_slack);
        self.limits
            .max_output
            .min(ratio_limit)
            .saturating_sub(self.total_output)
    }

    fn emit(&mut self, byte: u8, output: &mut u8) -> Result<()> {
        if self.output_budget() == 0 {
            return Err(Error::LimitExceeded);
        }
        *output = byte;
        self.emit_slice(core::slice::from_ref(output));
        Ok(())
    }

    /// Commit an already budgeted run of at most one history window.
    #[inline]
    fn emit_slice(&mut self, bytes: &[u8]) {
        let first = bytes.len().min(WINDOW_SIZE - self.position);
        self.window[self.position..self.position + first].copy_from_slice(&bytes[..first]);
        self.window[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.position = (self.position + bytes.len()) & (WINDOW_SIZE - 1);
        self.total_output += bytes.len() as u64;
        self.adler.update(bytes);
    }

    fn end_block(&mut self) {
        self.state = if self.final_block {
            self.bits.align();
            State::Trailer { read: 0, value: 0 }
        } else {
            State::Block
        };
    }

    fn fixed_tables(&mut self) -> Result<()> {
        self.lengths[..144].fill(8);
        self.lengths[144..256].fill(9);
        self.lengths[256..280].fill(7);
        self.lengths[280..288].fill(8);
        self.lengths[288..].fill(5);
        self.literal.build(&self.lengths[..288], false, false)?;
        self.distance.build(&self.lengths[288..], false, false)
    }

    fn finish_lengths(&mut self, count: u16) -> Result<()> {
        self.dynamic.next += count;
        let Dynamic {
            next,
            literal,
            total,
        } = self.dynamic;
        self.state = if next == total {
            if self.lengths[256] == 0 {
                return Err(Error::InvalidZlib);
            }
            self.literal
                .build(&self.lengths[..usize::from(literal)], false, true)?;
            self.distance.build(
                &self.lengths[usize::from(literal)..usize::from(total)],
                true,
                true,
            )?;
            State::Compressed
        } else {
            State::Lengths
        };
        Ok(())
    }
}

fn zlib_window(header: u32) -> Result<usize> {
    let cmf = header & 255;
    let flg = header >> 8;
    if cmf & 15 != 8 || cmf >> 4 > 7 || (cmf * 256 + flg) % 31 != 0 {
        return Err(Error::InvalidZlib);
    }
    if flg & 32 != 0 {
        return Err(Error::Unsupported);
    }
    Ok(1 << ((cmf >> 4) + 8))
}

/// Counts shared by the resumable phases of one dynamic Huffman header.
#[derive(Clone, Copy, Default)]
struct Dynamic {
    next: u16,
    literal: u16,
    total: u16,
}

#[derive(Clone, Copy)]
enum State {
    Header,
    Block,
    StoredLength,
    StoredComplement(u16),
    Stored(u16),
    Dynamic,
    CodeLengths {
        next: u8,
        count: u8,
    },
    Lengths,
    Repeat(u16),
    Compressed,
    Literal {
        byte: u8,
        stored_remaining: Option<u16>,
    },
    Length(u16),
    Distance(u16),
    DistanceExtra {
        length: u16,
        index: u16,
    },
    Copy {
        remaining: u16,
        distance: u16,
    },
    Trailer {
        read: u8,
        value: u32,
    },
    Done,
}
