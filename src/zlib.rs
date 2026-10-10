//! A streaming zlib encoder using uncompressed DEFLATE blocks (RFC 1950/1951).
//! This is valid Git compression with bounded memory, but does not reduce size.
use crate::{Result, io::Write};

pub fn write_parts(output: &mut impl Write, parts: &[&[u8]]) -> Result<()> {
    output.write_all(&[0x78, 0x01])?;
    let mut s1 = 1_u32;
    let mut s2 = 0_u32;
    for part in parts {
        for block in part.chunks(u16::MAX as usize) {
            let length = block.len() as u16;
            output.write_all(&[0])?; // BFINAL=0, BTYPE=00, aligned to a byte.
            output.write_all(&length.to_le_bytes())?;
            output.write_all(&(!length).to_le_bytes())?;
            output.write_all(block)?;
            for &byte in block {
                s1 = (s1 + u32::from(byte)) % 65521;
                s2 = (s2 + s1) % 65521;
            }
        }
    }
    output.write_all(&[1, 0, 0, 0xff, 0xff])?; // Empty final block, including for empty input.
    output.write_all(&((s2 << 16) | s1).to_be_bytes())
}
