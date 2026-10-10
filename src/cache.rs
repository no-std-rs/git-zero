use crate::{
    ObjectId, Repository, Result,
    io::{SliceWriter, Write},
    pack,
};

/// Immutable complete packs, keyed by snapshot HEAD and encoder format.
/// Backends own storage, eviction, concurrency and persistence. There is no
/// allocation requirement. Async platform caches can use these keys outside Rust.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    pub head: ObjectId,
    pub encoder_version: u32,
}

pub trait PackCache {
    fn get(&self, key: Key) -> Result<Option<&[u8]>>;
    fn put(&mut self, key: Key, pack: &[u8]) -> Result<()>;
}

pub struct NoCache;

impl PackCache for NoCache {
    fn get(&self, _: Key) -> Result<Option<&[u8]>> {
        Ok(None)
    }
    fn put(&mut self, _: Key, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

/// Hits stream directly. Misses generate into caller scratch before insertion.
/// Cache failures propagate; use `pack::write` when caching is unnecessary.
pub fn write(
    repository: &impl Repository,
    cache: &mut impl PackCache,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<()> {
    let key = Key {
        head: repository.head(),
        encoder_version: 1,
    };
    if let Some(bytes) = cache.get(key)? {
        return output.write_all(bytes);
    }
    let mut pack_buffer = SliceWriter::new(scratch);
    pack::write(repository, &mut pack_buffer)?;
    let bytes = pack_buffer.finish();
    cache.put(key, bytes)?;
    output.write_all(bytes)
}
