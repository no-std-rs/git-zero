//! Hosting service boundary. Local writers share one `LocalServices` instance
//! (or its clones); independent processes/instances must not write the same root.
//! Cloudflare wrappers delegate to bindings, not a deployed Workers SDK adapter.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub generation: u64,
    pub refs: BTreeMap<String, String>,
    pub packs: Vec<StoredPack>,
    /// Retired chunks are recorded in the published generation before deletion.
    /// Keep the list until the next publication so interrupted deletion retries
    /// are safe without listing the entire blob store on every push.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub garbage: Vec<String>,
}

impl Manifest {
    fn live_chunks(&self) -> BTreeSet<&str> {
        self.packs
            .iter()
            .flat_map(StoredPack::chunks)
            .map(|chunk| chunk.key.as_str())
            .collect()
    }

    /// Ordered retirement debt, excluding every live representation. This is
    /// also the order required in a fused publication's confirmation receipt.
    pub fn retirement_keys(&self) -> Vec<&str> {
        let live = self.live_chunks();
        self.garbage
            .iter()
            .map(String::as_str)
            .filter(|key| !live.contains(key))
            .collect()
    }

    /// Prepare retirement debt for this generation before publishing it.
    /// Shared chunks and chunks reused after a prior retirement remain live.
    pub fn retire_replaced(&mut self, previous: &Self) {
        let live = self.live_chunks();
        let retired: BTreeSet<_> = previous
            .garbage
            .iter()
            .chain(&self.garbage)
            .map(String::as_str)
            .chain(previous.live_chunks())
            .filter(|key| !live.contains(key))
            .map(str::to_owned)
            .collect();
        self.garbage = retired.into_iter().collect();
    }

    /// Collect only an authoritative, durably published generation's debt.
    /// The caller must exclude readers of older generations and publication
    /// while this runs. Failed or interrupted deletion leaves the published
    /// list intact for idempotent retry; no bucket listing is necessary.
    /// Returns the keys whose deletion failed. After successful collection,
    /// clear the in-memory debt before preparing the next generation.
    pub fn collect_garbage(&self, blobs: &dyn BlobStore) -> Vec<String> {
        let live = self.live_chunks();
        let mut failures = Vec::new();
        for key in &self.garbage {
            if live.contains(key.as_str()) {
                continue;
            }
            if blobs.delete(key).is_err() {
                failures.push(key.clone());
            }
        }
        failures
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetirementOutcome {
    pub key: String,
    /// Absence counts as success, matching BlobStore::delete's retry contract.
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetirementReceipt {
    pub generation: u64,
    pub outcomes: Vec<RetirementOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetiringCas {
    /// No publication, deletion, or other side effects have occurred.
    Unsupported,
    Stale,
    /// Publication is durable; individual retirement failures remain debt.
    Published(RetirementReceipt),
}

/// One explicitly non-durable chunk, bounded to the cloud chunk ceiling. It is
/// owned only by the current receive operation, never by a durable store/cache.
pub struct PendingChunk {
    chunk: Chunk,
    bytes: Vec<u8>,
}

impl PendingChunk {
    pub(crate) fn new(bytes: Vec<u8>) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid pending chunk bound",
            ));
        }
        Ok(Self {
            chunk: Chunk {
                key: format!("chunks/{}", crate::hash(&bytes)),
                len: bytes.len() as u64,
            },
            bytes,
        })
    }

    pub fn chunk(&self) -> &Chunk {
        &self.chunk
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// All chunks except its private pending tail are durable. Consume this value
/// only into the explicit staged publication path; ordinary stored APIs remain
/// eager and never return a descriptor for missing bytes.
pub struct StagedPack {
    pub(crate) stored: StoredPack,
    pub(crate) pending: PendingChunk,
}

impl StagedPack {
    pub fn into_parts(self) -> (StoredPack, PendingChunk) {
        (self.stored, self.pending)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UploadReceipt {
    pub key: String,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadRetiringCas {
    /// Absolutely no I/O or mutation has occurred.
    Unsupported,
    /// Upload is durable; losing CAS never authorizes deleting this chunk.
    Stale(UploadReceipt),
    Published {
        upload: UploadReceipt,
        retirement: RetirementReceipt,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPack {
    pub id: String,
    pub pack: Vec<Chunk>,
    pub index: Vec<Chunk>,
    /// Concatenated pack then index, split at `pack_bytes` after verification.
    /// Empty for legacy and local packs stored as separate files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundle: Vec<Chunk>,
    pub pack_bytes: u64,
    pub index_bytes: u64,
    pub level: u32,
}

impl StoredPack {
    /// Include every representation so retirement and orphan recovery preserve
    /// all referenced blobs, even before a malformed manifest is rejected.
    pub fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        self.pack.iter().chain(&self.index).chain(&self.bundle)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chunk {
    pub key: String,
    pub len: u64,
}

/// Owned, ordered chunk bytes whose lengths and content addresses were verified.
/// Payloads remain private until consumed against the exact requested metadata.
pub struct VerifiedChunks {
    chunks: Vec<Chunk>,
    data: Vec<Vec<u8>>,
}

impl VerifiedChunks {
    /// Verify every ordered payload before constructing a complete proof.
    pub fn verify(chunks: &[Chunk], data: Vec<Vec<u8>>) -> io::Result<Self> {
        if chunks.len() != data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete verified chunks",
            ));
        }
        for (chunk, bytes) in chunks.iter().zip(&data) {
            crate::verify_chunk(chunk, bytes)?;
        }
        Ok(Self {
            chunks: chunks.to_vec(),
            data,
        })
    }

    /// Release bytes only for the exact key, length and order that was verified.
    pub fn into_data(self, expected: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        if self.chunks.as_slice() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "verified chunk request mismatch",
            ));
        }
        Ok(self.data)
    }

    pub(crate) fn append(&mut self, mut other: Self) {
        self.chunks.append(&mut other.chunks);
        self.data.append(&mut other.data);
    }
}

/// Published blobs are immutable: repeated equal puts succeed, conflicts fail.
/// A successful put must make bytes durable before a manifest can reference them.
pub trait BlobStore: Send + Sync {
    fn get(&self, key: &str) -> io::Result<Vec<u8>>;
    /// Read in request order. Remote stores may batch these reads; the default
    /// preserves scalar store semantics, including repeated keys and failures.
    fn get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        chunks.iter().map(|chunk| self.get(&chunk.key)).collect()
    }
    /// Read and verify content-addressed chunks, preserving custom batch reads.
    fn get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
        VerifiedChunks::verify(chunks, self.get_many(chunks)?)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    /// Delete durably; absent keys succeed so recovery can retry partial work.
    fn delete(&self, key: &str) -> io::Result<()>;
}

/// CAS publishes the entire next generation atomically and durably. Missing state
/// is generation zero; success advances by exactly one. A stale CAS returns false.
pub trait StateStore: Send + Sync {
    fn load(&self) -> io::Result<Manifest>;
    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool>;
    /// Optional fused operation. Unsupported MUST have no side effects. Once
    /// attempted, any error is ambiguous and requires authoritative recovery;
    /// callers must never fall back to a second publication on an error.
    fn try_compare_and_swap_retiring(
        &self,
        _expected: u64,
        _next: &Manifest,
    ) -> io::Result<RetiringCas> {
        Ok(RetiringCas::Unsupported)
    }

    /// Upload one pending chunk durably before CAS, then retire durable debt.
    /// Unsupported is side-effect-free; attempted errors must never be retried
    /// as an eager PUT or another CAS, including a missing/malformed reply.
    fn try_compare_and_swap_upload_retiring(
        &self,
        _expected: u64,
        _next: &Manifest,
        _pending: &PendingChunk,
    ) -> io::Result<UploadRetiringCas> {
        Ok(UploadRetiringCas::Unsupported)
    }
}

#[derive(Clone)]
pub struct LocalServices {
    pub blobs: LocalBlobStore,
    pub state: LocalStateStore,
}

impl LocalServices {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        durable_create_dir_all(&root.join("blobs"))?;
        Ok(Self {
            blobs: LocalBlobStore {
                root: root.join("blobs"),
                writes: Arc::new(Mutex::new(())),
            },
            state: LocalStateStore {
                path: root.join("manifest.json"),
                writes: Arc::new(Mutex::new(())),
            },
        })
    }
}

#[derive(Clone)]
pub struct LocalBlobStore {
    root: PathBuf,
    writes: Arc<Mutex<()>>,
}

impl LocalBlobStore {
    /// Rebuild a disposable native file from a previously verified chunk.
    /// The caller has validated `verified` against this immutable blob key.
    /// A native view needs no additional durability barrier: its manifest and
    /// blob remain authoritative. Never replace an existing destination.
    pub(crate) fn materialize_file(
        &self,
        key: &str,
        destination: &Path,
        verified: &[u8],
    ) -> io::Result<()> {
        validate_key(key)?;
        let _guard = self.writes.lock().map_err(poisoned)?;
        let source = self.root.join(key);
        if fs::metadata(&source)?.len() != verified.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "blob length differs from verified bytes",
            ));
        }
        match fs::hard_link(source, destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
            Err(_) => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?
                .write_all(verified),
        }
    }

    /// Import a trusted, immutable codec output without writing another copy.
    /// `verified` must contain this exact file's bytes; the caller must keep
    /// the source immutable while this method runs and after a successful link.
    /// Filesystems without hard links fall back to the ordinary durable put.
    pub(crate) fn put_file(&self, key: &str, source: &Path, verified: &[u8]) -> io::Result<()> {
        validate_key(key)?;
        let guard = self.writes.lock().map_err(poisoned)?;
        let path = self.root.join(key);
        match fs::read(&path) {
            Ok(existing) if existing == verified => {
                File::open(&path)?.sync_all()?;
                return File::open(path.parent().expect("blob has a root"))?.sync_all();
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "immutable blob conflict",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let file = File::open(source)?;
        if file.metadata()?.len() != verified.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source length differs from verified bytes",
            ));
        }
        file.sync_all()?;
        let parent = path.parent().expect("blob has a root");
        durable_create_dir_all(parent)?;
        match fs::hard_link(source, &path) {
            Ok(()) => File::open(parent)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
            Err(_) => {
                drop(guard);
                self.put(key, verified)
            }
        }
    }

    /// Reclaim uncommitted writes after startup or failed publication. This
    /// recovery scan is intentionally separate from ordinary push retirement.
    /// The caller must hold exclusive repository ownership, load authoritative
    /// state, and prevent publication/put sequences until the scan completes.
    pub fn reclaim_unreferenced(&self, current: &Manifest) -> io::Result<usize> {
        let _guard = self.writes.lock().map_err(poisoned)?;
        let live = current.live_chunks();
        let mut directories = vec![self.root.clone()];
        let mut removed = 0;
        while let Some(directory) = directories.pop() {
            let mut changed = false;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    directories.push(entry.path());
                    continue;
                }
                let path = entry.path();
                let key = path
                    .strip_prefix(&self.root)
                    .expect("blob entry is inside its root")
                    .to_str()
                    .ok_or_else(|| io::Error::other("non-UTF-8 blob key"))?;
                if !live.contains(key) {
                    fs::remove_file(&path)?;
                    changed = true;
                    removed += 1;
                }
            }
            if changed {
                File::open(directory)?.sync_all()?;
            }
        }
        Ok(removed)
    }
}

impl BlobStore for LocalBlobStore {
    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        validate_key(key)?;
        fs::read(self.root.join(key))
    }

    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        validate_key(key)?;
        let _guard = self.writes.lock().map_err(poisoned)?;
        let path = self.root.join(key);
        match fs::read(&path) {
            Ok(existing) if existing == bytes => {
                // A prior attempt may have failed after rename but before sync.
                File::open(&path)?.sync_all()?;
                return File::open(path.parent().expect("blob has a root"))?.sync_all();
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "immutable blob conflict",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        durable_write(&path, bytes)
    }

    fn delete(&self, key: &str) -> io::Result<()> {
        validate_key(key)?;
        let _guard = self.writes.lock().map_err(poisoned)?;
        let path = self.root.join(key);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Retrying after a failed sync must also sync a now-absent file's
        // parent. A missing parent means no directory entry remains to sync.
        match File::open(path.parent().expect("blob has a root")) {
            Ok(parent) => parent.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone)]
pub struct LocalStateStore {
    path: PathBuf,
    writes: Arc<Mutex<()>>,
}

impl LocalStateStore {
    /// A prior publication can fail after rename but before directory sync.
    /// Stabilize the visible generation before recovery may retire older data.
    /// NativeHost holds exclusive repository ownership during this operation.
    pub(crate) fn stabilize(&self) -> io::Result<()> {
        let _guard = self.writes.lock().map_err(poisoned)?;
        match File::open(&self.path) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = self.path.parent().expect("manifest has a root");
        // Stabilize an ambiguously published rename before retiring anything.
        // A killed writer can also leave its pre-rename snapshot behind. This
        // directory is exclusively owned during recovery, so those regular
        // temporary files cannot belong to a live publication.
        File::open(parent)?.sync_all()?;
        let mut removed = false;
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let is_temporary = name
                .to_str()
                .and_then(|name| name.strip_prefix(".write-"))
                .and_then(|suffix| suffix.split_once('-'))
                .is_some_and(|(process, sequence)| {
                    !process.is_empty()
                        && !sequence.is_empty()
                        && process.bytes().all(|byte| byte.is_ascii_digit())
                        && sequence.bytes().all(|byte| byte.is_ascii_digit())
                });
            if is_temporary && entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
                removed = true;
            }
        }
        if removed {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

impl StateStore for LocalStateStore {
    fn load(&self) -> io::Result<Manifest> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid_data),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Manifest::default()),
            Err(error) => Err(error),
        }
    }

    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
        validate_generation(expected, next)?;
        let _guard = self.writes.lock().map_err(poisoned)?;
        if self.load()?.generation != expected {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(next).map_err(invalid_data)?;
        durable_write(&self.path, &bytes)?;
        Ok(true)
    }
}

/// Bridge implemented by a future Workers Rust/Wasm SDK adapter. R2 puts must
/// preserve immutable keys. `manifest_cas` must transact the expected-generation
/// check and entire manifest replacement in one repository Durable Object.
/// This synchronous prototype interface still needs an async streaming variant
/// for a Worker runtime; native tests exercise delegation and semantics only.
pub trait CloudflareBindings: Send + Sync {
    fn r2_get(&self, key: &str) -> io::Result<Vec<u8>>;
    fn r2_get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        chunks.iter().map(|chunk| self.r2_get(&chunk.key)).collect()
    }
    fn r2_get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
        VerifiedChunks::verify(chunks, self.r2_get_many(chunks)?)
    }
    fn r2_put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    fn r2_delete(&self, key: &str) -> io::Result<()>;
    fn manifest_load(&self) -> io::Result<Manifest>;
    fn manifest_cas(&self, expected: u64, next: &Manifest) -> io::Result<bool>;
    fn try_compare_and_swap_retiring(
        &self,
        _expected: u64,
        _next: &Manifest,
    ) -> io::Result<RetiringCas> {
        Ok(RetiringCas::Unsupported)
    }

    /// Upload one pending chunk durably before CAS, then retire durable debt.
    /// Unsupported is side-effect-free; attempted errors must never be retried
    /// as an eager PUT or another CAS, including a missing/malformed reply.
    fn try_compare_and_swap_upload_retiring(
        &self,
        _expected: u64,
        _next: &Manifest,
        _pending: &PendingChunk,
    ) -> io::Result<UploadRetiringCas> {
        Ok(UploadRetiringCas::Unsupported)
    }
}

pub struct R2BlobStore<T: CloudflareBindings> {
    bindings: Arc<T>,
}

impl<T: CloudflareBindings> R2BlobStore<T> {
    pub fn new(bindings: Arc<T>) -> Self {
        Self { bindings }
    }
}

impl<T: CloudflareBindings> BlobStore for R2BlobStore<T> {
    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        validate_key(key)?;
        self.bindings.r2_get(key)
    }

    fn get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        for chunk in chunks {
            validate_key(&chunk.key)?;
        }
        self.bindings.r2_get_many(chunks)
    }

    fn get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
        for chunk in chunks {
            validate_key(&chunk.key)?;
        }
        self.bindings.r2_get_many_verified(chunks)
    }

    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        validate_key(key)?;
        self.bindings.r2_put(key, bytes)
    }

    fn delete(&self, key: &str) -> io::Result<()> {
        validate_key(key)?;
        self.bindings.r2_delete(key)
    }
}

pub struct DurableStateStore<T: CloudflareBindings> {
    bindings: Arc<T>,
}

impl<T: CloudflareBindings> DurableStateStore<T> {
    pub fn new(bindings: Arc<T>) -> Self {
        Self { bindings }
    }
}

impl<T: CloudflareBindings> StateStore for DurableStateStore<T> {
    fn load(&self) -> io::Result<Manifest> {
        self.bindings.manifest_load()
    }

    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
        validate_generation(expected, next)?;
        self.bindings.manifest_cas(expected, next)
    }

    fn try_compare_and_swap_retiring(
        &self,
        expected: u64,
        next: &Manifest,
    ) -> io::Result<RetiringCas> {
        validate_generation(expected, next)?;
        self.bindings.try_compare_and_swap_retiring(expected, next)
    }

    fn try_compare_and_swap_upload_retiring(
        &self,
        expected: u64,
        next: &Manifest,
        pending: &PendingChunk,
    ) -> io::Result<UploadRetiringCas> {
        validate_generation(expected, next)?;
        self.bindings
            .try_compare_and_swap_upload_retiring(expected, next, pending)
    }
}

fn validate_key(key: &str) -> io::Result<()> {
    if key.split('/').any(|part| {
        part.is_empty()
            || part == "."
            || part == ".."
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid blob key",
        ));
    }
    Ok(())
}

fn validate_generation(expected: u64, next: &Manifest) -> io::Result<()> {
    if expected.checked_add(1) != Some(next.generation) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "generation must advance by one",
        ));
    }
    Ok(())
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("storage writer lock poisoned")
}

fn invalid_data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Sync each newly created directory's parent, including nested chunk prefixes.
pub(crate) fn durable_create_dir_all(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        durable_create_dir_all(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error),
    }
    if let Some(parent) = path.parent() {
        File::open(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })?
        .sync_all()?;
    }
    Ok(())
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn durable_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    durable_create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".write-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(bytes: &[u8]) -> Chunk {
        Chunk {
            key: format!("chunks/{}", crate::hash(bytes)),
            len: bytes.len() as u64,
        }
    }

    #[test]
    fn verified_chunks_require_complete_ordered_content_and_exact_consumption() {
        let chunks = [chunk(b"abc"), chunk(b"def"), chunk(b"abc")];
        let data = vec![b"abc".to_vec(), b"def".to_vec(), b"abc".to_vec()];
        assert_eq!(
            VerifiedChunks::verify(&chunks, data.clone())
                .unwrap()
                .into_data(&chunks)
                .unwrap(),
            data
        );
        assert!(
            VerifiedChunks::verify(&[], vec![])
                .unwrap()
                .into_data(&[])
                .unwrap()
                .is_empty()
        );
        for invalid in [
            vec![],
            data[..2].to_vec(),
            [data.clone(), vec![b"extra".to_vec()]].concat(),
            vec![b"ab".to_vec(), b"def".to_vec(), b"abc".to_vec()],
            vec![b"abd".to_vec(), b"def".to_vec(), b"abc".to_vec()],
            vec![b"def".to_vec(), b"abc".to_vec(), b"abc".to_vec()],
        ] {
            assert!(VerifiedChunks::verify(&chunks, invalid).is_err());
        }
        let mut wrong_length = chunks.to_vec();
        wrong_length[0].len += 1;
        let mut wrong_key = chunks.to_vec();
        wrong_key[0] = chunk(b"xyz");
        for expected in [
            chunks[..2].to_vec(),
            vec![chunks[1].clone(), chunks[0].clone(), chunks[2].clone()],
            wrong_length,
            wrong_key,
        ] {
            assert!(
                VerifiedChunks::verify(&chunks, data.clone())
                    .unwrap()
                    .into_data(&expected)
                    .is_err()
            );
        }
        let mut combined = VerifiedChunks::verify(&chunks[..1], data[..1].to_vec()).unwrap();
        combined.append(VerifiedChunks::verify(&chunks[1..], data[1..].to_vec()).unwrap());
        assert_eq!(combined.into_data(&chunks).unwrap(), data);
    }

    struct BatchReads {
        data: Vec<Vec<u8>>,
        batches: Mutex<Vec<Vec<Chunk>>>,
    }

    impl BlobStore for BatchReads {
        fn get(&self, _: &str) -> io::Result<Vec<u8>> {
            unreachable!("verified default must preserve the custom batch getter")
        }
        fn get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
            self.batches.lock().unwrap().push(chunks.to_vec());
            Ok(self.data.clone())
        }
        fn put(&self, _: &str, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> io::Result<()> {
            unreachable!()
        }
    }

    impl CloudflareBindings for BatchReads {
        fn r2_get(&self, _: &str) -> io::Result<Vec<u8>> {
            unreachable!("verified default must preserve the custom batch getter")
        }
        fn r2_get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
            self.get_many(chunks)
        }
        fn r2_put(&self, _: &str, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn r2_delete(&self, _: &str) -> io::Result<()> {
            unreachable!()
        }
        fn manifest_load(&self) -> io::Result<Manifest> {
            unreachable!()
        }
        fn manifest_cas(&self, _: u64, _: &Manifest) -> io::Result<bool> {
            unreachable!()
        }
    }

    #[test]
    fn verified_defaults_preserve_batch_calls_and_reject_untrusted_results() {
        let chunks = [chunk(b"abc"), chunk(b"abc")];
        for (data, valid) in [
            (vec![b"abc".to_vec(); 2], true),
            (vec![b"abc".to_vec()], false),
            (vec![b"abc".to_vec(); 3], false),
            (vec![b"abc".to_vec(), b"abd".to_vec()], false),
            (vec![b"abc".to_vec(), b"ab".to_vec()], false),
        ] {
            let reads = BatchReads {
                data,
                batches: Mutex::default(),
            };
            let blobs: &dyn BlobStore = &reads;
            let bindings: &dyn CloudflareBindings = &reads;
            assert_eq!(blobs.get_many_verified(&chunks).is_ok(), valid);
            assert_eq!(bindings.r2_get_many_verified(&chunks).is_ok(), valid);
            assert_eq!(*reads.batches.lock().unwrap(), vec![chunks.to_vec(); 2]);
        }
    }

    struct VerifiedBindings {
        reads: Mutex<usize>,
    }

    impl CloudflareBindings for VerifiedBindings {
        fn r2_get(&self, _: &str) -> io::Result<Vec<u8>> {
            unreachable!("verified reads must be forwarded")
        }
        fn r2_get_many(&self, _: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
            unreachable!("verified reads must be forwarded")
        }
        fn r2_get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
            *self.reads.lock().unwrap() += 1;
            VerifiedChunks::verify(chunks, vec![b"abc".to_vec(); chunks.len()])
        }
        fn r2_put(&self, _: &str, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn r2_delete(&self, _: &str) -> io::Result<()> {
            unreachable!()
        }
        fn manifest_load(&self) -> io::Result<Manifest> {
            unreachable!()
        }
        fn manifest_cas(&self, _: u64, _: &Manifest) -> io::Result<bool> {
            unreachable!()
        }
    }

    #[test]
    fn r2_wrapper_forwards_verified_reads_after_validating_every_key() {
        let bindings = Arc::new(VerifiedBindings {
            reads: Mutex::new(0),
        });
        let blobs = R2BlobStore::new(bindings.clone());
        let chunks = [chunk(b"abc"), chunk(b"abc")];
        assert_eq!(
            blobs
                .get_many_verified(&chunks)
                .unwrap()
                .into_data(&chunks)
                .unwrap(),
            vec![b"abc".to_vec(); 2]
        );
        let mut invalid = chunks.to_vec();
        invalid[1].key = "../escape".into();
        assert!(blobs.get_many_verified(&invalid).is_err());
        assert_eq!(*bindings.reads.lock().unwrap(), 1);
    }

    struct MockBindings(LocalServices);
    impl CloudflareBindings for MockBindings {
        fn r2_get(&self, key: &str) -> io::Result<Vec<u8>> {
            self.0.blobs.get(key)
        }
        fn r2_put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
            self.0.blobs.put(key, bytes)
        }
        fn r2_delete(&self, key: &str) -> io::Result<()> {
            self.0.blobs.delete(key)
        }
        fn manifest_load(&self) -> io::Result<Manifest> {
            self.0.state.load()
        }
        fn manifest_cas(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
            self.0.state.compare_and_swap(expected, next)
        }
    }

    fn temporary_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "git-zero-storage-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn contract(blobs: &dyn BlobStore, state: &dyn StateStore) {
        assert_eq!(state.load().unwrap(), Manifest::default());
        blobs.put("packs/abc.0", b"compressed").unwrap();
        blobs.put("packs/abc.0", b"compressed").unwrap();
        assert_eq!(blobs.get("packs/abc.0").unwrap(), b"compressed");
        assert_eq!(
            blobs
                .get_many(&[
                    Chunk {
                        key: "packs/abc.0".into(),
                        len: 10
                    },
                    Chunk {
                        key: "packs/abc.0".into(),
                        len: 10
                    },
                ])
                .unwrap(),
            vec![b"compressed".to_vec(), b"compressed".to_vec()]
        );
        assert!(blobs.get_many(&[]).unwrap().is_empty());
        assert_eq!(
            blobs.put("packs/abc.0", b"changed").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let next = Manifest {
            generation: 1,
            refs: BTreeMap::from([("refs/heads/main".into(), "abc".into())]),
            packs: vec![],
            garbage: vec![],
        };
        assert!(state.compare_and_swap(0, &next).unwrap());
        assert!(!state.compare_and_swap(0, &next).unwrap());
        assert_eq!(state.load().unwrap(), next);
        assert_eq!(
            state.compare_and_swap(1, &next).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        for bad in [
            "",
            "../escape",
            "packs/../escape",
            "/absolute",
            "a\\b",
            "a//b",
        ] {
            assert_eq!(
                blobs.put(bad, b"x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                blobs.get(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                blobs.delete(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        blobs.put("garbage/retired", b"unreferenced").unwrap();
        blobs.delete("garbage/retired").unwrap();
        blobs.delete("garbage/retired").unwrap();
        blobs.delete("missing-prefix/retired").unwrap();
        assert_eq!(
            blobs.get("garbage/retired").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    fn engine_contract<B: BlobStore, S: StateStore>(blobs: B, state: S) {
        let mut repository = crate::Repository::new(blobs, state);
        repository.chunk_bytes = 4;
        let previous = repository.state.load().unwrap();
        let pack = repository
            .store_pack("test-pack".into(), b"compressed-pack", b"index", 0)
            .unwrap();
        let mut next = previous.clone();
        next.packs.push(pack);
        assert!(repository.publish(&previous, next).unwrap());
        let current = repository.state.load().unwrap();
        assert_eq!(current.generation, previous.generation + 1);
        let pack = &current.packs[0];
        assert_eq!(
            repository.read_chunks(&pack.pack, pack.pack_bytes).unwrap(),
            b"compressed-pack"
        );
        assert_eq!(
            repository
                .read_chunks(&pack.index, pack.index_bytes)
                .unwrap(),
            b"index"
        );
        let bundle = repository
            .store_bundled_pack("bundled-pack".into(), b"compressed-pack", b"index", 0)
            .unwrap();
        assert_eq!(
            repository.read_pack_files(&bundle).unwrap(),
            (b"compressed-pack".to_vec(), b"index".to_vec())
        );
    }

    #[test]
    fn local_and_cloudflare_wrappers_share_the_storage_contract() {
        let root = temporary_root();
        let local = LocalServices::new(root.join("local")).unwrap();
        contract(&local.blobs, &local.state);
        engine_contract(local.blobs.clone(), local.state.clone());
        let reopened = LocalServices::new(root.join("local")).unwrap();
        assert_eq!(reopened.state.load().unwrap().generation, 2);
        assert_eq!(reopened.blobs.get("packs/abc.0").unwrap(), b"compressed");
        let bindings = Arc::new(MockBindings(
            LocalServices::new(root.join("cloud-mock")).unwrap(),
        ));
        let blobs = R2BlobStore::new(bindings.clone());
        let state = DurableStateStore::new(bindings);
        contract(&blobs, &state);
        engine_contract(blobs, state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_one_concurrent_generation_is_published() {
        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let state = services.state.clone();
                std::thread::spawn(move || {
                    state
                        .compare_and_swap(
                            0,
                            &Manifest {
                                generation: 1,
                                ..Manifest::default()
                            },
                        )
                        .unwrap()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn stored_pack(id: &str, pack_keys: &[&str], index_keys: &[&str]) -> StoredPack {
        let chunks = |keys: &[&str]| {
            keys.iter()
                .map(|key| Chunk {
                    key: (*key).into(),
                    len: 1,
                })
                .collect()
        };
        StoredPack {
            id: id.into(),
            pack: chunks(pack_keys),
            index: chunks(index_keys),
            ..StoredPack::default()
        }
    }

    #[test]
    fn retirement_survives_interruption_and_preserves_shared_chunks() {
        struct FailOnce {
            blobs: LocalBlobStore,
            failed: std::sync::atomic::AtomicBool,
        }
        impl BlobStore for FailOnce {
            fn get(&self, key: &str) -> io::Result<Vec<u8>> {
                self.blobs.get(key)
            }
            fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
                self.blobs.put(key, bytes)
            }
            fn delete(&self, key: &str) -> io::Result<()> {
                if key == "chunks/retired" && !self.failed.swap(true, Ordering::Relaxed) {
                    return Err(io::Error::other("simulated object deletion failure"));
                }
                self.blobs.delete(key)
            }
        }

        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        for key in ["shared", "retired", "old-index", "already-retired", "new"] {
            services.blobs.put(&format!("chunks/{key}"), b"x").unwrap();
        }
        let previous = Manifest {
            generation: 1,
            packs: vec![stored_pack(
                "old",
                &["chunks/shared", "chunks/retired"],
                &["chunks/old-index"],
            )],
            garbage: vec!["chunks/already-retired".into(), "chunks/shared".into()],
            ..Manifest::default()
        };
        assert!(services.state.compare_and_swap(0, &previous).unwrap());
        let mut bundled = stored_pack("new", &["chunks/shared", "chunks/new"], &[]);
        bundled.bundle = std::mem::take(&mut bundled.pack);
        let mut next = Manifest {
            generation: 2,
            packs: vec![bundled],
            ..Manifest::default()
        };
        next.retire_replaced(&previous);
        assert_eq!(
            next.garbage,
            [
                "chunks/already-retired",
                "chunks/old-index",
                "chunks/retired"
            ]
        );
        assert!(services.state.compare_and_swap(1, &next).unwrap());
        let blobs = FailOnce {
            blobs: services.blobs.clone(),
            failed: std::sync::atomic::AtomicBool::new(false),
        };
        assert_eq!(next.collect_garbage(&blobs), ["chunks/retired"]);
        assert_eq!(services.state.load().unwrap(), next);
        let mut recovered = services.state.load().unwrap();
        assert!(recovered.collect_garbage(&blobs).is_empty());
        assert_eq!(blobs.get("chunks/shared").unwrap(), b"x");
        assert_eq!(blobs.get("chunks/new").unwrap(), b"x");
        for key in &next.garbage {
            assert_eq!(blobs.get(key).unwrap_err().kind(), io::ErrorKind::NotFound);
        }
        recovered.garbage.clear();
        let mut unchanged = recovered.clone();
        unchanged.retire_replaced(&recovered);
        assert!(unchanged.garbage.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_scan_removes_unpublished_and_retired_chunks() {
        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        services.blobs.put("chunks/live", b"x").unwrap();
        services.blobs.put("chunks/orphan", b"x").unwrap();
        services.blobs.put("chunks/retired", b"x").unwrap();
        fs::write(root.join("blobs/.write-abandoned"), b"partial").unwrap();
        let current = Manifest {
            packs: vec![stored_pack("current", &["chunks/live"], &[])],
            garbage: vec!["chunks/retired".into()],
            ..Manifest::default()
        };
        assert_eq!(services.blobs.reclaim_unreferenced(&current).unwrap(), 3);
        assert_eq!(services.blobs.get("chunks/live").unwrap(), b"x");
        assert_eq!(services.blobs.reclaim_unreferenced(&current).unwrap(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_cas_loser_recovery_keeps_shared_published_chunks() {
        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        let mut repository = crate::Repository::new(services.blobs.clone(), services.state.clone());
        repository.chunk_bytes = 4;
        let previous = repository.state.load().unwrap();
        let winner = repository
            .store_bundled_pack("winner".into(), b"live-tail", b"win", 0)
            .unwrap();
        let loser = repository
            .store_bundled_pack("loser".into(), b"live-other", b"idx", 0)
            .unwrap();
        assert_eq!(winner.bundle[0].key, loser.bundle[0].key);
        let published = Manifest {
            packs: vec![winner.clone()],
            ..Manifest::default()
        };
        assert!(repository.publish(&previous, published).unwrap());
        assert!(
            !repository
                .publish(
                    &previous,
                    Manifest {
                        packs: vec![loser.clone()],
                        ..Manifest::default()
                    }
                )
                .unwrap()
        );
        // A failed CAS does not authorize deleting any candidate chunks. Only
        // the exclusive recovery scan can distinguish shared live data.
        for chunk in loser.chunks() {
            assert!(repository.blobs.get(&chunk.key).is_ok());
        }
        let current = repository.state.load().unwrap();
        assert_eq!(services.blobs.reclaim_unreferenced(&current).unwrap(), 3);
        assert_eq!(
            repository.read_pack_files(&winner).unwrap(),
            (b"live-tail".to_vec(), b"win".to_vec())
        );
        for chunk in loser.bundle.iter().skip(1) {
            assert_eq!(
                repository.blobs.get(&chunk.key).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
        let mut retired = Manifest::default();
        retired.retire_replaced(&current);
        assert_eq!(retired.garbage.len(), winner.bundle.len());
        assert!(repository.publish(&current, retired).unwrap());
        assert!(
            repository
                .state
                .load()
                .unwrap()
                .collect_garbage(&repository.blobs)
                .is_empty()
        );
        for chunk in winner.chunks() {
            assert_eq!(
                repository.blobs.get(&chunk.key).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immutable_codec_outputs_can_be_imported_without_another_write() {
        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        let source = root.join("codec.pack");
        fs::write(&source, b"compressed").unwrap();
        services
            .blobs
            .put_file("chunks/imported", &source, b"compressed")
            .unwrap();
        let materialized = root.join("restored.pack");
        services
            .blobs
            .materialize_file("chunks/imported", &materialized, b"compressed")
            .unwrap();
        assert_eq!(
            services
                .blobs
                .materialize_file("chunks/imported", &materialized, b"compressed")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        services
            .blobs
            .put_file("chunks/imported", &source, b"compressed")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(&source).unwrap().ino(),
                fs::metadata(root.join("blobs/chunks/imported"))
                    .unwrap()
                    .ino()
            );
            assert_eq!(
                fs::metadata(&source).unwrap().ino(),
                fs::metadata(&materialized).unwrap().ino()
            );
        }
        assert_eq!(
            services
                .blobs
                .put_file("chunks/imported", &source, b"different!")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            services
                .blobs
                .put_file("chunks/other", &source, b"wrong length")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        services.blobs.delete("chunks/imported").unwrap();
        assert_eq!(fs::read(source).unwrap(), b"compressed");
        assert_eq!(fs::read(materialized).unwrap(), b"compressed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_manifests_without_retirement_debt_remain_readable() {
        let legacy = br#"{"generation":1,"refs":{},"packs":[]}"#;
        let manifest: Manifest = serde_json::from_slice(legacy).unwrap();
        assert!(manifest.garbage.is_empty());
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), legacy);
    }

    #[test]
    fn recovery_removes_abandoned_state_snapshots_without_publishing_them() {
        let root = temporary_root();
        let services = LocalServices::new(root.clone()).unwrap();
        let first_temporary = root.join(".write-987654321-0");
        fs::write(&first_temporary, b"interrupted initial snapshot").unwrap();
        services.state.stabilize().unwrap();
        assert!(!first_temporary.exists());
        assert_eq!(services.state.load().unwrap(), Manifest::default());
        assert!(!root.join("manifest.json").exists());

        let published = Manifest {
            generation: 1,
            ..Manifest::default()
        };
        assert!(services.state.compare_and_swap(0, &published).unwrap());
        let published_bytes = fs::read(root.join("manifest.json")).unwrap();
        let abandoned = root.join(".write-987654321-1");
        let unpublished = Manifest {
            generation: 2,
            ..Manifest::default()
        };
        fs::write(&abandoned, serde_json::to_vec(&unpublished).unwrap()).unwrap();
        let unrelated = root.join(".write-keep-this");
        fs::write(&unrelated, b"unrelated file").unwrap();
        let directory = root.join(".write-987654321-2");
        fs::create_dir(&directory).unwrap();
        services.blobs.put("chunks/live", b"durable pack").unwrap();

        services.state.stabilize().unwrap();
        services.state.stabilize().unwrap();
        assert!(!abandoned.exists());
        assert!(unrelated.exists());
        assert!(directory.is_dir());
        assert_eq!(
            fs::read(root.join("manifest.json")).unwrap(),
            published_bytes
        );
        assert_eq!(services.state.load().unwrap(), published);
        assert_eq!(services.blobs.get("chunks/live").unwrap(), b"durable pack");
        fs::remove_dir_all(root).unwrap();
    }
}
