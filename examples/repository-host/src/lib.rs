//! Experimental host-side coordination. Native Git is the initial pack codec;
//! this crate does not add receive-pack to the allocator-free library or Worker.
pub mod accounting;
pub mod benchmark_cost;
pub mod cloudflare;
pub mod cost;
pub mod http;
pub mod native;
pub mod objects;
mod packed;
pub mod storage;

use sha2::{Digest, Sha256};
use std::io;
use storage::{BlobStore, Chunk, Manifest, StateStore, StoredPack};

pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;

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
        if expected > 128 * 1024 * 1024 {
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
