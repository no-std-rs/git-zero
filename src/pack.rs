use crate::{Error, Object, ObjectId, Repository, Result, io::Write, zlib};
use sha1::{Digest, Sha1};

struct HashWriter<'a, W> {
    output: &'a mut W,
    hash: Sha1,
}

impl<W: Write> Write for HashWriter<'_, W> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.output.write_all(bytes)?;
        self.hash.update(bytes);
        Ok(())
    }
}

/// Generate a complete version-2 pack with no deltas, without buffering the pack.
pub fn write(repository: &impl Repository, output: &mut impl Write) -> Result<ObjectId> {
    let count = u32::try_from(repository.object_count()).map_err(|_| Error::Unsupported)?;
    let mut output = HashWriter {
        output,
        hash: Sha1::new(),
    };
    output.write_all(b"PACK")?;
    output.write_all(&2_u32.to_be_bytes())?;
    output.write_all(&count.to_be_bytes())?;
    for index in 0..repository.object_count() {
        let object = repository.object(index).ok_or(Error::ObjectNotFound)?;
        let mut size = object.data.len();
        let mut byte = ((object.kind as u8) << 4) | (size as u8 & 15);
        size >>= 4;
        loop {
            output.write_all(&[byte | if size > 0 { 128 } else { 0 }])?;
            if size == 0 {
                break;
            }
            byte = size as u8 & 127;
            size >>= 7;
        }
        zlib::write_parts(&mut output, &[object.data])?;
    }
    let checksum = ObjectId(output.hash.finalize().into());
    output.output.write_all(&checksum.0)?;
    Ok(checksum)
}

pub fn write_loose(object: Object<'_>, output: &mut impl Write) -> Result<()> {
    let mut storage = [0; 32];
    zlib::write_parts(output, &[object.header(&mut storage)?, object.data])
}

/// Check the framing and checksum of a downloaded pack. Object/delta validation
/// and indexing are separate responsibilities; this does not decode objects.
pub fn validate(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 32
        || &bytes[..4] != b"PACK"
        || !matches!(&bytes[4..8], [0, 0, 0, 2] | [0, 0, 0, 3])
    {
        return Err(Error::CorruptPack);
    }
    let trailer = bytes.len() - 20;
    let actual = Sha1::digest(&bytes[..trailer]);
    if actual[..] != bytes[trailer..] {
        return Err(Error::CorruptPack);
    }
    Ok(())
}
