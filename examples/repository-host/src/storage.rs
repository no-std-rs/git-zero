//! Hosting service boundary. Local writers share one `LocalServices` instance
//! (or its clones); independent processes/instances must not write the same root.
//! The clone-shared mutexes serialize individual operations only. Callers provide
//! external per-repository coordination across complete write sequences from
//! blob puts through manifest CAS, readers of older generations, and collection.
//! See [`Manifest::collect_garbage`].
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
            .flat_map(|pack| pack.pack.iter().chain(&pack.index))
            .map(|chunk| chunk.key.as_str())
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
    /// The caller must acquire external per-repository exclusion before loading
    /// this authoritative manifest and hold it until collection returns. Exclude
    /// readers of older generations and every complete write sequence, from
    /// before its first blob put through the manifest CAS. This prevents deletion
    /// of retired keys reused by a staged write before its CAS. Writers may share
    /// access with each other while collection takes exclusive access.
    /// `LocalServices` mutexes cover individual operations only.
    /// Failed or interrupted deletion leaves the published list intact for
    /// idempotent retry; no bucket listing is necessary.
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPack {
    pub id: String,
    pub pack: Vec<Chunk>,
    pub index: Vec<Chunk>,
    pub pack_bytes: u64,
    pub index_bytes: u64,
    pub level: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chunk {
    pub key: String,
    pub len: u64,
}

/// Published blobs are immutable: repeated equal puts succeed, conflicts fail.
/// A successful put must make bytes durable before a manifest can reference them.
pub trait BlobStore: Send + Sync {
    fn get(&self, key: &str) -> io::Result<Vec<u8>>;
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    /// Delete durably; absent keys succeed so recovery can retry partial work.
    fn delete(&self, key: &str) -> io::Result<()>;
}

/// CAS publishes the entire next generation atomically and durably. Missing state
/// is generation zero; success advances by exactly one. A stale CAS returns false.
pub trait StateStore: Send + Sync {
    fn load(&self) -> io::Result<Manifest>;
    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool>;
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
    fn stabilize_existing(path: &Path, verified: &[u8]) -> io::Result<bool> {
        match fs::read(path) {
            Ok(existing) if existing == verified => {
                // A prior attempt may have failed after rename but before sync.
                File::open(path)?.sync_all()?;
                File::open(path.parent().expect("blob has a root"))?.sync_all()?;
                Ok(true)
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "immutable blob conflict",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

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
        install_native_file(&source, destination, verified)
    }

    /// Import a trusted, immutable codec output without writing another copy.
    /// `verified` must contain this exact file's bytes; the caller must keep
    /// the source immutable while this method runs and after a successful link.
    /// Filesystems without hard links fall back to the ordinary durable put.
    pub(crate) fn put_file(&self, key: &str, source: &Path, verified: &[u8]) -> io::Result<()> {
        validate_key(key)?;
        let guard = self.writes.lock().map_err(poisoned)?;
        let path = self.root.join(key);
        if Self::stabilize_existing(&path, verified)? {
            return Ok(());
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
                    .components()
                    .map(|part| {
                        part.as_os_str()
                            .to_str()
                            .ok_or_else(|| io::Error::other("non-UTF-8 blob key"))
                    })
                    .collect::<io::Result<Vec<_>>>()?
                    .join("/");
                if !live.contains(key.as_str()) {
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
        if Self::stabilize_existing(&path, bytes)? {
            return Ok(());
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
    fn r2_put(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
    fn r2_delete(&self, key: &str) -> io::Result<()>;
    fn manifest_load(&self) -> io::Result<Manifest>;
    fn manifest_cas(&self, expected: u64, next: &Manifest) -> io::Result<bool>;
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

/// Install a disposable view file, preferring an immutable shared source inode.
/// Only authoritative imports need a durability barrier; neither path replaces
/// an existing destination or removes a file whose creation it did not own.
pub(crate) fn install_native_file(
    source: &Path,
    destination: &Path,
    verified: &[u8],
) -> io::Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => write_native_file(destination, verified),
    }
}

pub(crate) fn write_native_file(destination: &Path, verified: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = file.write_all(verified);
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
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
    // A failed creation must not remove a pre-existing blob at this name.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
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
        let mut next = Manifest {
            generation: 2,
            packs: vec![stored_pack("new", &["chunks/shared", "chunks/new"], &[])],
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
    fn recovery_scan_preserves_live_chunks_in_existing_repository() {
        let root = temporary_root();
        // Existing directories avoid the local writer's directory-sync support
        // requirements, so recovery's path comparison is exercised on Windows.
        fs::create_dir_all(root.join("blobs/chunks")).unwrap();
        fs::write(root.join("blobs/chunks/live"), b"live").unwrap();
        let services = LocalServices::new(root.clone()).unwrap();
        let current = Manifest {
            packs: vec![stored_pack("current", &["chunks/live"], &[])],
            ..Manifest::default()
        };
        assert_eq!(services.blobs.reclaim_unreferenced(&current).unwrap(), 0);
        assert_eq!(services.blobs.get("chunks/live").unwrap(), b"live");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_scan_distinguishes_unix_backslashes_from_separators() {
        let root = temporary_root();
        fs::create_dir_all(root.join("blobs/chunks")).unwrap();
        fs::write(root.join("blobs/chunks/live"), b"live").unwrap();
        let orphan = root.join(r"blobs/chunks\live");
        fs::write(&orphan, b"orphan").unwrap();
        let services = LocalServices::new(root.clone()).unwrap();
        let current = Manifest {
            packs: vec![stored_pack("current", &["chunks/live"], &[])],
            ..Manifest::default()
        };
        assert_eq!(services.blobs.reclaim_unreferenced(&current).unwrap(), 1);
        assert_eq!(services.blobs.get("chunks/live").unwrap(), b"live");
        assert!(!orphan.exists());
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
