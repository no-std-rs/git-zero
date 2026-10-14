//! Allocation-free streaming zlib encoding and inflation (RFC 1950/1951).
//! The encoder uses stored blocks; the decoder also accepts fixed and dynamic
//! Huffman blocks, with caller-owned history and explicit expansion budgets.
//! The decoder is implemented from [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
//! and [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951).
//!
//! ```
//! # #[cfg(feature = "read")] {
//! use git_zero_core::{io::{SliceWriter, Write}, zlib::{Inflater, InflateLimits, InflateStatus, WINDOW_SIZE}};
//! # fn example() -> git_zero_core::Result<()> {
//! let mut input: &[u8] = &[120, 156, 203, 72, 205, 201, 201, 7, 0, 6, 44, 2, 21];
//! let mut window = [0; WINDOW_SIZE];
//! let limits = InflateLimits { max_output: 5, max_ratio: 16, ratio_slack: 0 };
//! let mut decoder = Inflater::new(&mut window, limits);
//! let mut scratch = [0; 2];
//! let mut storage = [0; 5];
//! let mut staged = SliceWriter::new(&mut storage);
//! loop {
//!     let progress = decoder.update(input, &mut scratch, true)?;
//!     staged.write_all(&scratch[..progress.written])?;
//!     input = &input[progress.consumed..];
//!     if progress.status == InflateStatus::Finished { break; }
//! }
//! assert_eq!(staged.as_bytes(), b"hello");
//! assert!(input.is_empty());
//! # Ok(()) }
//! # example().unwrap();
//! # }
//! ```
mod adler;
#[cfg(feature = "read")]
mod inflate;
#[cfg(feature = "read")]
pub use inflate::{InflateLimits, InflateProgress, InflateStatus, Inflater, WINDOW_SIZE};

#[cfg(feature = "write")]
use crate::{Error, ObjectReader, Repository, Result, io::Write};
#[cfg(feature = "write")]
use adler::Adler32;

#[cfg(feature = "write")]
pub fn write_parts(output: &mut impl Write, parts: &[&[u8]]) -> Result<()> {
    let mut encoder = Encoder::new(output)?;
    for part in parts {
        encoder.write_part(part)?;
    }
    encoder.finish()
}

/// Encode an optional loose-object header and a body, keeping block boundaries
/// identical to `write_parts` regardless of scratch size or provider short reads.
#[cfg(feature = "write")]
pub(crate) fn write_reader<R: Repository + ?Sized>(
    output: &mut impl Write,
    header: &[u8],
    reader: &mut ObjectReader<'_, R>,
    scratch: &mut [u8],
) -> Result<()> {
    if reader.remaining() != 0 && scratch.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let mut encoder = Encoder::new(output)?;
    encoder.write_part(header)?;
    while reader.remaining() != 0 {
        let length = reader.remaining().min(u64::from(u16::MAX)) as u16;
        encoder.block_header(length)?;
        let mut remaining = usize::from(length);
        while remaining != 0 {
            let capacity = remaining.min(scratch.len());
            let length = reader.read(&mut scratch[..capacity])?;
            encoder.write_bytes(&scratch[..length])?;
            remaining -= length;
        }
    }
    encoder.finish()
}

#[cfg(feature = "write")]
struct Encoder<'a, W> {
    output: &'a mut W,
    adler: Adler32,
}

#[cfg(feature = "write")]
impl<'a, W: Write> Encoder<'a, W> {
    fn new(output: &'a mut W) -> Result<Self> {
        output.write_all(&[0x78, 0x01])?;
        Ok(Self {
            output,
            adler: Adler32::new(),
        })
    }

    fn block_header(&mut self, length: u16) -> Result<()> {
        self.output.write_all(&[0])?; // BFINAL=0, BTYPE=00, byte-aligned.
        self.output.write_all(&length.to_le_bytes())?;
        self.output.write_all(&(!length).to_le_bytes())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.output.write_all(bytes)?;
        self.adler.update(bytes);
        Ok(())
    }

    fn write_part(&mut self, bytes: &[u8]) -> Result<()> {
        for block in bytes.chunks(u16::MAX as usize) {
            self.block_header(block.len() as u16)?;
            self.write_bytes(block)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        // Empty final block, including for empty input.
        self.output.write_all(&[1, 0, 0, 0xff, 0xff])?;
        self.output.write_all(&self.adler.value().to_be_bytes())
    }
}
