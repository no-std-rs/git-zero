use crate::{Error, Result, io::Write};

pub const MAX_LENGTH: usize = 65520;

pub fn write(output: &mut impl Write, data: &[u8]) -> Result<()> {
    write_parts(output, &[data])
}

/// Write one packet from borrowed parts without a contiguous scratch buffer.
pub fn write_parts(output: &mut impl Write, parts: &[&[u8]]) -> Result<()> {
    let length = parts
        .iter()
        .try_fold(4_usize, |length, part| length.checked_add(part.len()))
        .filter(|&n| n <= MAX_LENGTH)
        .ok_or(Error::InvalidPacket)?;
    let digits = b"0123456789abcdef";
    output.write_all(&[
        digits[(length >> 12) & 15],
        digits[(length >> 8) & 15],
        digits[(length >> 4) & 15],
        digits[length & 15],
    ])?;
    for part in parts {
        output.write_all(part)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Packet<'a> {
    Data(&'a [u8]),
    Flush,
}

pub fn read<'a>(input: &mut &'a [u8]) -> Result<Option<Packet<'a>>> {
    if input.is_empty() {
        return Ok(None);
    }
    let prefix = input.get(..4).ok_or(Error::InvalidPacket)?;
    let prefix = core::str::from_utf8(prefix).map_err(|_| Error::InvalidPacket)?;
    let length = usize::from_str_radix(prefix, 16).map_err(|_| Error::InvalidPacket)?;
    if length == 0 {
        *input = &input[4..];
        return Ok(Some(Packet::Flush));
    }
    if !(4..=MAX_LENGTH).contains(&length) {
        return Err(Error::InvalidPacket);
    }
    let bytes = input.get(4..length).ok_or(Error::InvalidPacket)?;
    *input = &input[length..];
    Ok(Some(Packet::Data(bytes)))
}
