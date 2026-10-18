//! Experimental host-side coordination. Native Git is the initial pack codec;
//! this crate does not add receive-pack to the allocator-free library or Worker.
pub mod accounting;
pub mod cost;
pub mod storage;

use sha2::{Digest, Sha256};
use std::io;
use storage::{BlobStore, Chunk, Manifest, StateStore, StoredPack};

pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACK_COMPONENT_BYTES: usize = 128 * 1024 * 1024;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Shared publication logic: immutable compressed data precedes the ref CAS.
/// Runtime-specific adapters implement the same blob/state traits.
pub struct Repository<B, S> {
    pub blobs: B,
    pub state: S,
    pub chunk_bytes: usize,
}

impl<B: BlobStore, S: StateStore> Repository<B, S> {
    pub fn new(blobs: B, state: S) -> Self {
        Self {
            blobs,
            state,
            chunk_bytes: CHUNK_BYTES,
        }
    }

    fn store_chunks(&self, bytes: &[u8]) -> io::Result<Vec<Chunk>> {
        if self.chunk_bytes == 0 {
            return Err(io::Error::other("chunk size must be positive"));
        }
        bytes
            .chunks(self.chunk_bytes)
            .map(|data| {
                let key = format!("chunks/{}", hash(data));
                self.blobs.put(&key, data)?;
                Ok(Chunk {
                    key,
                    len: data.len() as u64,
                })
            })
            .collect()
    }

    pub fn store_pack(
        &self,
        id: String,
        pack: &[u8],
        index: &[u8],
        level: u32,
    ) -> io::Result<StoredPack> {
        if pack.len() > MAX_PACK_COMPONENT_BYTES || index.len() > MAX_PACK_COMPONENT_BYTES {
            return Err(io::Error::other("prototype pack limit exceeded"));
        }
        Ok(StoredPack {
            id,
            pack: self.store_chunks(pack)?,
            index: self.store_chunks(index)?,
            pack_bytes: pack.len() as u64,
            index_bytes: index.len() as u64,
            level,
        })
    }

    pub fn read_chunks(&self, chunks: &[Chunk], expected: u64) -> io::Result<Vec<u8>> {
        if expected > MAX_PACK_COMPONENT_BYTES as u64 {
            return Err(io::Error::other("prototype pack limit exceeded"));
        }
        let mut result = Vec::with_capacity(expected as usize);
        for chunk in chunks {
            let data = self.blobs.get(&chunk.key)?;
            if data.len() as u64 != chunk.len
                || chunk.key != format!("chunks/{}", hash(&data))
                || result.len() as u64 + chunk.len > expected
            {
                return Err(io::Error::other("corrupt chunk"));
            }
            result.extend_from_slice(&data);
        }
        if result.len() as u64 != expected {
            return Err(io::Error::other("incomplete pack"));
        }
        Ok(result)
    }

    pub fn publish(&self, previous: &Manifest, mut next: Manifest) -> io::Result<bool> {
        next.generation = previous
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("generation overflow"))?;
        self.state.compare_and_swap(previous.generation, &next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingBlobs(AtomicUsize);

    impl BlobStore for CountingBlobs {
        fn get(&self, _: &str) -> io::Result<Vec<u8>> {
            panic!("unexpected blob read")
        }

        fn put(&self, _: &str, _: &[u8]) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn delete(&self, _: &str) -> io::Result<()> {
            panic!("unexpected blob deletion")
        }
    }

    struct UnusedState;

    impl StateStore for UnusedState {
        fn load(&self) -> io::Result<Manifest> {
            panic!("unexpected manifest read")
        }

        fn compare_and_swap(&self, _: u64, _: &Manifest) -> io::Result<bool> {
            panic!("unexpected publication")
        }
    }

    #[test]
    fn oversized_pack_or_index_is_rejected_before_any_blob_writes() {
        let oversized = vec![0; MAX_PACK_COMPONENT_BYTES + 1];
        for (pack, index) in [
            (oversized.as_slice(), b"index".as_slice()),
            (b"pack".as_slice(), oversized.as_slice()),
        ] {
            let repository = Repository::new(CountingBlobs::default(), UnusedState);
            let result = repository.store_pack("oversized".into(), pack, index, 0);
            assert_eq!(repository.blobs.0.load(Ordering::Relaxed), 0);
            assert!(result.is_err());
        }
    }
}
