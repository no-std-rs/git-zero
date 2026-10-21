use crate::{Error, ObjectFormat, ObjectId, ObjectInfo, ObjectKind, Result, zlib::adler::Adler32};

pub(super) fn read(bytes: &[u8], format: ObjectFormat, ids: &mut [ObjectId]) -> Result<()> {
    let end = bytes
        .len()
        .checked_sub(format.id_length())
        .ok_or(Error::CorruptPack)?;
    let (payload, trailer) = bytes.split_at(end);
    let mut hash = format.checked_hash();
    hash.update(payload);
    if hash.finish().map_err(|_| Error::CorruptPack)?.as_bytes() != trailer {
        return Err(Error::CorruptPack);
    }
    let mut input = payload;
    if take(&mut input, 8)? != b"PACK\0\0\0\x02"
        || u32::from_be_bytes(take(&mut input, 4)?.try_into().unwrap()) as u64 != ids.len() as u64
    {
        return Err(Error::CorruptPack);
    }
    for id in ids {
        *id = object(&mut input, format)?;
    }
    if !input.is_empty() {
        return Err(Error::CorruptPack);
    }
    Ok(())
}

fn object(input: &mut &[u8], format: ObjectFormat) -> Result<ObjectId> {
    let info = entry(input)?;
    let mut hash = format.checked_hash();
    hash.update(info.header(&mut [0; 32])?);
    let zlib = take(input, 2)?;
    if zlib[0] & 15 != 8
        || zlib[0] >> 4 > 7
        || zlib[1] & 32 != 0
        || u16::from_be_bytes(zlib.try_into().unwrap()) % 31 != 0
    {
        return Err(Error::InvalidZlib);
    }
    let mut adler = Adler32::new();
    let mut remaining = info.size;
    loop {
        let flags = take(input, 1)?[0];
        if flags & 6 != 0 {
            return Err(Error::Unsupported);
        }
        let lengths = take(input, 4)?;
        let length = u16::from_le_bytes(lengths[..2].try_into().unwrap());
        if length != !u16::from_le_bytes(lengths[2..].try_into().unwrap()) {
            return Err(Error::InvalidZlib);
        }
        remaining = remaining
            .checked_sub(u64::from(length))
            .ok_or(Error::CorruptPack)?;
        let body = take(input, usize::from(length))?;
        hash.update(body);
        adler.update(body);
        if flags & 1 != 0 {
            break;
        }
    }
    if remaining != 0 || u32::from_be_bytes(take(input, 4)?.try_into().unwrap()) != adler.value() {
        return Err(Error::CorruptPack);
    }
    hash.finish().map_err(|_| Error::CorruptPack)
}

fn entry(input: &mut &[u8]) -> Result<ObjectInfo> {
    let first = take(input, 1)?[0];
    let kind = match (first >> 4) & 7 {
        1 => ObjectKind::Commit,
        2 => ObjectKind::Tree,
        3 => ObjectKind::Blob,
        4 => ObjectKind::Tag,
        _ => return Err(Error::Unsupported),
    };
    let mut size = u64::from(first & 15);
    let mut byte = first;
    let mut shift = 4;
    while byte & 128 != 0 {
        byte = take(input, 1)?[0];
        let bits = u64::from(byte & 127);
        if shift >= 64 || bits > u64::MAX >> shift {
            return Err(Error::CorruptPack);
        }
        size |= bits << shift;
        shift += 7;
    }
    Ok(ObjectInfo { kind, size })
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    let bytes = input.get(..count).ok_or(Error::CorruptPack)?;
    *input = &input[count..];
    Ok(bytes)
}
