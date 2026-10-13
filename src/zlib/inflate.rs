use crate::{Error, Result};

/// DEFLATE's maximum back-reference distance. Each decoder borrows one window.
pub const WINDOW_SIZE: usize = 32_768;

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
    state: State,
    final_block: bool,
    total_output: u64,
    adler_s1: u32,
    adler_s2: u32,
    adler_pending: u16,
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
            state: State::Header,
            final_block: false,
            total_output: 0,
            adler_s1: 1,
            adler_s2: 0,
            adler_pending: 0,
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
        self.state = State::Header;
        self.final_block = false;
        self.total_output = 0;
        self.adler_s1 = 1;
        self.adler_s2 = 0;
        self.adler_pending = 0;
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
                match self.bits.read($count, input)? {
                    Some(value) => value,
                    None => return Ok((written, InflateStatus::NeedInput)),
                }
            };
        }
        macro_rules! symbol {
            ($table:ident) => {
                match self.symbol.read(&self.$table, &mut self.bits, input)? {
                    Some(value) => value,
                    None => return Ok((written, InflateStatus::NeedInput)),
                }
            };
        }
        loop {
            match self.state {
                State::Header => {
                    let header = bits!(16);
                    let cmf = header & 255;
                    let flg = header >> 8;
                    if cmf & 15 != 8 || cmf >> 4 > 7 || (cmf * 256 + flg) % 31 != 0 {
                        return Err(Error::InvalidZlib);
                    }
                    if flg & 32 != 0 {
                        return Err(Error::Unsupported);
                    }
                    self.window_limit = 1 << ((cmf >> 4) + 8);
                    self.state = State::Block;
                }
                State::Block => {
                    let header = bits!(3);
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
                    // Each stored byte consumes one input byte before its limit
                    // check. With a positive integer ratio, an allowed first
                    // byte guarantees the rest of this run's ratio checks.
                    let next_input = self.bits.total.saturating_add(1);
                    let ratio_limit = next_input
                        .saturating_mul(u64::from(self.limits.max_ratio))
                        .saturating_add(self.limits.ratio_slack);
                    if self.bits.count == 0 && self.total_output < ratio_limit {
                        let budget = if self.limits.max_ratio == 0 {
                            self.limits.max_output.min(self.limits.ratio_slack)
                        } else {
                            self.limits.max_output
                        };
                        let count = usize::from(remaining)
                            .min(input.len())
                            .min(output.len() - written)
                            .min(WINDOW_SIZE)
                            .min(
                                budget
                                    .saturating_sub(self.total_output)
                                    .min(u16::MAX as u64) as usize,
                            )
                            .min((u64::MAX - self.bits.total).min(u16::MAX as u64) as usize);
                        if count != 0 {
                            output[written..written + count].copy_from_slice(&input[..count]);
                            self.bits.total += count as u64;
                            *input = &input[count..];
                            self.emit_slice(&output[written..written + count]);
                            written += count;
                            self.state = State::Stored(remaining - count as u16);
                            continue;
                        }
                    }
                    // Preserve the pending byte and failure position even when
                    // a run ends at an output, input or budget boundary.
                    let byte = bits!(8) as u8;
                    self.state = State::Literal {
                        byte,
                        stored_remaining: Some(remaining - 1),
                    };
                }
                State::Dynamic => {
                    let header = bits!(14);
                    let literal = (header & 31) as u16 + 257;
                    if literal > 286 {
                        return Err(Error::InvalidZlib);
                    }
                    let distance = ((header >> 5) & 31) as u16 + 1;
                    let count = ((header >> 10) & 15) as u8 + 4;
                    self.lengths.fill(0);
                    self.state = State::CodeLengths {
                        next: 0,
                        count,
                        literal,
                        total: literal + distance,
                    };
                }
                State::CodeLengths {
                    next,
                    count,
                    literal,
                    total,
                } => {
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    self.lengths[ORDER[usize::from(next)]] = bits!(3) as u8;
                    self.state = if next + 1 == count {
                        self.code.build(&self.lengths[..19], false, false)?;
                        State::Lengths {
                            next: 0,
                            literal,
                            total,
                        }
                    } else {
                        State::CodeLengths {
                            next: next + 1,
                            count,
                            literal,
                            total,
                        }
                    };
                }
                State::Lengths {
                    next,
                    literal,
                    total,
                } => {
                    let value = symbol!(code);
                    if value <= 15 {
                        self.lengths[usize::from(next)] = value as u8;
                        self.finish_lengths(next + 1, literal, total)?;
                    } else {
                        if value == 16 && next == 0 {
                            return Err(Error::InvalidZlib);
                        }
                        self.state = State::Repeat {
                            value,
                            next,
                            literal,
                            total,
                        };
                    }
                }
                State::Repeat {
                    value,
                    next,
                    literal,
                    total,
                } => {
                    let (extra, base, length) = match value {
                        16 => (2, 3, self.lengths[usize::from(next - 1)]),
                        17 => (3, 3, 0),
                        18 => (7, 11, 0),
                        _ => return Err(Error::InvalidZlib),
                    };
                    let count = bits!(extra) as u16 + base;
                    if count > total - next {
                        return Err(Error::InvalidZlib);
                    }
                    self.lengths[usize::from(next)..usize::from(next + count)].fill(length);
                    self.finish_lengths(next + count, literal, total)?;
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
                    const BASE: [u16; 29] = [
                        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59,
                        67, 83, 99, 115, 131, 163, 195, 227, 258,
                    ];
                    let extra = if index < 8 || index == 28 {
                        0
                    } else {
                        (index / 4 - 1) as u8
                    };
                    self.state = State::Distance(BASE[usize::from(index)] + bits!(extra) as u16);
                }
                State::Distance(length) => {
                    let index = symbol!(distance);
                    if index >= 30 {
                        return Err(Error::InvalidZlib);
                    }
                    self.state = State::DistanceExtra { length, index };
                }
                State::DistanceExtra { length, index } => {
                    let extra = if index < 4 { 0 } else { (index / 2 - 1) as u8 };
                    let base = if index < 4 {
                        index + 1
                    } else {
                        (2 + index % 2) * (1 << extra) + 1
                    };
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
                    let count = usize::from(remaining)
                        .min(output.len() - written)
                        .min(self.output_budget().min(u16::MAX as u64) as usize);
                    if count == 0 {
                        return Err(Error::LimitExceeded);
                    }
                    let target = &mut output[written..written + count];
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
                    written += count;
                    self.state = if remaining == count as u16 {
                        State::Compressed
                    } else {
                        State::Copy {
                            remaining: remaining - count as u16,
                            distance: distance as u16,
                        }
                    };
                }
                State::Trailer { read, value } => {
                    let value = (value << 8) | bits!(8);
                    if read == 3 {
                        let actual = ((self.adler_s2 % 65521) << 16) | (self.adler_s1 % 65521);
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
        self.window[self.position] = byte;
        self.position = (self.position + 1) & (WINDOW_SIZE - 1);
        self.total_output += 1;
        self.adler_s1 += u32::from(byte);
        self.adler_s2 += self.adler_s1;
        self.adler_pending += 1;
        if self.adler_pending == 5552 {
            self.adler_s1 %= 65521;
            self.adler_s2 %= 65521;
            self.adler_pending = 0;
        }
        Ok(())
    }

    /// Commit an already budgeted run of at most one history window.
    fn emit_slice(&mut self, bytes: &[u8]) {
        let first = bytes.len().min(WINDOW_SIZE - self.position);
        self.window[self.position..self.position + first].copy_from_slice(&bytes[..first]);
        self.window[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        self.position = (self.position + bytes.len()) & (WINDOW_SIZE - 1);
        self.total_output += bytes.len() as u64;
        let (mut s1, mut s2) = (self.adler_s1, self.adler_s2);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let count = remaining.len().min(5552 - usize::from(self.adler_pending));
            for &byte in &remaining[..count] {
                s1 += u32::from(byte);
                s2 += s1;
            }
            self.adler_pending += count as u16;
            if self.adler_pending == 5552 {
                s1 %= 65521;
                s2 %= 65521;
                self.adler_pending = 0;
            }
            remaining = &remaining[count..];
        }
        self.adler_s1 = s1;
        self.adler_s2 = s2;
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
        for (index, length) in self.lengths[..288].iter_mut().enumerate() {
            *length = match index {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        self.lengths[288..].fill(5);
        self.literal.build(&self.lengths[..288], false, false)?;
        self.distance.build(&self.lengths[288..], false, false)
    }

    fn finish_lengths(&mut self, next: u16, literal: u16, total: u16) -> Result<()> {
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
            State::Lengths {
                next,
                literal,
                total,
            }
        };
        Ok(())
    }
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
        literal: u16,
        total: u16,
    },
    Lengths {
        next: u16,
        literal: u16,
        total: u16,
    },
    Repeat {
        value: u16,
        next: u16,
        literal: u16,
        total: u16,
    },
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

#[derive(Default)]
struct Bits {
    value: u32,
    count: u8,
    total: u64,
}

impl Bits {
    fn read(&mut self, count: u8, input: &mut &[u8]) -> Result<Option<u32>> {
        while self.count < count {
            let Some((&byte, remaining)) = input.split_first() else {
                return Ok(None);
            };
            self.total = self.total.checked_add(1).ok_or(Error::LimitExceeded)?;
            *input = remaining;
            self.value |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.value & ((1 << count) - 1);
        self.value >>= count;
        self.count -= count;
        Ok(Some(value))
    }

    fn align(&mut self) {
        let padding = self.count % 8;
        self.value >>= padding;
        self.count -= padding;
    }
}

/// Small prefix tables handle short codes; canonical counts and sorted symbols
/// handle longer codes in at most 15 bit steps without speculative input reads.
struct Huffman<const N: usize, const FAST: usize> {
    counts: [u16; 16],
    symbols: [u16; N],
    max_bits: u8,
    fast: [u16; FAST],
}

impl<const N: usize, const FAST: usize> Huffman<N, FAST> {
    fn new() -> Self {
        Self {
            counts: [0; 16],
            symbols: [0; N],
            max_bits: 0,
            fast: [0; FAST],
        }
    }

    fn build(&mut self, lengths: &[u8], allow_empty: bool, allow_single: bool) -> Result<()> {
        self.counts.fill(0);
        self.max_bits = 0;
        self.fast.fill(0);
        for &length in lengths {
            if length > 15 {
                return Err(Error::InvalidZlib);
            }
            if length != 0 {
                self.counts[usize::from(length)] += 1;
                self.max_bits = self.max_bits.max(length);
            }
        }
        if self.max_bits == 0 {
            return if allow_empty {
                Ok(())
            } else {
                Err(Error::InvalidZlib)
            };
        }
        let mut left = 1_i32;
        for &count in &self.counts[1..] {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err(Error::InvalidZlib);
            }
        }
        // RFC 1951 permits a single one-bit literal/distance code. The code
        // length alphabet must be complete; an empty distance alphabet is only
        // usable when the block contains no back-references.
        if left != 0 && !(allow_single && self.max_bits == 1 && self.counts[1] == 1) {
            return Err(Error::InvalidZlib);
        }
        let mut offsets = [0_usize; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + usize::from(self.counts[length]);
        }
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let offset = &mut offsets[usize::from(length)];
                self.symbols[*offset] = symbol as u16;
                *offset += 1;
            }
        }
        let (mut code, mut index) = (0_u16, 0_usize);
        for length in 1..=self.max_bits {
            let count = self.counts[usize::from(length)];
            if (1_usize << length) <= FAST {
                for offset in 0..count {
                    let prefix = usize::from((code + offset).reverse_bits() >> (16 - length));
                    let value =
                        (u16::from(length) << 9) | self.symbols[index + usize::from(offset)];
                    for slot in (prefix..FAST).step_by(1 << length) {
                        self.fast[slot] = value;
                    }
                }
            }
            index += usize::from(count);
            // A complete 15-bit tree can exhaust u16 only after its last level.
            code = code.wrapping_add(count).wrapping_mul(2);
        }
        Ok(())
    }
}

struct Symbol {
    code: u32,
    first: u32,
    index: usize,
    length: u8,
}

impl Default for Symbol {
    fn default() -> Self {
        Self {
            code: 0,
            first: 0,
            index: 0,
            length: 1,
        }
    }
}

impl Symbol {
    fn read<const N: usize, const FAST: usize>(
        &mut self,
        table: &Huffman<N, FAST>,
        bits: &mut Bits,
        input: &mut &[u8],
    ) -> Result<Option<u16>> {
        if self.length == 1 && table.max_bits != 0 {
            // Fetch only when the scalar decoder also needs its first bit.
            // Prefetching to fill the table index would alter ratio budgets and
            // could consume bytes belonging to the next pack entry.
            if bits.count == 0 {
                let Some(byte) = bits.read(8, input)? else {
                    return Ok(None);
                };
                bits.value = byte;
                bits.count = 8;
            }
            let value = table.fast[bits.value as usize & (FAST - 1)];
            let length = (value >> 9) as u8;
            if length != 0 && length <= bits.count {
                bits.value >>= length;
                bits.count -= length;
                return Ok(Some(value & 511));
            }
        }
        while self.length <= table.max_bits {
            let Some(bit) = bits.read(1, input)? else {
                return Ok(None);
            };
            self.code |= bit;
            let count = u32::from(table.counts[usize::from(self.length)]);
            if self.code >= self.first && self.code - self.first < count {
                let symbol = table.symbols[self.index + (self.code - self.first) as usize];
                *self = Self::default();
                return Ok(Some(symbol));
            }
            self.index += count as usize;
            self.first = (self.first + count) << 1;
            self.code <<= 1;
            self.length += 1;
        }
        Err(Error::InvalidZlib)
    }
}
