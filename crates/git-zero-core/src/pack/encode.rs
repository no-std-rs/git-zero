//! Streaming pack and loose-object encoding.
use crate::{
    Error, Object, ObjectId, ObjectInfo, ObjectReader, Repository, Result, io::Write,
    reader::DEFAULT_READ_BUFFER_SIZE, zlib,
};

/// Generate a complete version-2 pack with no deltas, without buffering the pack.
/// Reserves 1 KiB of read scratch on the stack for streaming-only providers.
pub fn write(repository: &impl Repository, output: &mut impl Write) -> Result<ObjectId> {
    write_with_scratch(repository, &mut [0; DEFAULT_READ_BUFFER_SIZE], output)
}

/// Stream borrowed, file-backed, or generated objects using caller-owned scratch.
/// A nonempty buffer suffices for any body size; borrowed bodies need no scratch.
/// Output is independent of the buffer size, preserving pack/cache identities.
/// Errors invalidate partial output. Scratch is reused for each object; encoder
/// and hash state are inline and independent of object/pack sizes.
pub fn write_with_scratch(
    repository: &impl Repository,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<ObjectId> {
    write_encoder(repository, None, scratch, output)
}

/// Write exactly the chosen object indices, in caller-specified order. Indices
/// must be valid and unique; violations fail before any output. Use a validated
/// repository's `select_objects` result for an exact reachable closure. Encoder
/// and read budgets match `write_with_scratch`; no pack-sized buffer is needed.
pub fn write_selected_with_scratch(
    repository: &impl Repository,
    indices: &[usize],
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<ObjectId> {
    write_encoder(repository, Some(indices), scratch, output)
}

pub(super) fn validate_order(repository: &impl Repository, indices: &[usize]) -> Result<()> {
    let ordered = indices.windows(2).all(|pair| pair[0] < pair[1]);
    for (position, &index) in indices.iter().enumerate() {
        if index >= repository.object_count() {
            return Err(Error::ObjectNotFound);
        }
        if !ordered && indices[..position].contains(&index) {
            return Err(Error::InvalidObject);
        }
    }
    Ok(())
}

fn write_encoder(
    repository: &impl Repository,
    indices: Option<&[usize]>,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<ObjectId> {
    let mut encoder = super::Encoder::new(repository, indices)?;
    if scratch.is_empty() {
        // Preserve the historical scratchless borrowed-object API. Generated
        // nonempty bodies still require caller-provided read storage.
        let count = indices.map_or(repository.object_count(), <[usize]>::len);
        for position in 0..count {
            let index = indices.map_or(position, |indices| indices[position]);
            if repository.object(index).is_none() && repository.object_info(index)?.size != 0 {
                return Err(Error::BufferTooSmall);
            }
        }
        return drain_encoder(
            repository,
            &mut encoder,
            &mut [0; DEFAULT_READ_BUFFER_SIZE],
            output,
        );
    }
    drain_encoder(repository, &mut encoder, scratch, output)
}

fn drain_encoder(
    repository: &impl Repository,
    encoder: &mut super::Encoder<'_>,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<ObjectId> {
    loop {
        let count = encoder.read(repository, scratch)?;
        if count == 0 {
            return encoder.checksum().ok_or(Error::InvalidObject);
        }
        output.write_all(&scratch[..count])?;
    }
}

pub(super) fn write_entry_header(info: ObjectInfo, output: &mut impl Write) -> Result<()> {
    let mut size = info.size;
    let mut byte = ((info.kind as u8) << 4) | (size as u8 & 15);
    size >>= 4;
    loop {
        output.write_all(&[byte | if size > 0 { 128 } else { 0 }])?;
        if size == 0 {
            return Ok(());
        }
        byte = size as u8 & 127;
        size >>= 7;
    }
}

pub fn write_loose(object: Object<'_>, output: &mut impl Write) -> Result<()> {
    let mut storage = [0; 32];
    zlib::write_parts(output, &[object.header(&mut storage)?, object.data])
}

/// Stream a loose object by index. Borrowed bodies use `write_loose` directly;
/// other providers need nonempty scratch for nonempty bodies. Output is identical
/// for every scratch size. Discard partial output on error.
pub fn write_loose_with_scratch(
    repository: &impl Repository,
    index: usize,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<()> {
    if let Some(object) = repository.object(index) {
        return write_loose(object, output);
    }
    let mut reader = ObjectReader::new(repository, index)?;
    let mut header = [0; 32];
    let header = reader.info().header(&mut header)?;
    zlib::write_reader(output, header, &mut reader, scratch)
}
