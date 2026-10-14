#[cfg(feature = "read")]
use crate::{Error, Result};
#[cfg(feature = "read")]
use sha1::{Digest, Sha1};

#[cfg(feature = "read")]
mod decode;
#[cfg(feature = "read")]
pub use decode::{DeltaLimits, Entry, EntryKind, Reader, ResolvedObject, apply_delta, crc32};

#[cfg(feature = "write")]
mod encode;
#[cfg(feature = "write")]
pub use encode::{
    write, write_loose, write_loose_with_scratch, write_selected_with_scratch, write_with_scratch,
};

/// Check the framing and checksum of a downloaded pack. Object/delta validation
/// and indexing are separate responsibilities; this does not decode objects.
#[cfg(feature = "read")]
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
