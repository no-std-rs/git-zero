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
use storage::{
    BlobStore, Chunk, Manifest, RetirementOutcome, RetirementReceipt, RetiringCas, StateStore,
    StoredPack,
};

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

    /// Validate every representation before reading, then verify the whole
    /// snapshot before its caller materializes any disposable native files.
    /// Flattening across packs lets a remote store bound its own read batches.
    /// Chunk payloads and reconstructed file payloads each total at most
    /// 128 MiB and remain transient; no blob cache survives this call.
    pub fn read_snapshot_files(&self, manifest: &Manifest) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let chunks = snapshot_chunks(manifest)?;
        let data = self.blobs.get_many_verified(&chunks)?.into_data(&chunks)?;
        let mut data = data.into_iter();
        let mut files = Vec::with_capacity(manifest.packs.len());
        for pack in &manifest.packs {
            let mut read = |count: usize, size: u64| {
                if count == 0 {
                    return Vec::new();
                }
                let mut file = data.next().expect("verified chunk count");
                file.reserve_exact(size as usize - file.len());
                for chunk in data.by_ref().take(count - 1) {
                    file.extend_from_slice(&chunk);
                }
                file
            };
            files.push(if pack.bundle.is_empty() {
                (
                    read(pack.pack.len(), pack.pack_bytes),
                    read(pack.index.len(), pack.index_bytes),
                )
            } else {
                let mut bytes = read(pack.bundle.len(), pack.pack_bytes + pack.index_bytes);
                let index = bytes.split_off(pack.pack_bytes as usize);
                (bytes, index)
            });
        }
        Ok(files)
    }

    pub fn read_chunks(&self, chunks: &[Chunk], expected: u64) -> io::Result<Vec<u8>> {
        if expected > MAX_PACK_BYTES {
            return Err(io::Error::other("prototype pack limit exceeded"));
        }
        validate_chunks(chunks, expected)?;
        let mut result = Vec::with_capacity(expected as usize);
        for chunk in chunks {
            let data = self.blobs.get(&chunk.key)?;
            verify_chunk(chunk, &data)?;
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

    /// Publish durably, then retire only this generation's non-live debt.
    /// Stores without an advertised fused capability use the original CAS and
    /// scalar deletion path. An attempted fused operation never retries CAS.
    /// The caller excludes older readers and concurrent writers until return.
    /// Durable debt stays complete; only the caller's in-memory view may clear
    /// confirmed outcomes, so a lost reply remains safe to recover and retry.
    pub fn publish_retiring(
        &self,
        previous: &Manifest,
        mut next: Manifest,
    ) -> io::Result<RetiringCas> {
        next.generation = previous
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("generation overflow"))?;
        match self
            .state
            .try_compare_and_swap_retiring(previous.generation, &next)?
        {
            RetiringCas::Unsupported => {
                if !self.state.compare_and_swap(previous.generation, &next)? {
                    return Ok(RetiringCas::Stale);
                }
                let outcomes = next
                    .retirement_keys()
                    .into_iter()
                    .map(|key| RetirementOutcome {
                        key: key.into(),
                        deleted: self.blobs.delete(key).is_ok(),
                    })
                    .collect();
                Ok(RetiringCas::Published(RetirementReceipt {
                    generation: next.generation,
                    outcomes,
                }))
            }
            result => Ok(result),
        }
    }
}

/// The complete metadata checks also apply when an operation needs no objects.
/// Keep them shared with full reconstruction, including unread representations.
pub(crate) fn snapshot_chunks(manifest: &Manifest) -> io::Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut lengths = std::collections::BTreeMap::new();
    let mut snapshot_bytes = 0u64;
    for pack in &manifest.packs {
        let total = pack_total(pack.pack_bytes, pack.index_bytes)?;
        snapshot_bytes = snapshot_bytes
            .checked_add(total)
            .filter(|total| *total <= MAX_PACK_BYTES)
            .ok_or_else(|| io::Error::other("prototype snapshot limit exceeded"))?;
        if pack.bundle.is_empty() {
            validate_chunks(&pack.pack, pack.pack_bytes)?;
            validate_chunks(&pack.index, pack.index_bytes)?;
        } else {
            if !pack.pack.is_empty() || !pack.index.is_empty() {
                return Err(io::Error::other("mixed pack chunk representations"));
            }
            validate_chunks(&pack.bundle, total)?;
        }
        for chunk in pack.chunks() {
            if chunk.key.len() != 71
                || !chunk.key.starts_with("chunks/")
                || !chunk.key.as_bytes()[7..]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
                || lengths
                    .insert(&chunk.key, chunk.len)
                    .is_some_and(|len| len != chunk.len)
            {
                return Err(io::Error::other("invalid content-addressed chunk"));
            }
            chunks.push(chunk.clone());
        }
    }
    Ok(chunks)
}

fn validate_chunks(chunks: &[Chunk], expected: u64) -> io::Result<()> {
    if chunks.iter().try_fold(0u64, |total, chunk| {
        (chunk.len != 0).then_some(())?;
        total.checked_add(chunk.len)
    }) != Some(expected)
    {
        return Err(io::Error::other("invalid chunk lengths"));
    }
    Ok(())
}

pub(crate) fn verify_chunk(chunk: &Chunk, data: &[u8]) -> io::Result<()> {
    if data.len() as u64 != chunk.len || chunk.key != format!("chunks/{}", hash(data)) {
        return Err(io::Error::other("corrupt chunk"));
    }
    Ok(())
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
        batches: Mutex<Vec<Vec<Chunk>>>,
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

        fn get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
            self.batches.lock().unwrap().push(chunks.to_vec());
            chunks.iter().map(|chunk| self.get(&chunk.key)).collect()
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

    struct VerifiedBlobs {
        chunks: Vec<Chunk>,
        data: Vec<Vec<u8>>,
        reads: AtomicUsize,
    }

    impl BlobStore for VerifiedBlobs {
        fn get(&self, _: &str) -> io::Result<Vec<u8>> {
            unreachable!("snapshot reads must use the verified batch override")
        }
        fn get_many(&self, _: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
            unreachable!("snapshot reads must use the verified batch override")
        }
        fn get_many_verified(&self, _: &[Chunk]) -> io::Result<storage::VerifiedChunks> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            storage::VerifiedChunks::verify(&self.chunks, self.data.clone())
        }
        fn put(&self, _: &str, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> io::Result<()> {
            unreachable!()
        }
    }

    #[test]
    fn snapshot_consumes_verified_override_and_rejects_valid_proof_substitution() {
        let chunk = |bytes: &[u8]| Chunk {
            key: format!("chunks/{}", hash(bytes)),
            len: bytes.len() as u64,
        };
        let chunks = vec![chunk(b"abc"), chunk(b"def")];
        let manifest = Manifest {
            packs: vec![StoredPack {
                bundle: chunks.clone(),
                pack_bytes: 3,
                index_bytes: 3,
                ..StoredPack::default()
            }],
            ..Manifest::default()
        };
        let mut repository = Repository::new(
            VerifiedBlobs {
                chunks,
                data: vec![b"abc".to_vec(), b"def".to_vec()],
                reads: AtomicUsize::new(0),
            },
            UnusedState,
        );
        assert_eq!(
            repository.read_snapshot_files(&manifest).unwrap(),
            vec![(b"abc".to_vec(), b"def".to_vec())]
        );
        // These bytes remain individually valid for their own metadata, but
        // swapping equal-sized chunks must not substitute a different snapshot.
        repository.blobs.chunks.reverse();
        repository.blobs.data.reverse();
        assert!(repository.read_snapshot_files(&manifest).is_err());
        repository.blobs.chunks[0] = chunk(b"xyz");
        repository.blobs.data[0] = b"xyz".to_vec();
        assert!(repository.read_snapshot_files(&manifest).is_err());
        assert_eq!(repository.blobs.reads.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn retiring_publication_default_preserves_live_representations_and_stale_cas() {
        use crate::storage::LocalServices;
        let root = std::env::temp_dir().join(format!(
            "retiring-local-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let services = LocalServices::new(root.clone()).unwrap();
        let repository = Repository::new(services.blobs, services.state);
        let split = repository
            .store_pack("split".into(), b"pack", b"index", 0)
            .unwrap();
        let bundle = repository
            .store_bundled_pack("bundle".into(), b"packed", b"idx", 0)
            .unwrap();
        let retired = repository
            .store_pack("retired".into(), b"gone", b"gone-index", 0)
            .unwrap();
        let previous = Manifest {
            generation: 1,
            packs: vec![split.clone(), bundle.clone(), retired.clone()],
            ..Manifest::default()
        };
        assert!(
            repository
                .publish(&Manifest::default(), previous.clone())
                .unwrap()
        );
        let mut next = Manifest {
            packs: vec![split, bundle],
            ..Manifest::default()
        };
        next.retire_replaced(&previous);
        // Defensively exclude live keys even if durable debt includes them.
        next.garbage.extend(
            next.packs
                .iter()
                .flat_map(StoredPack::chunks)
                .map(|c| c.key.clone()),
        );
        let durable_debt = next.garbage.clone();
        let RetiringCas::Published(receipt) = repository
            .publish_retiring(&previous, next.clone())
            .unwrap()
        else {
            panic!("publication did not succeed");
        };
        assert_eq!(receipt.generation, 2);
        assert_eq!(receipt.outcomes.len(), 2);
        assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
        assert_eq!(repository.state.load().unwrap().garbage, durable_debt);
        for pack in &next.packs {
            repository.read_pack_files(pack).unwrap();
        }
        for chunk in retired.chunks() {
            assert!(repository.blobs.get(&chunk.key).is_err());
        }
        // A stale CAS must leave even eligible debt intact.
        let orphan = repository
            .store_pack("orphan".into(), b"new-orphan", b"new-index", 0)
            .unwrap();
        next.garbage = orphan.chunks().map(|c| c.key.clone()).collect();
        assert_eq!(
            repository.publish_retiring(&previous, next).unwrap(),
            RetiringCas::Stale
        );
        repository.read_pack_files(&orphan).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    struct AttemptedState {
        cas_calls: AtomicUsize,
        fail: bool,
    }

    impl StateStore for AttemptedState {
        fn load(&self) -> io::Result<Manifest> {
            unreachable!()
        }
        fn compare_and_swap(&self, _: u64, _: &Manifest) -> io::Result<bool> {
            self.cas_calls.fetch_add(1, Ordering::Relaxed);
            panic!("attempted fusion must never retry scalar CAS")
        }
        fn try_compare_and_swap_retiring(
            &self,
            _: u64,
            next: &Manifest,
        ) -> io::Result<RetiringCas> {
            if self.fail {
                return Err(io::Error::other("lost committed response"));
            }
            Ok(RetiringCas::Published(RetirementReceipt {
                generation: next.generation,
                outcomes: vec![RetirementOutcome {
                    key: next.garbage[0].clone(),
                    deleted: false,
                }],
            }))
        }
    }

    #[test]
    fn attempted_fusion_never_republishes_or_double_collects_partial_failure() {
        for fail in [false, true] {
            let repository = Repository::new(
                MemoryBlobs::default(),
                AttemptedState {
                    cas_calls: AtomicUsize::new(0),
                    fail,
                },
            );
            repository.blobs.put("retired", b"unchanged").unwrap();
            let next = Manifest {
                garbage: vec!["retired".into()],
                ..Manifest::default()
            };
            let result = repository.publish_retiring(&Manifest::default(), next);
            if fail {
                assert!(result.is_err());
            } else {
                let RetiringCas::Published(receipt) = result.unwrap() else {
                    panic!("missing successful CAS");
                };
                assert!(!receipt.outcomes[0].deleted);
            }
            assert_eq!(repository.state.cas_calls.load(Ordering::Relaxed), 0);
            assert_eq!(repository.blobs.get("retired").unwrap(), b"unchanged");
        }
    }

    #[test]
    fn snapshot_reads_flatten_mixed_packs_and_preserve_repeated_keys_in_order() {
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let legacy = repository
            .store_pack("legacy".into(), b"pack", b"index", 0)
            .unwrap();
        let bundled = repository
            .store_bundled_pack("bundle".into(), b"other", b"index", 0)
            .unwrap();
        let manifest = Manifest {
            packs: vec![legacy.clone(), bundled.clone(), legacy.clone()],
            ..Manifest::default()
        };
        assert_eq!(
            repository.read_snapshot_files(&manifest).unwrap(),
            vec![
                (b"pack".to_vec(), b"index".to_vec()),
                (b"other".to_vec(), b"index".to_vec()),
                (b"pack".to_vec(), b"index".to_vec()),
            ]
        );
        assert_eq!(
            *repository.blobs.batches.lock().unwrap(),
            vec![
                manifest
                    .packs
                    .iter()
                    .flat_map(StoredPack::chunks)
                    .cloned()
                    .collect::<Vec<_>>()
            ]
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 5);
        assert!(
            repository
                .read_snapshot_files(&Manifest::default())
                .unwrap()
                .is_empty()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn snapshot_validates_all_metadata_before_io_and_rejects_missing_or_corrupt_data() {
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let valid = repository
            .store_bundled_pack("bundle".into(), b"pack", b"index", 0)
            .unwrap();
        let mut invalid = Vec::new();
        let mut mixed = valid.clone();
        mixed.pack = valid.bundle.clone();
        invalid.push(mixed);
        for len in [0, u64::MAX, valid.bundle[0].len + 1] {
            let mut changed = valid.clone();
            changed.bundle[0].len = len;
            invalid.push(changed);
        }
        let mut bad_key = valid.clone();
        bad_key.bundle[0].key = format!("chunks/{}", "A".repeat(64));
        invalid.push(bad_key);
        let mut overflow = valid.clone();
        overflow.pack_bytes = u64::MAX;
        invalid.push(overflow);
        // Equal keys with conflicting expected lengths are invalid even when
        // both packs' own sums are consistent.
        let mut conflict = valid.clone();
        conflict.bundle[0].len += 1;
        conflict.pack_bytes += 1;
        invalid.push(conflict);
        for pack in invalid {
            let manifest = Manifest {
                packs: vec![valid.clone(), pack],
                ..Manifest::default()
            };
            assert!(repository.read_snapshot_files(&manifest).is_err());
        }
        let huge = StoredPack {
            bundle: vec![Chunk {
                key: valid.bundle[0].key.clone(),
                len: MAX_PACK_BYTES,
            }],
            pack_bytes: MAX_PACK_BYTES,
            ..StoredPack::default()
        };
        assert!(
            repository
                .read_snapshot_files(&Manifest {
                    packs: vec![huge.clone(), huge],
                    ..Manifest::default()
                })
                .is_err()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 0);
        assert!(repository.blobs.batches.lock().unwrap().is_empty());
        let manifest = Manifest {
            packs: vec![valid.clone()],
            ..Manifest::default()
        };
        repository
            .blobs
            .bytes
            .lock()
            .unwrap()
            .get_mut(&valid.bundle[0].key)
            .unwrap()[0] ^= 1;
        assert!(repository.read_snapshot_files(&manifest).is_err());
        repository.blobs.bytes.lock().unwrap().clear();
        assert_eq!(
            repository
                .read_snapshot_files(&manifest)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
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
