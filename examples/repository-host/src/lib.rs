//! Experimental host-side coordination. Native Git is the initial pack codec;
//! this crate does not add receive-pack to the allocator-free library or Worker.
pub mod accounting;
pub mod benchmark_cost;
pub mod cloudflare;
pub mod cost;
pub mod diagnostics;
mod first_read;
pub mod http;
pub mod native;
pub mod objects;
mod packed;
mod socket_rpc;
pub mod storage;

use sha2::{Digest, Sha256};
use std::io;
use storage::{
    BlobStore, Chunk, Manifest, PendingChunk, RetirementOutcome, RetirementReceipt, RetiringCas,
    StagedPack, StateStore, StoredPack, UploadRetiringCas,
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

pub(crate) struct ResolvedSnapshot {
    pub files: Vec<(Vec<u8>, Vec<u8>)>,
    #[cfg(test)]
    pub admission: Option<storage::VerifiedChunk>,
    pub cache_hits: usize,
    pub cache_hit_bytes: u64,
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

    /// Import the virtual concatenation of pack and index, retaining only the
    /// last chunk. Scratch storage never exceeds one 16 MiB chunk, including
    /// the chunk which straddles the pack/index boundary.
    pub fn stage_bundled_pack(
        &self,
        id: String,
        pack: &[u8],
        index: &[u8],
        level: u32,
    ) -> io::Result<StagedPack> {
        let total = pack_total(pack.len() as u64, index.len() as u64)? as usize;
        if total == 0 || self.chunk_bytes == 0 || self.chunk_bytes > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid staged pack bound",
            ));
        }
        let mut bundle = Vec::new();
        let mut offset = 0usize;
        loop {
            let end = offset.saturating_add(self.chunk_bytes).min(total);
            let mut bytes = Vec::with_capacity(end - offset);
            if offset < pack.len() {
                bytes.extend_from_slice(&pack[offset..end.min(pack.len())]);
            }
            if end > pack.len() {
                bytes
                    .extend_from_slice(&index[offset.saturating_sub(pack.len())..end - pack.len()]);
            }
            let pending = PendingChunk::new(bytes)?;
            bundle.push(pending.chunk().clone());
            if end == total {
                return Ok(StagedPack {
                    stored: StoredPack {
                        id,
                        bundle,
                        pack_bytes: pack.len() as u64,
                        index_bytes: index.len() as u64,
                        level,
                        ..StoredPack::default()
                    },
                    pending,
                });
            }
            self.blobs.put(&pending.chunk().key, pending.bytes())?;
            offset = end;
        }
    }

    /// Flush an explicitly staged tail before staging another new pack.
    pub fn flush_pending(&self, pending: PendingChunk) -> io::Result<()> {
        self.blobs.put(&pending.chunk().key, pending.bytes())
    }

    /// An optional final upload is durable before the next manifest is CASed.
    /// Only a side-effect-free Unsupported result permits one ordinary PUT and
    /// the existing retiring publication; ambiguous outcomes never republish.
    pub fn publish_staged_retiring(
        &self,
        previous: &Manifest,
        mut next: Manifest,
        pending: Option<PendingChunk>,
    ) -> io::Result<RetiringCas> {
        let Some(pending) = pending else {
            return self.publish_retiring(previous, next);
        };
        next.generation = previous
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("generation overflow"))?;
        validate_staged_manifest(&next, &pending)?;
        let result = self.state.try_compare_and_swap_upload_retiring(
            previous.generation,
            &next,
            &pending,
        )?;
        let (upload, retirement) = match result {
            UploadRetiringCas::Unsupported => {
                self.flush_pending(pending)?;
                return self.publish_retiring(previous, next);
            }
            UploadRetiringCas::Stale(upload) => (upload, RetiringCas::Stale),
            UploadRetiringCas::Published { upload, retirement } => {
                (upload, RetiringCas::Published(retirement))
            }
        };
        if upload.key != pending.chunk().key || upload.len != pending.chunk().len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "upload receipt mismatch",
            ));
        }
        if let RetiringCas::Published(receipt) = &retirement {
            if receipt.generation != next.generation
                || receipt.outcomes.len() != next.retirement_keys().len()
                || receipt
                    .outcomes
                    .iter()
                    .zip(next.retirement_keys())
                    .any(|(outcome, key)| outcome.key != key)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "retirement receipt mismatch",
                ));
            }
        }
        Ok(retirement)
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
        self.read_snapshot_selection(manifest, &(0..manifest.packs.len()).collect::<Vec<_>>())
    }

    /// Private selection boundary: validate the entire descriptor graph before
    /// reading even one selected chunk. The native host supplies ordered pack
    /// positions from its private provenance proof, never client metadata.
    pub(crate) fn read_snapshot_selection(
        &self,
        manifest: &Manifest,
        selected: &[usize],
    ) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.read_snapshot_selection_prefetched(manifest, selected, None)
    }

    pub(crate) fn read_snapshot_selection_prefetched(
        &self,
        manifest: &Manifest,
        selected: &[usize],
        prefetched: Option<(usize, storage::VerifiedChunks)>,
    ) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        snapshot_chunks(manifest)?;
        if selected.iter().any(|i| *i >= manifest.packs.len())
            || selected.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(io::Error::other("invalid ordered pack selection"));
        }
        let chunks: Vec<_> = selected
            .iter()
            .flat_map(|i| manifest.packs[*i].chunks().cloned())
            .collect();
        let data = if let Some((count, first)) = prefetched {
            if count == 0 || count > chunks.len() {
                return Err(io::Error::other("invalid prefetched prefix"));
            }
            let mut data = first.into_data(&chunks[..count])?;
            if count < chunks.len() {
                data.extend(
                    self.blobs
                        .get_many_verified(&chunks[count..])?
                        .into_data(&chunks[count..])?,
                );
            }
            data
        } else {
            self.blobs.get_many_verified(&chunks)?.into_data(&chunks)?
        };
        let mut data = data.into_iter();
        let mut files = Vec::with_capacity(selected.len());
        for pack in selected.iter().map(|i| &manifest.packs[*i]) {
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

    /// Original one-slot interface retained for its proof/order tests.
    #[cfg(test)]
    pub(crate) fn read_snapshot_selection_resolved(
        &self,
        manifest: &Manifest,
        selected: &[usize],
        cached: Option<storage::VerifiedChunk>,
        prefetched: Option<(Vec<Chunk>, storage::VerifiedChunks)>,
        admit: bool,
    ) -> io::Result<ResolvedSnapshot> {
        let mut admission = None;
        let mut result = self.read_snapshot_selection_cached(
            manifest,
            selected,
            &cached.into_iter().collect::<Vec<_>>(),
            prefetched,
            &mut |value| {
                if admit && admission.is_none() && value.bytes().len() <= 16 * 1024 * 1024 {
                    admission = Some(value.clone());
                }
            },
        )?;
        result.admission = admission;
        Ok(result)
    }

    /// Snapshot the exact cached partition before resolving all missing bytes.
    /// The visitor sees every verified miss, not a truncated candidate prefix.
    pub(crate) fn read_snapshot_selection_cached(
        &self,
        manifest: &Manifest,
        selected: &[usize],
        cached: &[storage::VerifiedChunk],
        prefetched: Option<(Vec<Chunk>, storage::VerifiedChunks)>,
        admit: &mut impl FnMut(&storage::VerifiedChunk),
    ) -> io::Result<ResolvedSnapshot> {
        snapshot_chunks(manifest)?;
        if selected.iter().any(|i| *i >= manifest.packs.len())
            || selected.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(io::Error::other("invalid ordered pack selection"));
        }
        let chunks: Vec<_> = selected
            .iter()
            .flat_map(|i| manifest.packs[*i].chunks().cloned())
            .collect();
        if cached.len() > 32
            || cached.iter().enumerate().any(|(i, value)| {
                cached[..i]
                    .iter()
                    .any(|old| old.descriptor() == value.descriptor())
            })
        {
            return Err(io::Error::other("invalid cached descriptor set"));
        }
        let find = |chunk: &Chunk| cached.iter().find(|v| v.descriptor() == chunk);
        let hit = |chunk: &Chunk| find(chunk).is_some();
        let missing: Vec<_> = chunks.iter().filter(|c| !hit(c)).cloned().collect();
        let mut fresh = if let Some((first, verified)) = prefetched {
            if first.is_empty() || first.len() > missing.len() || first != missing[..first.len()] {
                return Err(io::Error::other("invalid prefetched missing batch"));
            }
            let mut fresh = verified.into_handles(&first)?;
            if first.len() < missing.len() {
                let rest = &missing[first.len()..];
                fresh.extend(self.blobs.get_many_verified(rest)?.into_handles(rest)?);
            }
            fresh
        } else {
            self.blobs
                .get_many_verified(&missing)?
                .into_handles(&missing)?
        }
        .into_iter();
        let mut cache_hits = 0;
        let mut cache_hit_bytes = 0;
        let mut handles = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            let value = if hit(chunk) {
                cache_hits += 1;
                cache_hit_bytes += chunk.len;
                find(chunk).unwrap().clone()
            } else {
                let value = fresh
                    .next()
                    .ok_or_else(|| io::Error::other("missing verified chunk"))?;
                admit(&value);
                value
            };
            if value.descriptor() != chunk {
                return Err(io::Error::other("resolved chunk identity mismatch"));
            }
            handles.push(value);
        }
        if fresh.next().is_some() {
            return Err(io::Error::other("extra verified chunks"));
        }
        let mut handles = handles.into_iter();
        let mut files = Vec::with_capacity(selected.len());
        for pack in selected.iter().map(|i| &manifest.packs[*i]) {
            let mut read = |count: usize, size: u64| {
                if count == 0 {
                    return Vec::new();
                }
                let first = handles.next().expect("verified chunk count");
                let mut file = match first.try_into_bytes() {
                    Ok(mut owned) => {
                        owned.reserve_exact(size as usize - owned.len());
                        owned
                    }
                    Err(shared) => {
                        let mut file = Vec::with_capacity(size as usize);
                        file.extend_from_slice(shared.bytes());
                        file
                    }
                };
                for chunk in handles.by_ref().take(count - 1) {
                    file.extend_from_slice(chunk.bytes());
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
        Ok(ResolvedSnapshot {
            files,
            #[cfg(test)]
            admission: None,
            cache_hits,
            cache_hit_bytes,
        })
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
    let mut ids = std::collections::BTreeSet::new();
    for pack in &manifest.packs {
        if !ids.insert(&pack.id) {
            return Err(io::Error::other("duplicate snapshot pack ID"));
        }
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

/// Validate the complete descriptor graph and pending membership before any
/// fast-path or fallback I/O, including repeated/shared chunk lengths.
fn validate_staged_manifest(next: &Manifest, pending: &PendingChunk) -> io::Result<()> {
    // PendingChunk's private constructor computes its immutable descriptor
    // from these exact owned bytes. Reuse that proof without hashing again.
    if next.refs.len() > 16 * 1024
        || next
            .refs
            .iter()
            .try_fold(0usize, |n, (name, id)| {
                n.checked_add(name.len())?.checked_add(id.len())
            })
            .is_none_or(|n| n > 4 * 1024 * 1024)
    {
        return Err(io::Error::other("prototype ref limit exceeded"));
    }
    for (name, id) in &next.refs {
        git_zero_core::repository::validate_ref_name(name)
            .map_err(|_| io::Error::other("invalid staged ref name"))?;
        if id.len() != 40
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || id.bytes().all(|byte| byte == b'0')
        {
            return Err(io::Error::other("invalid staged ref object ID"));
        }
    }
    let mut lengths = std::collections::BTreeMap::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut total = 0u64;
    for pack in &next.packs {
        if !ids.insert(&pack.id) {
            return Err(io::Error::other("duplicate staged pack ID"));
        }
        let size = pack_total(pack.pack_bytes, pack.index_bytes)?;
        total = total
            .checked_add(size)
            .filter(|size| *size <= MAX_PACK_BYTES)
            .ok_or_else(|| io::Error::other("prototype snapshot limit exceeded"))?;
        if pack.bundle.is_empty() {
            validate_chunks(&pack.pack, pack.pack_bytes)?;
            validate_chunks(&pack.index, pack.index_bytes)?;
        } else {
            if !pack.pack.is_empty() || !pack.index.is_empty() {
                return Err(io::Error::other("mixed pack chunk representations"));
            }
            validate_chunks(&pack.bundle, size)?;
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
                return Err(io::Error::other("invalid staged chunk"));
            }
        }
    }
    if lengths.get(&pending.chunk().key) != Some(&pending.chunk().len) {
        return Err(io::Error::other(
            "pending chunk is not a matching manifest member",
        ));
    }
    Ok(())
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

    impl BlobStore for std::sync::Arc<MemoryBlobs> {
        fn get(&self, key: &str) -> io::Result<Vec<u8>> {
            (**self).get(key)
        }
        fn put(&self, key: &str, data: &[u8]) -> io::Result<()> {
            (**self).put(key, data)
        }
        fn delete(&self, key: &str) -> io::Result<()> {
            (**self).delete(key)
        }
    }

    #[test]
    fn staging_keeps_only_last_virtual_chunk_and_eager_apis_remain_durable() {
        let mut repository = Repository::new(MemoryBlobs::default(), UnusedState);
        repository.chunk_bytes = 4;
        let (stored, pending) = repository
            .stage_bundled_pack("stage".into(), b"abcdefghij", b"klmnop", 2)
            .unwrap()
            .into_parts();
        assert_eq!(repository.blobs.puts.load(Ordering::Relaxed), 3);
        assert_eq!(stored.bundle.len(), 4);
        assert_eq!(stored.level, 2);
        assert_eq!(pending.bytes(), b"mnop");
        // The third chunk straddles the original pack/index split.
        assert_eq!(
            repository.blobs.get(&stored.bundle[2].key).unwrap(),
            b"ijkl"
        );
        assert!(repository.blobs.get(&stored.bundle[3].key).is_err());
        repository.flush_pending(pending).unwrap();
        assert_eq!(
            repository.read_pack_files(&stored).unwrap(),
            (b"abcdefghij".to_vec(), b"klmnop".to_vec())
        );
        let eager = repository
            .store_bundled_pack("eager".into(), b"abcd", b"efgh", 0)
            .unwrap();
        repository.read_pack_files(&eager).unwrap();
        let eager = repository
            .store_pack("eager-split".into(), b"abcd", b"efgh", 0)
            .unwrap();
        repository.read_pack_files(&eager).unwrap();
        assert_eq!(repository.blobs.puts.load(Ordering::Relaxed), 8);
        assert!(
            repository
                .stage_bundled_pack("empty".into(), b"", b"", 0)
                .is_err()
        );
        for chunk_bytes in [0, 16 * 1024 * 1024 + 1, usize::MAX] {
            repository.chunk_bytes = chunk_bytes;
            assert!(
                repository
                    .stage_bundled_pack("invalid".into(), b"x", b"", 0)
                    .is_err()
            );
        }
    }

    struct UploadState {
        blobs: std::sync::Arc<MemoryBlobs>,
        current: Mutex<Manifest>,
        cas: AtomicUsize,
        uploads: AtomicUsize,
        mode: u8, // 0 fallback, 1 success, 2 stale, 3 lost reply, 4 invalid receipt
    }

    impl StateStore for UploadState {
        fn load(&self) -> io::Result<Manifest> {
            Ok(self.current.lock().unwrap().clone())
        }
        fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
            self.cas.fetch_add(1, Ordering::Relaxed);
            for chunk in next.packs.iter().flat_map(StoredPack::chunks) {
                self.blobs.get(&chunk.key)?;
            }
            let mut current = self.current.lock().unwrap();
            if current.generation != expected {
                return Ok(false);
            }
            *current = next.clone();
            Ok(true)
        }
        fn try_compare_and_swap_upload_retiring(
            &self,
            _: u64,
            next: &Manifest,
            pending: &PendingChunk,
        ) -> io::Result<UploadRetiringCas> {
            if self.mode == 0 {
                return Ok(UploadRetiringCas::Unsupported);
            }
            self.uploads.fetch_add(1, Ordering::Relaxed);
            self.blobs.put(&pending.chunk().key, pending.bytes())?;
            let mut upload = storage::UploadReceipt {
                key: pending.chunk().key.clone(),
                len: pending.chunk().len,
            };
            if self.mode == 2 {
                return Ok(UploadRetiringCas::Stale(upload));
            }
            *self.current.lock().unwrap() = next.clone();
            if self.mode == 3 {
                return Err(io::Error::other("lost response after durable publication"));
            }
            if self.mode == 4 {
                upload.len += 1;
            }
            Ok(UploadRetiringCas::Published {
                upload,
                retirement: RetirementReceipt {
                    generation: next.generation,
                    outcomes: Vec::new(),
                },
            })
        }
    }

    #[test]
    fn staged_publication_uploads_once_before_cas_and_never_replays_ambiguous_results() {
        for mode in 0..=4 {
            let blobs = std::sync::Arc::new(MemoryBlobs::default());
            let repository = Repository::new(
                blobs.clone(),
                UploadState {
                    blobs: blobs.clone(),
                    current: Mutex::new(Manifest::default()),
                    cas: AtomicUsize::new(0),
                    uploads: AtomicUsize::new(0),
                    mode,
                },
            );
            let (stored, pending) = repository
                .stage_bundled_pack("tail".into(), b"pack", b"index", 0)
                .unwrap()
                .into_parts();
            assert_eq!(blobs.puts.load(Ordering::Relaxed), 0);
            let result = repository.publish_staged_retiring(
                &Manifest::default(),
                Manifest {
                    packs: vec![stored.clone()],
                    ..Manifest::default()
                },
                Some(pending),
            );
            match mode {
                0 | 1 => assert!(matches!(result.unwrap(), RetiringCas::Published(_))),
                2 => assert_eq!(result.unwrap(), RetiringCas::Stale),
                _ => assert!(result.is_err()),
            }
            assert_eq!(blobs.puts.load(Ordering::Relaxed), 1);
            assert_eq!(
                repository.state.cas.load(Ordering::Relaxed),
                usize::from(mode == 0)
            );
            assert_eq!(
                repository.state.uploads.load(Ordering::Relaxed),
                usize::from(mode != 0)
            );
            // Even stale uploads remain intact: the winner may share their key.
            repository.read_pack_files(&stored).unwrap();
        }
    }

    #[test]
    fn staged_membership_and_complete_shape_are_checked_before_any_mutation() {
        let blobs = std::sync::Arc::new(MemoryBlobs::default());
        let repository = Repository::new(
            blobs.clone(),
            UploadState {
                blobs: blobs.clone(),
                current: Mutex::new(Manifest::default()),
                cas: AtomicUsize::new(0),
                uploads: AtomicUsize::new(0),
                mode: 1,
            },
        );
        for case in 0..5 {
            let (stored, pending) = repository
                .stage_bundled_pack("tail".into(), b"pack", b"index", 0)
                .unwrap()
                .into_parts();
            let mut next = Manifest {
                packs: vec![stored.clone()],
                ..Manifest::default()
            };
            match case {
                0 => next.packs.clear(),
                1 => next.packs[0].bundle[0].len += 1,
                2 => next.packs[0].pack.push(stored.bundle[0].clone()),
                3 => {
                    let mut other = stored.clone();
                    other.bundle[0].len -= 1;
                    other.index_bytes -= 1;
                    next.packs.push(other);
                }
                _ => next.packs[0].bundle[0].key = "../invalid".into(),
            }
            assert!(
                repository
                    .publish_staged_retiring(&Manifest::default(), next, Some(pending))
                    .is_err()
            );
        }
        assert_eq!(blobs.puts.load(Ordering::Relaxed), 0);
        assert_eq!(repository.state.cas.load(Ordering::Relaxed), 0);
        assert_eq!(repository.state.uploads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn several_staged_packs_flush_only_previous_tail_and_preserve_repeated_keys() {
        let blobs = std::sync::Arc::new(MemoryBlobs::default());
        let mut repository = Repository::new(
            blobs.clone(),
            UploadState {
                blobs: blobs.clone(),
                current: Mutex::new(Manifest::default()),
                cas: AtomicUsize::new(0),
                uploads: AtomicUsize::new(0),
                mode: 0,
            },
        );
        repository.chunk_bytes = 4;
        let (first, pending) = repository
            .stage_bundled_pack("first".into(), b"abcdabcd", b"ab", 0)
            .unwrap()
            .into_parts();
        assert_eq!(blobs.puts.load(Ordering::Relaxed), 2);
        repository.flush_pending(pending).unwrap();
        let (last, pending) = repository
            .stage_bundled_pack("last".into(), b"abcd", b"ab", 0)
            .unwrap()
            .into_parts();
        assert_eq!(blobs.puts.load(Ordering::Relaxed), 4);
        let mut next = Manifest {
            packs: vec![first, last],
            ..Manifest::default()
        };
        // Live repeated keys in durable debt cannot become retirement work.
        next.garbage.push(pending.chunk().key.clone());
        assert!(matches!(
            repository
                .publish_staged_retiring(&Manifest::default(), next.clone(), Some(pending))
                .unwrap(),
            RetiringCas::Published(_)
        ));
        assert_eq!(blobs.puts.load(Ordering::Relaxed), 5);
        for pack in &next.packs {
            repository.read_pack_files(pack).unwrap();
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
        let mut repeated = legacy.clone();
        repeated.id = "repeated-legacy".into();
        let manifest = Manifest {
            packs: vec![legacy.clone(), bundled.clone(), repeated],
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
    fn prefetched_snapshot_consumes_exact_first_prefix_once_then_reads_only_suffix() {
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let first = repository
            .store_pack("legacy".into(), b"pack", b"index", 0)
            .unwrap();
        let last = repository
            .store_bundled_pack("bundle".into(), b"tail", b"idx", 0)
            .unwrap();
        let manifest = Manifest {
            packs: vec![first, last],
            ..Manifest::default()
        };
        let chunks = snapshot_chunks(&manifest).unwrap();
        let prefix = storage::VerifiedChunks::verify(
            &chunks[..2],
            vec![b"pack".to_vec(), b"index".to_vec()],
        )
        .unwrap();
        let files = repository
            .read_snapshot_selection_prefetched(&manifest, &[0, 1], Some((2, prefix)))
            .unwrap();
        assert_eq!(
            files,
            vec![
                (b"pack".to_vec(), b"index".to_vec()),
                (b"tail".to_vec(), b"idx".to_vec())
            ]
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 1);
        assert_eq!(
            *repository.blobs.batches.lock().unwrap(),
            vec![chunks[2..].to_vec()]
        );
        let wrong =
            storage::VerifiedChunks::verify(&chunks[1..2], vec![b"index".to_vec()]).unwrap();
        assert!(
            repository
                .read_snapshot_selection_prefetched(&manifest, &[0, 1], Some((1, wrong)))
                .is_err()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cached_snapshot_merges_an_interior_missing_batch_and_rejects_prefix_substitution() {
        use std::sync::atomic::Ordering;
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let first = repository
            .store_pack("first".into(), b"pack", b"index", 0)
            .unwrap();
        let last = repository
            .store_bundled_pack("last".into(), b"tail", b"idx", 0)
            .unwrap();
        let manifest = Manifest {
            packs: vec![first, last],
            ..Manifest::default()
        };
        let chunks = snapshot_chunks(&manifest).unwrap();
        let cached = storage::VerifiedChunks::verify(&chunks[..1], vec![b"pack".to_vec()])
            .unwrap()
            .into_handles(&chunks[..1])
            .unwrap()
            .remove(0);
        let paid = storage::VerifiedChunks::verify(&chunks[1..2], vec![b"index".to_vec()]).unwrap();
        let result = repository
            .read_snapshot_selection_resolved(
                &manifest,
                &[0, 1],
                Some(cached.clone()),
                Some((chunks[1..2].to_vec(), paid)),
                true,
            )
            .unwrap();
        assert_eq!(
            result.files,
            vec![
                (b"pack".to_vec(), b"index".to_vec()),
                (b"tail".to_vec(), b"idx".to_vec())
            ]
        );
        assert_eq!(result.cache_hits, 1);
        assert_eq!(result.cache_hit_bytes, 4);
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 1);
        assert_eq!(
            *repository.blobs.batches.lock().unwrap(),
            vec![chunks[2..].to_vec()]
        );
        let wrong = storage::VerifiedChunks::verify(&chunks[..1], vec![b"pack".to_vec()]).unwrap();
        assert!(
            repository
                .read_snapshot_selection_resolved(
                    &manifest,
                    &[0, 1],
                    Some(cached),
                    Some((chunks[..1].to_vec(), wrong)),
                    false,
                )
                .is_err()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn multi_cached_snapshot_visits_all_misses_and_keeps_exact_order() {
        use std::sync::atomic::Ordering;
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let mut manifest = Manifest::default();
        for index in 0..40u8 {
            manifest.packs.push(
                repository
                    .store_bundled_pack(index.to_string(), &[index], &[index, 1], 0)
                    .unwrap(),
            );
        }
        let chunks = snapshot_chunks(&manifest).unwrap();
        let cached_descriptors = vec![chunks[1].clone(), chunks[35].clone()];
        let cached = storage::VerifiedChunks::verify(
            &cached_descriptors,
            vec![vec![1, 1, 1], vec![35, 35, 1]],
        )
        .unwrap()
        .into_handles(&cached_descriptors)
        .unwrap();
        let missing: Vec<_> = chunks
            .iter()
            .filter(|c| !cached_descriptors.contains(c))
            .cloned()
            .collect();
        let first = vec![missing[0].clone(), missing[1].clone()];
        let paid =
            storage::VerifiedChunks::verify(&first, vec![vec![0, 0, 1], vec![2, 2, 1]]).unwrap();
        let mut seen = Vec::new();
        let selected: Vec<_> = (0..40).collect();
        let resolved = repository
            .read_snapshot_selection_cached(
                &manifest,
                &selected,
                &cached,
                Some((first, paid)),
                &mut |value| seen.push(value.descriptor().clone()),
            )
            .unwrap();
        assert_eq!(
            seen, missing,
            "later fitting candidates cannot be lost to a prefix limit"
        );
        assert_eq!(resolved.cache_hits, 2);
        for (index, (pack, idx)) in resolved.files.iter().enumerate() {
            assert_eq!(pack, &vec![index as u8]);
            assert_eq!(idx, &vec![index as u8, 1]);
        }
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 36);
        let before = repository.blobs.gets.load(Ordering::Relaxed);
        assert!(
            repository
                .read_snapshot_selection_cached(
                    &manifest,
                    &selected,
                    &[cached[0].clone(), cached[0].clone()],
                    None,
                    &mut |_| panic!("invalid set before admission")
                )
                .is_err()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), before);
    }

    #[test]
    fn all_cached_snapshot_checks_metadata_and_never_reads_or_readmits() {
        use std::sync::atomic::Ordering;
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let pack = repository
            .store_bundled_pack("one".into(), b"pack", b"index", 0)
            .unwrap();
        let mut manifest = Manifest {
            packs: vec![pack],
            ..Manifest::default()
        };
        let chunks = snapshot_chunks(&manifest).unwrap();
        let cached = storage::VerifiedChunks::verify(&chunks, vec![b"packindex".to_vec()])
            .unwrap()
            .into_handles(&chunks)
            .unwrap()
            .remove(0);
        let result = repository
            .read_snapshot_selection_resolved(&manifest, &[0], Some(cached.clone()), None, true)
            .unwrap();
        assert_eq!(result.files, vec![(b"pack".to_vec(), b"index".to_vec())]);
        assert_eq!(result.cache_hits, 1);
        assert!(result.admission.is_none());
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 0);
        manifest.packs[0].bundle[0].len += 1;
        assert!(
            repository
                .read_snapshot_selection_resolved(&manifest, &[0], Some(cached), None, true)
                .is_err()
        );
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cached_middle_chunk_merges_noncontiguous_missing_batch_and_rejects_forged_partitions() {
        use std::sync::atomic::Ordering;
        let repository = Repository::new(MemoryBlobs::default(), UnusedState);
        let first = repository
            .store_pack("first".into(), b"pack", b"index", 0)
            .unwrap();
        let last = repository
            .store_bundled_pack("last".into(), b"tail", b"idx", 0)
            .unwrap();
        let manifest = Manifest {
            packs: vec![first, last],
            ..Manifest::default()
        };
        let chunks = snapshot_chunks(&manifest).unwrap();
        let middle = storage::VerifiedChunks::verify(&chunks[1..2], vec![b"index".to_vec()])
            .unwrap()
            .into_handles(&chunks[1..2])
            .unwrap()
            .remove(0);
        let missing = vec![chunks[0].clone(), chunks[2].clone()];
        let proof =
            storage::VerifiedChunks::verify(&missing, vec![b"pack".to_vec(), b"tailidx".to_vec()])
                .unwrap();
        let result = repository
            .read_snapshot_selection_resolved(
                &manifest,
                &[0, 1],
                Some(middle.clone()),
                Some((missing.clone(), proof)),
                false,
            )
            .unwrap();
        assert_eq!(
            result.files,
            vec![
                (b"pack".to_vec(), b"index".to_vec()),
                (b"tail".to_vec(), b"idx".to_vec())
            ]
        );
        assert_eq!(result.cache_hits, 1);
        assert_eq!(repository.blobs.gets.load(Ordering::Relaxed), 0);
        let foreign = Chunk {
            key: format!("chunks/{}", hash(b"foreign")),
            len: 7,
        };
        let verify = |descriptors: &[Chunk], data: Vec<Vec<u8>>| {
            storage::VerifiedChunks::verify(descriptors, data).unwrap()
        };
        let mut wrong_length = chunks[0].clone();
        wrong_length.len += 1;
        let cases = vec![
            (vec![], verify(&[], vec![])),
            (
                vec![chunks[0].clone(), chunks[0].clone()],
                verify(
                    &[chunks[0].clone(), chunks[0].clone()],
                    vec![b"pack".to_vec(), b"pack".to_vec()],
                ),
            ),
            (
                vec![chunks[2].clone(), chunks[0].clone()],
                verify(
                    &[chunks[2].clone(), chunks[0].clone()],
                    vec![b"tailidx".to_vec(), b"pack".to_vec()],
                ),
            ),
            (
                missing.clone(),
                verify(
                    &[chunks[0].clone(), foreign.clone()],
                    vec![b"pack".to_vec(), b"foreign".to_vec()],
                ),
            ),
            (
                vec![foreign.clone()],
                verify(std::slice::from_ref(&foreign), vec![b"foreign".to_vec()]),
            ),
            (
                vec![wrong_length],
                verify(&chunks[..1], vec![b"pack".to_vec()]),
            ),
        ];
        for (first, proof) in cases {
            assert!(
                repository
                    .read_snapshot_selection_resolved(
                        &manifest,
                        &[0, 1],
                        Some(middle.clone()),
                        Some((first, proof)),
                        false
                    )
                    .is_err()
            );
            assert_eq!(
                repository.blobs.gets.load(Ordering::Relaxed),
                0,
                "invalid partition performs no suffix reads"
            );
        }
        let foreign_cache = verify(std::slice::from_ref(&foreign), vec![b"foreign".to_vec()])
            .into_handles(std::slice::from_ref(&foreign))
            .unwrap()
            .remove(0);
        let proof = verify(&missing, vec![b"pack".to_vec(), b"tailidx".to_vec()]);
        assert!(
            repository
                .read_snapshot_selection_resolved(
                    &manifest,
                    &[0, 1],
                    Some(foreign_cache),
                    Some((missing, proof)),
                    false
                )
                .is_err()
        );
        assert_eq!(
            repository.blobs.gets.load(Ordering::Relaxed),
            0,
            "foreign cached descriptor cannot remove a selected missing chunk"
        );
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
