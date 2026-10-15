//! Single-member RFC 1952 decoding for bounded HTTP upload bodies.
use super::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE};
use crate::{Error, Result, pack::crc32};

/// Decode a complete single gzip member into caller-owned storage. Validate
/// optional header fields/checksum, raw DEFLATE, CRC32 and ISIZE. Trailing data
/// and concatenated members are rejected. All output is provisional on error.
pub fn decode_gzip(
    input: &[u8],
    output: &mut [u8],
    window: &mut [u8; WINDOW_SIZE],
    limits: InflateLimits,
) -> Result<usize> {
    let header = input.get(..10).ok_or(Error::InvalidZlib)?;
    if header[..3] != [31, 139, 8] || header[3] & 0xe0 != 0 {
        return Err(Error::InvalidZlib);
    }
    let flags = header[3];
    let mut offset = 10;
    if flags & 4 != 0 {
        let length = input.get(offset..offset + 2).ok_or(Error::InvalidZlib)?;
        offset += 2 + usize::from(u16::from_le_bytes([length[0], length[1]]));
        input.get(..offset).ok_or(Error::InvalidZlib)?;
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            offset += input
                .get(offset..)
                .ok_or(Error::InvalidZlib)?
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::InvalidZlib)?
                + 1;
        }
    }
    if flags & 2 != 0 {
        let checksum = input.get(offset..offset + 2).ok_or(Error::InvalidZlib)?;
        if u16::from_le_bytes([checksum[0], checksum[1]]) != crc32(&input[..offset]) as u16 {
            return Err(Error::InvalidZlib);
        }
        offset += 2;
    }
    let mut decoder = Inflater::new_raw(window, limits);
    let progress = decoder.update(input.get(offset..).ok_or(Error::InvalidZlib)?, output, true)?;
    if progress.status != InflateStatus::Finished {
        return Err(Error::BufferTooSmall);
    }
    let trailer = input
        .get(offset + progress.consumed..)
        .ok_or(Error::InvalidZlib)?;
    if trailer.len() != 8
        || trailer[..4] != crc32(&output[..progress.written]).to_le_bytes()
        || trailer[4..] != (progress.written as u32).to_le_bytes()
    {
        return Err(Error::InvalidZlib);
    }
    Ok(progress.written)
}
