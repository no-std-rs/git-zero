//! Bit input and canonical Huffman decoding shared by the inflater phases.
use crate::{Error, Result};

#[derive(Default)]
pub(super) struct Bits {
    value: u32,
    pub(super) count: u8,
    pub(super) total: u64,
}

impl Bits {
    pub(super) fn read(&mut self, count: u8, input: &mut &[u8]) -> Result<Option<u32>> {
        while self.count < count {
            if !self.refill(input)? {
                return Ok(None);
            }
        }
        let value = self.value & ((1 << count) - 1);
        self.value >>= count;
        self.count -= count;
        Ok(Some(value))
    }

    #[inline]
    fn refill(&mut self, input: &mut &[u8]) -> Result<bool> {
        let Some((&byte, remaining)) = input.split_first() else {
            return Ok(false);
        };
        self.total = self.total.checked_add(1).ok_or(Error::LimitExceeded)?;
        *input = remaining;
        self.value |= u32::from(byte) << self.count;
        self.count += 8;
        Ok(true)
    }

    pub(super) fn align(&mut self) {
        let padding = self.count % 8;
        self.value >>= padding;
        self.count -= padding;
    }
}

/// Small prefix tables handle short codes; canonical counts and sorted symbols
/// handle longer codes in at most 15 bit steps without speculative input reads.
pub(super) struct Huffman<const N: usize, const FAST: usize> {
    counts: [u16; 16],
    symbols: [u16; N],
    max_bits: u8,
    fast: [u16; FAST],
}

impl<const N: usize, const FAST: usize> Huffman<N, FAST> {
    pub(super) fn new() -> Self {
        Self {
            counts: [0; 16],
            symbols: [0; N],
            max_bits: 0,
            fast: [0; FAST],
        }
    }

    pub(super) fn build(
        &mut self,
        lengths: &[u8],
        allow_empty: bool,
        allow_single: bool,
    ) -> Result<()> {
        self.counts.fill(0);
        self.max_bits = 0;
        self.fast.fill(0);
        for &length in lengths {
            let count = self
                .counts
                .get_mut(usize::from(length))
                .ok_or(Error::InvalidZlib)?;
            *count += 1;
            self.max_bits = self.max_bits.max(length);
        }
        if self.max_bits == 0 {
            return if allow_empty {
                Ok(())
            } else {
                Err(Error::InvalidZlib)
            };
        }
        let mut left = 1_u32;
        for &count in &self.counts[1..] {
            left = (left * 2)
                .checked_sub(u32::from(count))
                .ok_or(Error::InvalidZlib)?;
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
        // Longer codes use the canonical decoder and need no prefix-table work.
        let fast_bits = self.max_bits.min(FAST.trailing_zeros() as u8);
        for length in 1..=fast_bits {
            let count = self.counts[usize::from(length)];
            for offset in 0..count {
                let prefix = usize::from((code + offset).reverse_bits() >> (16 - length));
                let value = (u16::from(length) << 9) | self.symbols[index + usize::from(offset)];
                for slot in (prefix..FAST).step_by(1 << length) {
                    self.fast[slot] = value;
                }
            }
            index += usize::from(count);
            // A complete 15-bit tree can exhaust u16 only after its last level.
            code = code.wrapping_add(count).wrapping_mul(2);
        }
        Ok(())
    }
}

pub(super) struct Symbol {
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
    pub(super) fn read<const N: usize, const FAST: usize>(
        &mut self,
        table: &Huffman<N, FAST>,
        bits: &mut Bits,
        input: &mut &[u8],
    ) -> Result<Option<u16>> {
        if self.length == 1 && table.max_bits != 0 {
            // Fetch only when the scalar decoder also needs its first bit.
            // Prefetching to fill the table index would alter ratio budgets and
            // could consume bytes belonging to the next pack entry.
            if bits.count == 0 && !bits.refill(input)? {
                return Ok(None);
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
            // A code below `first` wraps beyond the small symbol count.
            let offset = self.code.wrapping_sub(self.first);
            if offset < count {
                let symbol = table.symbols[self.index + offset as usize];
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
