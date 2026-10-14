//! Stored-block zlib encoding with bounded scratch.
use super::adler::Adler32;
use crate::{Error, ObjectReader, Repository, Result, io::Write};

pub fn write_parts(output: &mut impl Write, parts: &[&[u8]]) -> Result<()> {
    let mut encoder = Encoder::new(output)?;
    for part in parts {
        encoder.write_part(part)?;
    }
    encoder.finish()
}

/// Encode an optional loose-object header and a body, keeping block boundaries
/// identical to `write_parts` regardless of scratch size or provider short reads.
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

struct Encoder<'a, W> {
    output: &'a mut W,
    adler: Adler32,
}

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
