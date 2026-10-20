#[cfg(feature = "read")]
use crate::{Error, ObjectFormat, Result};

#[cfg(any(feature = "read", feature = "write"))]
mod crc;
#[cfg(any(feature = "read", feature = "write"))]
pub use crc::crc32;

#[cfg(any(feature = "read", feature = "write"))]
mod index_entry;
#[cfg(any(feature = "read", feature = "write"))]
pub use index_entry::IndexEntry;

#[cfg(feature = "read")]
mod index_lookup;
#[cfg(feature = "read")]
pub use index_lookup::{IndexHeader, IndexLookup, IndexRange, LookupStatus};

#[cfg(feature = "read")]
mod decode;
#[cfg(feature = "read")]
pub use decode::{DeltaLimits, Entry, EntryHeader, EntryKind, Reader, ResolvedObject, apply_delta};

#[cfg(feature = "read")]
mod delta;
#[cfg(feature = "read")]
pub use delta::{DeltaDecoder, DeltaProgress, DeltaStatus};

#[cfg(feature = "read")]
mod range;
#[cfg(feature = "read")]
pub use range::{
    ObjectKey, ObjectRange, RangeFrame, RangeInput, RangeLimits, RangeObject, RangeProgress,
    RangeRepresentation, RangeRequest, RangeResolver,
};

#[cfg(feature = "read")]
mod receive;
#[cfg(feature = "read")]
pub use receive::{DecodeLimits, DecodeProgress, DecodeStatus, Decoder, EntryDecoder};

#[cfg(feature = "read")]
mod entry_range;
#[cfg(feature = "read")]
pub use entry_range::{ByteRange, EntryRangeDecoder, EntryRangeProgress, EntryRangeStatus};

#[cfg(feature = "read")]
mod materialize;
#[cfg(feature = "read")]
pub use materialize::{
    MaterializeLimits, MaterializedEntry, MaterializedPack, find_materialized, materialize,
};

#[cfg(feature = "write")]
mod index;
#[cfg(feature = "write")]
pub use index::{write_index, write_indexed_with_scratch};

#[cfg(feature = "write")]
mod encode;
#[cfg(feature = "write")]
mod stream;
#[cfg(feature = "write")]
pub use encode::{
    write, write_loose, write_loose_with_scratch, write_selected_with_scratch, write_with_scratch,
};
#[cfg(feature = "write")]
pub use stream::{Encoder, LooseEncoder};

#[cfg(feature = "write")]
mod reuse;
#[cfg(feature = "write")]
pub use reuse::{CompressedEntry, CompressedKind, PackComposer};

/// Check the framing and checksum of a downloaded pack. Object/delta validation
/// and indexing are separate responsibilities; this does not decode objects.
#[cfg(feature = "read")]
pub fn validate(bytes: &[u8]) -> Result<()> {
    validate_with_format(bytes, ObjectFormat::Sha1)
}

#[cfg(feature = "read")]
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
