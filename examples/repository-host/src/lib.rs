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
const MAX_PACK_BYTES: u64 = 128 * 1024 * 1024;

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
            bundle: Vec::new(),
            pack_bytes: pack.len() as u64,
            index_bytes: index.len() as u64,
            level,
        })
    }

    /// Store one compressed byte stream, reducing remote operations for small
    /// packs while retaining the same content-addressed chunk size.
    pub fn store_bundled_pack(
        &self,
        id: String,
        pack: &[u8],
        index: &[u8],
        level: u32,
    ) -> io::Result<StoredPack> {
        let total = pack_total(pack.len() as u64, index.len() as u64)?;
        let mut bytes = Vec::with_capacity(total as usize);
        bytes.extend_from_slice(pack);
        bytes.extend_from_slice(index);
        Ok(StoredPack {
            id,
            bundle: self.store_chunks(&bytes)?,
            pack_bytes: pack.len() as u64,
            index_bytes: index.len() as u64,
            level,
            ..StoredPack::default()
        })
    }

    /// Reconstruct both files with one pass over a bundle's chunks. Verify
    /// complete chunk lengths and hashes before using the declared split.
    pub fn read_pack_files(&self, pack: &StoredPack) -> io::Result<(Vec<u8>, Vec<u8>)> {
        let total = pack_total(pack.pack_bytes, pack.index_bytes)?;
        if pack.bundle.is_empty() {
            return Ok((
                self.read_chunks(&pack.pack, pack.pack_bytes)?,
                self.read_chunks(&pack.index, pack.index_bytes)?,
            ));
        }
        if !pack.pack.is_empty() || !pack.index.is_empty() {
            return Err(io::Error::other("mixed pack chunk representations"));
        }
        let mut bytes = self.read_chunks(&pack.bundle, total)?;
        let index = bytes.split_off(pack.pack_bytes as usize);
        Ok((bytes, index))
    }

    pub fn read_chunks(&self, chunks: &[Chunk], expected: u64) -> io::Result<Vec<u8>> {
        if expected > MAX_PACK_BYTES {
            return Err(io::Error::other("prototype pack limit exceeded"));
        }
        if chunks.iter().try_fold(0u64, |total, chunk| {
            (chunk.len != 0).then_some(())?;
            total.checked_add(chunk.len)
        }) != Some(expected)
        {
            return Err(io::Error::other("invalid chunk lengths"));
        }
        let mut result = Vec::with_capacity(expected as usize);
        for chunk in chunks {
            let data = self.blobs.get(&chunk.key)?;
            if data.len() as u64 != chunk.len
                || chunk.key != format!("chunks/{}", hash(&data))
                || chunk.len > expected - result.len() as u64
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

fn pack_total(pack_bytes: u64, index_bytes: u64) -> io::Result<u64> {
    pack_bytes
        .checked_add(index_bytes)
        .filter(|total| *total <= MAX_PACK_BYTES)
        .ok_or_else(|| io::Error::other("prototype pack and index limit exceeded"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct MemoryBlobs {
        bytes: Mutex<BTreeMap<String, Vec<u8>>>,
        gets: AtomicUsize,
        puts: AtomicUsize,
    }

    impl BlobStore for MemoryBlobs {
        fn get(&self, key: &str) -> io::Result<Vec<u8>> {
            self.gets.fetch_add(1, Ordering::Relaxed);
            self.bytes
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
            self.puts.fetch_add(1, Ordering::Relaxed);
            let mut stored = self.bytes.lock().unwrap();
            if stored.get(key).is_some_and(|previous| previous != bytes) {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            stored.insert(key.into(), bytes.to_vec());
            Ok(())
        }

        fn delete(&self, key: &str) -> io::Result<()> {
            self.bytes.lock().unwrap().remove(key);
            Ok(())
        }
    }

    struct UnusedState;

    impl StateStore for UnusedState {
        fn load(&self) -> io::Result<Manifest> {
            unreachable!("file reconstruction does not access state")
        }

        fn compare_and_swap(&self, _: u64, _: &Manifest) -> io::Result<bool> {
            unreachable!("file reconstruction does not publish state")
        }
    }

    #[test]
    fn legacy_and_bundled_pack_files_round_trip() {
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let legacy = repository
            .store_pack("legacy".into(), b"compressed-pack", b"index", 2)
            .unwrap();
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json.get("bundle").is_none());
        let restored: StoredPack = serde_json::from_value(json).unwrap();
        assert!(restored.bundle.is_empty());
        assert_eq!(
            repository.read_pack_files(&restored).unwrap(),
            (b"compressed-pack".to_vec(), b"index".to_vec())
        );
        assert_eq!(repository.blobs.puts.load(Ordering::Relaxed), 2);

        let bundled = repository
            .store_bundled_pack("bundled".into(), b"compressed-pack", b"index", 2)
            .unwrap();
        assert!(bundled.pack.is_empty());
        assert!(bundled.index.is_empty());
        assert_eq!(bundled.bundle.len(), 1);
        assert_eq!(repository.blobs.puts.load(Ordering::Relaxed), 3);
        let restored: StoredPack =
            serde_json::from_slice(&serde_json::to_vec(&bundled).unwrap()).unwrap();
        let gets = repository.blobs.gets.load(Ordering::Relaxed);
        assert_eq!(
            repository.read_pack_files(&restored).unwrap(),
            (b"compressed-pack".to_vec(), b"index".to_vec())
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed) - gets, 1);
        assert_eq!(restored, bundled);
    }

    #[test]
    fn bundled_pack_and_index_share_the_eight_mib_chunk_boundary() {
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let pack = vec![0x51; CHUNK_BYTES - 3];
        let index = b"index bytes";
        let stored = repository
            .store_bundled_pack("boundary".into(), &pack, index, 0)
            .unwrap();
        assert_eq!(stored.bundle.len(), 2);
        assert_eq!(stored.bundle[0].len, CHUNK_BYTES as u64);
        assert_eq!(stored.bundle[1].len, (index.len() - 3) as u64);
        let first = repository.blobs.get(&stored.bundle[0].key).unwrap();
        assert_eq!(&first[pack.len()..], &index[..3]);
        assert_eq!(
            repository.read_pack_files(&stored).unwrap(),
            (pack, index.to_vec())
        );
    }

    #[test]
    fn pack_files_reject_mixed_missing_overflowing_and_corrupt_chunks() {
        let mut repository = Repository::new(MemoryBlobs::default(), UnusedState);
        repository.chunk_bytes = 5;
        let stored = repository
            .store_bundled_pack("checked".into(), b"pack", b"index", 0)
            .unwrap();
        let mut invalid = Vec::new();
        for use_pack in [true, false] {
            let mut mixed = stored.clone();
            if use_pack {
                mixed.pack = stored.bundle.clone();
            } else {
                mixed.index = stored.bundle.clone();
            }
            invalid.push(mixed);
        }
        let mut missing = stored.clone();
        missing.bundle.pop();
        invalid.push(missing);
        let mut missing = stored.clone();
        missing.bundle.clear();
        invalid.push(missing);
        for (pack_bytes, index_bytes) in [
            (u64::MAX, 1),
            (MAX_PACK_BYTES, 1),
            (stored.pack_bytes + 1, stored.index_bytes),
            (stored.pack_bytes, stored.index_bytes - 1),
        ] {
            let mut sizes = stored.clone();
            sizes.pack_bytes = pack_bytes;
            sizes.index_bytes = index_bytes;
            invalid.push(sizes);
        }
        for len in [0, u64::MAX, stored.bundle[0].len + 1] {
            let mut lengths = stored.clone();
            lengths.bundle[0].len = len;
            invalid.push(lengths);
        }
        let gets = repository.blobs.gets.load(Ordering::Relaxed);
        for invalid in invalid {
            assert!(repository.read_pack_files(&invalid).is_err());
        }
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), gets);

        // Corruption wholly inside the index must fail the full chunk hash.
        let last = stored.bundle.last().unwrap();
        repository
            .blobs
            .bytes
            .lock()
            .unwrap()
            .get_mut(&last.key)
            .unwrap()[0] ^= 1;
        assert!(repository.read_pack_files(&stored).is_err());
        // A truncated blob fails even when its manifest still has valid totals.
        repository
            .blobs
            .bytes
            .lock()
            .unwrap()
            .get_mut(&last.key)
            .unwrap()
            .pop();
        assert!(repository.read_pack_files(&stored).is_err());
        let mut missing = stored.clone();
        missing.bundle.last_mut().unwrap().key = "chunks/missing".into();
        assert!(repository.read_pack_files(&missing).is_err());
    }
}
