//! Cache raw packs by their exact selected object set.
//!
//! Before every call, including hits, hosts must complete authorization,
//! visible-ref/reachability checks, selection and traversal budgets. ACK-only
//! rounds and shallow-list probes do not use this cache. Generate their control
//! frames separately: cached bytes contain only PACK data.

use crate::{
    Error, ObjectFormat, ObjectId, Repository, Result,
    io::{SliceWriter, Write},
    pack,
};

mod fixed;
mod stored;
pub use fixed::{FixedCache, Statistics};

/// Explicit pack selection. A server's absent selection can mean an ACK-only
/// round or probe, so callers must decide that a pack is required before using
/// this type. `Indices(&[])` deliberately encodes an empty pack.
#[derive(Clone, Copy, Debug)]
pub enum Selection<'a> {
    All,
    Indices(&'a [usize]),
}

impl<'a> Selection<'a> {
    fn indices(self) -> Option<&'a [usize]> {
        match self {
            Self::All => None,
            Self::Indices(indices) => Some(indices),
        }
    }
}

/// Stable, owned identity for a stored, non-delta selected pack. Serialization
/// uses explicit numeric codes and big-endian integers, independent of Rust ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key([u8; 48]);

impl Key {
    /// Require strictly ascending selected indices, as returned by repository
    /// selection. `All` selects every object. IDs are canonicalized in caller
    /// storage, so equivalent sets have the same key across enumeration orders.
    /// Metadata, format, count, indices and duplicate IDs fail before cache I/O.
    pub fn derive(
        repository: &impl Repository,
        selection: Selection<'_>,
        ids: &mut [ObjectId],
    ) -> Result<Self> {
        let indices = selection.indices();
        let count = indices.map_or(repository.object_count(), <[usize]>::len);
        let count32 = u32::try_from(count).map_err(|_| Error::LimitExceeded)?;
        let ids = ids.get_mut(..count).ok_or(Error::BufferTooSmall)?;
        if indices.is_some_and(|indices| indices.windows(2).any(|pair| pair[0] >= pair[1])) {
            return Err(Error::InvalidObject);
        }
        let format = repository.object_format();
        for (position, slot) in ids.iter_mut().enumerate() {
            let index = indices.map_or(position, |indices| indices[position]);
            if index >= repository.object_count() {
                return Err(Error::ObjectNotFound);
            }
            repository.object_info(index)?;
            *slot = repository.object_id(index).ok_or(Error::ObjectNotFound)?;
            if slot.format() != format {
                return Err(Error::InvalidObjectId);
            }
        }
        Self::from_ids(format, count32, ids)
    }

    /// Parse exactly 48 bytes. Unknown schemas, codecs, revisions, formats and
    /// reserved bits are rejected, rather than sharing a future encoder's keys.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; 48] = bytes.try_into().map_err(|_| Error::InvalidRequest)?;
        if &bytes[..5] != b"GZPC\x01"
            || !matches!(bytes[5], 1 | 2)
            || bytes[6..12] != [0, 0, 0, 0, 0, 1]
        {
            return Err(Error::Unsupported);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 48] {
        &self.0
    }
    pub fn object_format(self) -> ObjectFormat {
        if self.0[5] == 1 {
            ObjectFormat::Sha1
        } else {
            ObjectFormat::Sha256
        }
    }
    pub fn object_count(self) -> u32 {
        u32::from_be_bytes(self.0[12..16].try_into().unwrap())
    }

    /// Verify framing, checksums, canonical object IDs and exact unique set
    /// before any bytes reach an output sink. Requires one caller ID per object;
    /// no inflated-body buffer or DEFLATE window is needed for this codec.
    pub fn verify_pack(self, bytes: &[u8], ids: &mut [ObjectId], byte_limit: usize) -> Result<()> {
        if bytes.len() > byte_limit {
            return Err(Error::LimitExceeded);
        }
        let count = usize::try_from(self.object_count()).map_err(|_| Error::LimitExceeded)?;
        let ids = ids.get_mut(..count).ok_or(Error::BufferTooSmall)?;
        stored::read(bytes, self.object_format(), ids)?;
        if Self::from_ids(self.object_format(), self.object_count(), ids)? != self {
            return Err(Error::CorruptPack);
        }
        Ok(())
    }

    fn from_ids(format: ObjectFormat, count: u32, ids: &mut [ObjectId]) -> Result<Self> {
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::InvalidObject);
        }
        let mut bytes = [0; 48];
        bytes[..5].copy_from_slice(b"GZPC\x01");
        bytes[5] = match format {
            ObjectFormat::Sha1 => 1,
            ObjectFormat::Sha256 => 2,
        };
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&count.to_be_bytes());
        let mut hash = ObjectFormat::Sha256.hash();
        hash.update(b"git-zero/selected-pack/v1\0");
        hash.update(&bytes[..16]);
        for id in ids {
            hash.update(id.as_bytes());
        }
        bytes[16..].copy_from_slice(hash.finish().as_bytes());
        Ok(Self(bytes))
    }
}

/// Hosts own persistence, eviction and concurrency. A failed insertion must
/// preserve the old entry. Successful insertion publishes complete immutable
/// bytes atomically. Returned bytes are always verified by `write` before use.
pub trait PackCache {
    fn get(&mut self, key: Key) -> Result<Option<&[u8]>>;
    fn put(&mut self, key: Key, pack: &[u8]) -> Result<()>;
}

impl PackCache for super::NoCache {
    fn get(&mut self, _: Key) -> Result<Option<&[u8]>> {
        Ok(None)
    }
    fn put(&mut self, _: Key, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

/// Caller-owned storage. Hits use IDs and `byte_limit` but do not need pack or
/// read scratch. Misses are bounded by both pack storage and `byte_limit`.
pub struct Buffers<'a> {
    pub ids: &'a mut [ObjectId],
    pub pack: &'a mut [u8],
    pub read: &'a mut [u8],
    pub byte_limit: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Hit,
    Miss,
}

/// Key/preflight errors, backend errors and corruption propagate with no
/// output. A miss is encoded and verified before insertion. Sink errors may
/// leave partial output and a valid inserted entry; discard that output and
/// retry. Key/integrity work is O(pack bytes + n log n), with n caller ID slots;
/// graph selection and authorization happen separately before this function.
pub fn write(
    repository: &impl Repository,
    selection: Selection<'_>,
    cache: &mut impl PackCache,
    buffers: Buffers<'_>,
    output: &mut impl Write,
) -> Result<Outcome> {
    let key = Key::derive(repository, selection, buffers.ids)?;
    if let Some(bytes) = cache.get(key)? {
        key.verify_pack(bytes, buffers.ids, buffers.byte_limit)?;
        output.write_all(bytes)?;
        return Ok(Outcome::Hit);
    }
    let capacity = buffers.pack.len().min(buffers.byte_limit);
    let mut staging = SliceWriter::new(&mut buffers.pack[..capacity]);
    match selection {
        Selection::Indices(indices) => {
            pack::write_selected_with_scratch(repository, indices, buffers.read, &mut staging)?
        }
        Selection::All => pack::write_with_scratch(repository, buffers.read, &mut staging)?,
    };
    let bytes = staging.finish();
    key.verify_pack(bytes, buffers.ids, buffers.byte_limit)?;
    cache.put(key, bytes)?;
    output.write_all(bytes)?;
    Ok(Outcome::Miss)
}
