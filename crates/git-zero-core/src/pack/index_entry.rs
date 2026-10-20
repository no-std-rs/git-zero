use crate::ObjectId;

/// Metadata for one complete encoded pack entry. Offsets include the pack header;
/// CRCs cover its type/length header and complete zlib stream. An index's entries
/// must be unique and sorted by ID. Fields do not authenticate a foreign pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub id: ObjectId,
    pub offset: u64,
    pub crc32: u32,
}

impl IndexEntry {
    /// Initial value for caller-owned metadata arrays; overwritten during encoding.
    pub const EMPTY: Self = Self {
        id: ObjectId::Sha1([0; 20]),
        offset: 0,
        crc32: 0,
    };
}
