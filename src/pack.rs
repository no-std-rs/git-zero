use crate::{
    Error, Object, ObjectFormat, ObjectId, ObjectInfo, ObjectReader, Repository, Result, io::Write,
    object::Hash, reader::DEFAULT_READ_BUFFER_SIZE, zlib,
};

mod decode;
pub use decode::{DeltaLimits, Entry, EntryKind, Reader, ResolvedObject, apply_delta, crc32};
mod materialize;
pub use materialize::{
    MaterializeLimits, MaterializedEntry, MaterializedPack, find_materialized, materialize,
};

struct HashWriter<'a, W> {
    output: &'a mut W,
    hash: Hash,
}

impl<W: Write> Write for HashWriter<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.output.write_all(bytes)?;
        self.hash.update(bytes);
        Ok(())
    }
}

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
    write_order(repository, 0..repository.object_count(), scratch, output)
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
    let ordered = indices.windows(2).all(|pair| pair[0] < pair[1]);
    for (position, &index) in indices.iter().enumerate() {
        if index >= repository.object_count() {
            return Err(Error::ObjectNotFound);
        }
        if !ordered && indices[..position].contains(&index) {
            return Err(Error::InvalidObject);
        }
    }
    write_order(repository, indices.iter().copied(), scratch, output)
}

fn write_order(
    repository: &impl Repository,
    order: impl ExactSizeIterator<Item = usize>,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<ObjectId> {
    let count = u32::try_from(order.len()).map_err(|_| Error::Unsupported)?;
    let mut output = HashWriter {
        output,
        hash: repository.object_format().hash(),
    };
    output.write_all(b"PACK")?;
    output.write_all(&2_u32.to_be_bytes())?;
    output.write_all(&count.to_be_bytes())?;
    for index in order {
        if let Some(object) = repository.object(index) {
            write_entry_header(object.info(), &mut output)?;
            zlib::write_parts(&mut output, &[object.data])?;
        } else {
            let mut reader = ObjectReader::new(repository, index)?;
            write_entry_header(reader.info(), &mut output)?;
            zlib::write_reader(&mut output, &[], &mut reader, scratch)?;
        }
    }
    let checksum = output.hash.finish();
    output.output.write_all(checksum.as_bytes())?;
    Ok(checksum)
}

fn write_entry_header(info: ObjectInfo, output: &mut impl Write) -> Result<()> {
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

/// Check the framing and checksum of a downloaded pack. Object/delta validation
/// and indexing are separate responsibilities; this does not decode objects.
pub fn validate(bytes: &[u8]) -> Result<()> {
    validate_with_format(bytes, ObjectFormat::Sha1)
}

pub fn validate_with_format(bytes: &[u8], format: ObjectFormat) -> Result<()> {
    if bytes.len() < 12 + format.id_length()
        || &bytes[..4] != b"PACK"
        || !matches!(&bytes[4..8], [0, 0, 0, 2] | [0, 0, 0, 3])
    {
        return Err(Error::CorruptPack);
    }
    let trailer = bytes.len() - format.id_length();
    let actual = format.digest(&bytes[..trailer]);
    if actual.as_bytes() != &bytes[trailer..] {
        return Err(Error::CorruptPack);
    }
    Ok(())
}
