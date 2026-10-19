//! Native codec and local runner. Git subprocesses stay on this side of the
//! service boundary; a Worker needs a portable codec before it can host writes.
use crate::cloudflare::{HostBlobs, HostState, HttpServices};
use crate::http::{Request, Response};
use crate::storage::{
    BlobStore, Chunk, DurableStateStore, LocalServices, Manifest, R2BlobStore, StateStore,
    StoredPack,
};
use crate::{Repository, hash};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

mod codec;
use codec::{Route, Service, git_rpc, incoming_pack, init, receive_advertisement, write_refs};
pub use codec::{command, http_backend, refs};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 256;
const MAX_REPOSITORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REFS: usize = 16 * 1024;
const MAX_REF_BYTES: usize = 4 * 1024 * 1024;
const COMPACTION_BYTES: u64 = 8 * 1024 * 1024;
const FANOUT: usize = 4;

fn validate_snapshot_refs(refs: &BTreeMap<String, String>) -> io::Result<()> {
    if refs.len() > MAX_REFS
        || refs
            .iter()
            .try_fold(0usize, |bytes, (name, id)| {
                bytes.checked_add(name.len())?.checked_add(id.len())
            })
            .is_none_or(|bytes| bytes > MAX_REF_BYTES)
    {
        return Err(io::Error::other("prototype ref limit exceeded"));
    }
    Ok(())
}

fn validate_snapshot_bytes(mut sizes: impl Iterator<Item = (u64, u64)>) -> io::Result<()> {
    if sizes
        .try_fold(0u64, |total, (pack, index)| {
            total.checked_add(pack)?.checked_add(index)
        })
        .is_none_or(|total| total > MAX_REPOSITORY_BYTES)
    {
        return Err(io::Error::other(
            "prototype repository exceeds 128 MiB of packs and indexes",
        ));
    }
    Ok(())
}

fn file_matches(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0u8; 64 * 1024];
    for expected in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..expected.len()])?;
        if &buffer[..expected.len()] != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn install_native_file(source: &Path, destination: &Path, verified: &[u8]) -> io::Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            // Native views are disposable. Only the authoritative import needs
            // a durability barrier; a failed copy may remove only our new file.
            let mut file = fs::OpenOptions::new()
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
    }
}

#[derive(Default, Serialize)]
pub struct Metrics {
    pub requests: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub compactions: u64,
    pub compaction_input_bytes: u64,
    pub compaction_output_bytes: u64,
    pub blob_puts: u64,
    pub blob_put_bytes: u64,
    pub inline_maintenance_seconds: f64,
    pub receive_codec_seconds: f64,
    pub receive_storage_seconds: f64,
    pub receive_reused_pack_bytes: u64,
    pub receive_publication_seconds: f64,
    pub receive_prepare_seconds: f64,
    pub upload_codec_seconds: f64,
    pub retired_chunk_deletes: u64,
    pub cache_bytes: u64,
    pub cache_budget_bytes: u64,
    pub cache_entries: u64,
    pub cache_entry_limit: u64,
    pub discovery_cache_hits: u64,
    pub cloud_view_rehydrates: u64,
}

struct CandidatePack {
    stored: Option<StoredPack>,
    id: String,
    level: u32,
    pack: PathBuf,
    index: PathBuf,
    bytes: u64,
}

fn compaction_selection(packs: &[CandidatePack], level: u32) -> Vec<usize> {
    let mut candidates: Vec<_> = packs
        .iter()
        .enumerate()
        .filter(|(_, pack)| pack.level == level)
        .map(|(index, pack)| (pack.bytes, index))
        .collect();
    candidates.sort_unstable();
    let mut selected: Vec<_> = candidates
        .into_iter()
        .take(FANOUT)
        .map(|(_, index)| index)
        .collect();
    // Keep removal safe, independently of size order. A large old pack must not
    // prevent smaller later appends from receiving their maintenance budget.
    selected.sort_unstable();
    selected
}
struct Quarantine(PathBuf);
impl Drop for Quarantine {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct NativeHost {
    root: PathBuf,
    view: PathBuf,
    repository: Repository<HostBlobs, HostState>,
    cloudflare: Option<Arc<HttpServices>>,
    cold_view: bool,
    cache: VecDeque<(String, Response)>,
    cache_bytes: usize,
    view_dirty: bool,
    manifest: Manifest,
    pub metrics: Metrics,
}

impl NativeHost {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        crate::storage::durable_create_dir_all(&root)?;
        let root = root.canonicalize()?;
        if let Some(parent) = root.parent() {
            File::open(parent)?.sync_all()?;
        }
        let services = LocalServices::new(root.join("services"))?;
        let mut repository = Repository::new(
            HostBlobs::Local(services.blobs),
            HostState::Local(services.state),
        );
        // A 16 MiB local chunk keeps ordinary imported packs on the zero-copy
        // inode path. The shared storage engine keeps this a policy parameter.
        repository.chunk_bytes = 16 * 1024 * 1024;
        Self::with_services(root, repository, None, false)
    }

    pub fn open_cloudflare(
        root: PathBuf,
        origin: &str,
        repo: &str,
        token: &str,
        cold_view: bool,
    ) -> io::Result<Self> {
        crate::storage::durable_create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let services = Arc::new(HttpServices::new(origin, repo, token)?);
        let repository = Repository::new(
            HostBlobs::Cloudflare(R2BlobStore::new(services.clone()), services.clone()),
            HostState::Cloudflare(DurableStateStore::new(services.clone())),
        );
        Self::with_services(root, repository, Some(services), cold_view)
    }

    fn with_services(
        root: PathBuf,
        repository: Repository<HostBlobs, HostState>,
        cloudflare: Option<Arc<HttpServices>>,
        cold_view: bool,
    ) -> io::Result<Self> {
        let view = root.join("view/repo.git");
        let mut host = Self {
            root,
            view,
            repository,
            cloudflare,
            cold_view,
            cache: VecDeque::new(),
            cache_bytes: 0,
            view_dirty: false,
            manifest: Manifest::default(),
            metrics: Metrics::default(),
        };
        host.recover()?;
        Ok(host)
    }

    fn recover(&mut self) -> io::Result<()> {
        self.repository.state.stabilize()?;
        let mut manifest = self.repository.state.load()?;
        validate_snapshot_refs(&manifest.refs)?;
        validate_snapshot_bytes(manifest.packs.iter().map(|p| (p.pack_bytes, p.index_bytes)))?;
        self.rebuild_view(&manifest)?;
        // Cleanup may fail after rebuilding. Retry recovery until the view,
        // in-memory manifest and cached responses agree on this generation.
        self.view_dirty = true;
        self.metrics.retired_chunk_deletes +=
            self.repository.blobs.reclaim_unreferenced(&manifest)? as u64;
        manifest.garbage.clear();
        self.manifest = manifest;
        self.cache.clear();
        self.cache_bytes = 0;
        self.view_dirty = false;
        Ok(())
    }

    fn rebuild_view(&mut self, manifest: &Manifest) -> io::Result<()> {
        self.view_dirty = true;
        // The alternate object DB must contain only published data. In
        // particular, a pre-CAS crash can leave native pack links that are not
        // in the authoritative manifest. Never let a later push depend on them.
        if self.view.exists() {
            fs::remove_dir_all(&self.view)?;
        }
        init(&self.view)?;
        for pack in &manifest.packs {
            let directory = self.view.join("objects/pack");
            fs::create_dir_all(&directory)?;
            for (extension, chunks, length) in [
                ("pack", &pack.pack, pack.pack_bytes),
                ("idx", &pack.index, pack.index_bytes),
            ] {
                let bytes = self.repository.read_chunks(chunks, length)?;
                let path = directory.join(format!("pack-{}.{extension}", pack.id));
                if chunks.len() == 1 {
                    self.repository
                        .blobs
                        .materialize_file(&chunks[0].key, &path, &bytes)?;
                } else {
                    fs::write(path, bytes)?;
                }
            }
        }
        write_refs(&self.view, &manifest.refs)?;
        self.view_dirty = false;
        Ok(())
    }

    fn cloud_read_view(&mut self) -> io::Result<()> {
        if self.cloudflare.is_some() && self.cold_view {
            self.rebuild_view(&self.manifest.clone())?;
            self.metrics.cloud_view_rehydrates += 1;
        }
        Ok(())
    }

    fn import_pack(
        &self,
        pack: &CandidatePack,
        data: &[u8],
        index: &[u8],
        pack_keys: Option<&[String]>,
    ) -> io::Result<StoredPack> {
        let store =
            |source: &Path, bytes: &[u8], keys: Option<&[String]>| -> io::Result<Vec<Chunk>> {
                bytes
                    .chunks(self.repository.chunk_bytes)
                    .enumerate()
                    .map(|(position, bytes)| {
                        let key = keys
                            .map(|keys| keys[position].clone())
                            .unwrap_or_else(|| format!("chunks/{}", hash(bytes)));
                        if position == 0 && source.metadata()?.len() == bytes.len() as u64 {
                            self.repository.blobs.put_file(&key, source, bytes)?;
                        } else {
                            self.repository.blobs.put(&key, bytes)?;
                        }
                        Ok(Chunk {
                            key,
                            len: bytes.len() as u64,
                        })
                    })
                    .collect()
            };
        Ok(StoredPack {
            id: pack.id.clone(),
            pack: store(&pack.pack, data, pack_keys)?,
            index: store(&pack.index, index, None)?,
            pack_bytes: data.len() as u64,
            index_bytes: index.len() as u64,
            level: pack.level,
        })
    }

    fn prune_native_packs(&self) -> io::Result<()> {
        let live: std::collections::BTreeSet<_> = self
            .manifest
            .packs
            .iter()
            .map(|pack| pack.id.as_str())
            .collect();
        for entry in fs::read_dir(self.view.join("objects/pack"))? {
            let entry = entry?;
            let path = entry.path();
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if stem
                .strip_prefix("pack-")
                .is_some_and(|id| !live.contains(id))
            {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    fn native_pack(&self, stored: &StoredPack) -> CandidatePack {
        CandidatePack {
            stored: Some(stored.clone()),
            id: stored.id.clone(),
            level: stored.level,
            pack: self
                .view
                .join(format!("objects/pack/pack-{}.pack", stored.id)),
            index: self
                .view
                .join(format!("objects/pack/pack-{}.idx", stored.id)),
            bytes: stored.pack_bytes,
        }
    }

    fn compact(&mut self, stage: &Path, packs: &mut Vec<CandidatePack>) -> io::Result<()> {
        for level in 0..16 {
            let selected = compaction_selection(packs, level);
            let bytes: u64 = selected.iter().map(|i| packs[*i].bytes).sum();
            if selected.len() < FANOUT || bytes > COMPACTION_BYTES {
                continue;
            }
            let start = Instant::now();
            let sources: Vec<_> = selected
                .iter()
                .map(|index| {
                    Ok((
                        fs::read(&packs[*index].pack)?,
                        fs::read(&packs[*index].index)?,
                    ))
                })
                .collect::<io::Result<_>>()?;
            let views: Vec<_> = sources
                .iter()
                .map(|(pack, index)| (pack.as_slice(), index.as_slice()))
                .collect();
            let (id, data, index_data) = crate::packed::merge_packs(&views)?;
            let path = stage.join(format!("objects/pack/pack-{id}.pack"));
            let index = path.with_extension("idx");
            fs::write(&path, &data)?;
            fs::write(&index, index_data)?;
            let result_bytes = data.len() as u64;
            for i in selected.into_iter().rev() {
                packs.remove(i);
            }
            packs.push(CandidatePack {
                stored: None,
                id,
                level: level + 1,
                pack: path,
                index,
                bytes: result_bytes,
            });
            self.metrics.compactions += 1;
            self.metrics.compaction_input_bytes += bytes;
            self.metrics.compaction_output_bytes += result_bytes;
            self.metrics.inline_maintenance_seconds += start.elapsed().as_secs_f64();
            // One bounded job per push, never a full-history repack by default.
            break;
        }
        Ok(())
    }

    fn receive(&mut self, request: &Request, route: Route) -> io::Result<Response> {
        let prepare_start = Instant::now();
        self.cloud_read_view()?;
        let garbage = self.manifest.garbage.len();
        let failed = self.manifest.collect_garbage(&self.repository.blobs);
        self.metrics.retired_chunk_deletes += (garbage - failed.len()) as u64;
        // Remember completed retries without changing the durable retirement
        // debt, which remains available for recovery until the next publication.
        self.manifest.garbage = failed;
        if !self.manifest.garbage.is_empty() {
            return Err(io::Error::other(
                "retired chunk cleanup must complete before another push",
            ));
        }
        let previous = self.manifest.clone();
        let quarantine = Quarantine(self.root.join("quarantine"));
        if quarantine.0.exists() {
            fs::remove_dir_all(&quarantine.0)?;
        }
        let stage = quarantine.0.join("repo.git");
        init(&stage)?;
        fs::create_dir_all(stage.join("objects/info"))?;
        fs::write(
            stage.join("objects/info/alternates"),
            // Git resolves relative alternates from the object database. This
            // fixed sibling layout also handles non-UTF-8 and newline roots.
            b"../../../view/repo.git/objects\n",
        )?;
        write_refs(&stage, &previous.refs)?;
        self.metrics.receive_prepare_seconds += prepare_start.elapsed().as_secs_f64();
        let codec_start = Instant::now();
        // Hash large input streams while strict native validation runs. Reuse
        // only after comparing every byte of the immutable codec output. Thin
        // pack completion or compaction takes the ordinary output-hashing path.
        let input_pack = incoming_pack(&request.body).filter(|bytes| bytes.len() >= 256 * 1024);
        let chunk_bytes = self.repository.chunk_bytes;
        let (response, input_keys) = if let Some(bytes) = input_pack {
            std::thread::scope(|scope| -> io::Result<_> {
                let worker = scope.spawn(|| {
                    bytes
                        .chunks(chunk_bytes)
                        .map(|chunk| format!("chunks/{}", hash(chunk)))
                        .collect::<Vec<_>>()
                });
                let response = git_rpc(&stage, request, route);
                let keys = worker
                    .join()
                    .map_err(|_| io::Error::other("pack checksum thread failed"))?;
                Ok((response?, Some(keys)))
            })?
        } else {
            (git_rpc(&stage, request, route)?, None)
        };
        let next_refs = refs(&stage)?;
        self.metrics.receive_codec_seconds += codec_start.elapsed().as_secs_f64();
        if response.status != 200 || next_refs == previous.refs {
            return Ok(response);
        }
        validate_snapshot_refs(&next_refs)?;
        self.view_dirty = true;
        let mut packs: Vec<_> = previous.packs.iter().map(|p| self.native_pack(p)).collect();
        for entry in fs::read_dir(stage.join("objects/pack"))? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("pack") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .trim_start_matches("pack-")
                .to_string();
            if !packs.iter().any(|p| p.id == id) {
                packs.push(CandidatePack {
                    stored: None,
                    id,
                    level: 0,
                    index: path.with_extension("idx"),
                    bytes: path.metadata()?.len(),
                    pack: path,
                });
            }
        }
        let sizes = packs
            .iter()
            .map(|pack| Ok((pack.bytes, pack.index.metadata()?.len())))
            .collect::<io::Result<Vec<_>>>()?;
        validate_snapshot_bytes(sizes.into_iter())?;
        self.compact(&stage, &mut packs)?;
        let sizes = packs
            .iter()
            .map(|pack| Ok((pack.bytes, pack.index.metadata()?.len())))
            .collect::<io::Result<Vec<_>>>()?;
        validate_snapshot_bytes(sizes.into_iter())?;
        let storage_start = Instant::now();
        let mut next = Manifest {
            generation: previous.generation + 1,
            refs: next_refs,
            packs: Vec::new(),
            garbage: Vec::new(),
        };
        for pack in packs {
            let stored = if let Some(stored) = pack.stored.clone() {
                stored
            } else {
                let input = match input_pack {
                    Some(bytes) if file_matches(&pack.pack, bytes)? => Some(bytes),
                    _ => None,
                };
                let data;
                let bytes = if let Some(bytes) = input {
                    self.metrics.receive_reused_pack_bytes += bytes.len() as u64;
                    bytes
                } else {
                    data = fs::read(&pack.pack)?;
                    &data
                };
                let index = fs::read(&pack.index)?;
                let keys = input.and(input_keys.as_deref());
                let stored = self.import_pack(&pack, bytes, &index, keys)?;
                self.metrics.blob_puts += (stored.pack.len() + stored.index.len()) as u64;
                self.metrics.blob_put_bytes += (bytes.len() + index.len()) as u64;
                for (source, extension, verified) in [
                    (&pack.pack, "pack", bytes),
                    (&pack.index, "idx", index.as_slice()),
                ] {
                    let destination = self
                        .view
                        .join(format!("objects/pack/pack-{}.{extension}", pack.id));
                    if !destination.exists() {
                        install_native_file(source, &destination, verified)?;
                    }
                }
                stored
            };
            next.packs.push(stored);
        }
        self.metrics.receive_storage_seconds += storage_start.elapsed().as_secs_f64();
        let publication_start = Instant::now();
        next.retire_replaced(&previous);
        // A manifest write can become visible before an I/O error is reported.
        // Recover from authoritative state before serving any later request.
        self.view_dirty = true;
        if !self.repository.publish(&previous, next.clone())? {
            return Err(io::Error::other("concurrent generation update"));
        }
        // Publication is authoritative. A restart reconstructs refs from it if
        // the process dies while updating this disposable native view.
        write_refs(&self.view, &next.refs)?;
        self.manifest = next;
        self.prune_native_packs()?;
        let garbage = self.manifest.garbage.len();
        let failed = self.manifest.collect_garbage(&self.repository.blobs);
        self.metrics.retired_chunk_deletes += (garbage - failed.len()) as u64;
        // Failed deletions remain in the durable manifest and are retried
        // before accepting more data; publication itself is already durable.
        self.manifest.garbage = failed;
        self.metrics.receive_publication_seconds += publication_start.elapsed().as_secs_f64();
        self.view_dirty = false;
        self.cache.clear();
        self.cache_bytes = 0;
        Ok(response)
    }

    fn dispatch(&mut self, request: &Request) -> io::Result<Response> {
        let route = Route::new(request);
        if let Some(services) = &self.cloudflare {
            services.set_phase(if route.push { "push" } else { "fetch" })?;
        }
        if self.view_dirty {
            self.recover()?;
        }
        if self.cloudflare.is_some() {
            let manifest = self.repository.state.load()?;
            if manifest.generation != self.manifest.generation {
                self.recover()?;
            }
        }
        if route.service == Some(Service::Receive) && !route.advertise {
            return self.receive(request, route);
        }
        if route.service == Some(Service::Upload) && !route.advertise {
            let key = self.cache_key(request);
            if let Some((_, response)) = self.cache.iter().find(|(k, _)| k == &key) {
                self.metrics.cache_hits += 1;
                return Ok(response.clone());
            }
            self.metrics.cache_misses += 1;
            self.cloud_read_view()?;
            let codec_start = Instant::now();
            let response = git_rpc(&self.view, request, route)?;
            self.metrics.upload_codec_seconds += codec_start.elapsed().as_secs_f64();
            self.cache_insert(key, &response);
            return Ok(response);
        }
        if route.advertise {
            let key = self.cache_key(request);
            if let Some((_, response)) = self.cache.iter().find(|(entry, _)| entry == &key) {
                self.metrics.discovery_cache_hits += 1;
                return Ok(response.clone());
            }
            let protocol = request
                .headers
                .get("git-protocol")
                .map(String::as_str)
                .unwrap_or("");
            let response = if route.service == Some(Service::Receive)
                && (protocol.is_empty() || protocol == "version=0")
            {
                receive_advertisement(&self.manifest.refs)?
            } else {
                self.cloud_read_view()?;
                git_rpc(&self.view, request, route)?
            };
            self.cache_insert(key, &response);
            return Ok(response);
        }
        git_rpc(&self.view, request, route)
    }

    fn cache_key(&self, request: &Request) -> String {
        let mut digest = Sha256::new();
        digest.update(self.manifest.generation.to_le_bytes());
        for field in [
            request.method.as_bytes(),
            request.target.as_bytes(),
            request
                .headers
                .get("git-protocol")
                .map_or(b"".as_slice(), |value| value.as_bytes()),
            request
                .headers
                .get("content-type")
                .map_or(b"".as_slice(), |value| value.as_bytes()),
            request
                .headers
                .get("content-encoding")
                .map_or(b"".as_slice(), |value| value.as_bytes()),
            &request.body,
        ] {
            digest.update((field.len() as u64).to_le_bytes());
            digest.update(field);
        }
        format!("{:x}", digest.finalize())
    }

    fn cache_weight(key: &str, response: &Response) -> usize {
        key.len()
            + response.body.len()
            + response
                .headers
                .iter()
                .map(|(name, value)| name.len() + value.len())
                .sum::<usize>()
            + 256
    }

    fn cache_insert(&mut self, key: String, response: &Response) {
        if response.status != 200
            || response.body.is_empty()
            || response.body.len() > CACHE_ENTRY_BYTES
        {
            return;
        }
        let weight = Self::cache_weight(&key, response);
        if weight > CACHE_BYTES {
            return;
        }
        while self.cache_bytes + weight > CACHE_BYTES || self.cache.len() >= CACHE_ENTRIES {
            if let Some((old_key, old)) = self.cache.pop_front() {
                self.cache_bytes -= Self::cache_weight(&old_key, &old);
            } else {
                break;
            }
        }
        self.cache_bytes += weight;
        self.cache.push_back((key, response.clone()));
    }

    pub fn handle(&mut self, request: Request) -> Response {
        if request.method == "GET"
            && matches!(request.target.as_str(), "/metrics" | "/_object-stats")
        {
            let result = (|| -> io::Result<serde_json::Value> {
                if let Some(services) = &self.cloudflare {
                    services.set_phase("stats")?;
                }
                if self.view_dirty {
                    self.recover()?;
                }
                self.metrics.cache_bytes = self.cache_bytes as u64;
                self.metrics.cache_budget_bytes = CACHE_BYTES as u64;
                self.metrics.cache_entries = self.cache.len() as u64;
                self.metrics.cache_entry_limit = CACHE_ENTRIES as u64;
                let mut value = serde_json::to_value(&self.metrics)?;
                value["native_view_policy"] = serde_json::json!(if self.cold_view {
                    "full-rehydrate-on-codec-cache-miss"
                } else {
                    "persistent-native-view-up-to128MiB-packs-and-indexes"
                });
                if request.target == "/_object-stats" {
                    value["manifest"] = serde_json::to_value(&self.manifest)?;
                    value["objects"] = crate::objects::inventory(&self.view, &self.manifest)?;
                }
                if let Some(services) = &self.cloudflare {
                    let usage = services.usage()?;
                    if request.target == "/_object-stats" {
                        let counters = serde_json::from_value(usage.clone())?;
                        value["estimated_cost"] = serde_json::to_value(
                            crate::cost::estimate_service_usage(&counters, 1000)?,
                        )?;
                    }
                    value["cloudflare_services"] = usage;
                }
                Ok(value)
            })();
            return match result {
                Ok(value) => Response {
                    status: 200,
                    headers: BTreeMap::from([("Content-Type".into(), "application/json".into())]),
                    body: serde_json::to_vec(&value).unwrap_or_default().into(),
                },
                Err(error) => {
                    eprintln!("statistics failed: {error}");
                    Response::error(503, "statistics failed")
                }
            };
        }
        self.metrics.requests += 1;
        self.metrics.input_bytes += request.body.len() as u64;
        let response = self.dispatch(&request).unwrap_or_else(|error| {
            eprintln!("hosting request failed: {error}");
            Response::error(503, "repository operation failed")
        });
        self.metrics.output_bytes += response.body.len() as u64;
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> Quarantine {
        Quarantine(std::env::temp_dir().join(format!(
            "git-zero-native-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )))
    }

    #[test]
    fn recovery_cleanup_is_not_counted_again_before_a_push() {
        let directory = temporary_directory();
        let services = LocalServices::new(directory.0.join("services")).unwrap();
        let retired = "chunks/retired";
        services.blobs.put(retired, b"retired").unwrap();
        let published = Manifest {
            generation: 1,
            garbage: vec![retired.into()],
            ..Manifest::default()
        };
        assert!(services.state.compare_and_swap(0, &published).unwrap());
        let mut host = NativeHost::open(directory.0.clone()).unwrap();
        assert_eq!(host.metrics.retired_chunk_deletes, 1);
        assert_eq!(host.repository.state.load().unwrap(), published);

        let response = host.handle(Request {
            method: "POST".into(),
            target: "/repo.git/git-receive-pack".into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-git-receive-pack-request".into(),
            )]),
            body: b"0000".to_vec(),
        });
        assert_eq!(response.status, 200);
        assert_eq!(host.metrics.retired_chunk_deletes, 1);
        assert!(host.manifest.garbage.is_empty());
        assert_eq!(host.repository.state.load().unwrap(), published);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_installation_copies_when_hard_links_cross_devices() {
        use std::os::unix::fs::MetadataExt;

        let directory = temporary_directory();
        fs::create_dir(&directory.0).unwrap();
        let source = Path::new("/proc/self/cmdline");
        let verified = fs::read(source).unwrap();
        assert!(!verified.is_empty());
        let copied = directory.0.join("copied.pack");
        assert_eq!(
            fs::hard_link(source, &copied).unwrap_err().kind(),
            io::ErrorKind::CrossesDevices
        );
        install_native_file(source, &copied, &verified).unwrap();
        assert_eq!(fs::read(&copied).unwrap(), verified);
        assert_eq!(
            install_native_file(source, &copied, b"replacement")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&copied).unwrap(), verified);

        let source = directory.0.join("source.pack");
        fs::write(&source, &verified).unwrap();
        let linked = directory.0.join("linked.pack");
        install_native_file(&source, &linked, &verified).unwrap();
        assert_eq!(
            fs::metadata(source).unwrap().ino(),
            fs::metadata(linked).unwrap().ino()
        );
    }

    #[test]
    fn derived_head_prefers_main_then_master_then_the_first_branch() {
        let directory = temporary_directory();
        init(&directory.0).unwrap();
        let id = "a".repeat(40);
        for (names, expected) in [
            (
                vec!["refs/heads/zeta", "refs/heads/alpha"],
                "refs/heads/alpha",
            ),
            (
                vec!["refs/heads/alpha", "refs/heads/master"],
                "refs/heads/master",
            ),
            (
                vec!["refs/heads/master", "refs/heads/main", "refs/heads/alpha"],
                "refs/heads/main",
            ),
            (vec!["refs/tags/release"], "refs/heads/main"),
            (vec![], "refs/heads/main"),
        ] {
            let wanted = names
                .into_iter()
                .map(|name| (name.into(), id.clone()))
                .collect();
            write_refs(&directory.0, &wanted).unwrap();
            assert_eq!(
                fs::read_to_string(directory.0.join("HEAD")).unwrap(),
                format!("ref: {expected}\n")
            );
        }
    }

    #[test]
    fn retirement_retries_count_partial_and_complete_cleanup_once() {
        let directory = temporary_directory();
        let mut host = NativeHost::open(directory.0.clone()).unwrap();
        let removed_key = "chunks/removed";
        let blocked_key = "chunks/blocked";
        host.repository.blobs.put(removed_key, b"retired").unwrap();
        let blob_root = directory.0.join("services/blobs");
        let blocked = blob_root.join(blocked_key);
        fs::create_dir(&blocked).unwrap();
        let published = Manifest {
            generation: 1,
            garbage: vec![removed_key.into(), blocked_key.into()],
            ..Manifest::default()
        };
        assert!(
            host.repository
                .state
                .compare_and_swap(0, &published)
                .unwrap()
        );
        host.manifest = published.clone();
        let push = || Request {
            method: "POST".into(),
            target: "/repo.git/git-receive-pack".into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-git-receive-pack-request".into(),
            )]),
            body: b"0000".to_vec(),
        };

        // One deletion succeeds even though another blocks the next push.
        assert_eq!(host.handle(push()).status, 503);
        assert!(!blob_root.join(removed_key).exists());
        assert_eq!(host.metrics.retired_chunk_deletes, 1);
        assert_eq!(host.manifest.garbage, [blocked_key]);
        assert_eq!(host.repository.state.load().unwrap(), published);

        // Retrying the blocked key must not recount the completed deletion.
        assert_eq!(host.handle(push()).status, 503);
        assert_eq!(host.metrics.retired_chunk_deletes, 1);
        fs::remove_dir(&blocked).unwrap();
        host.repository.blobs.put(blocked_key, b"retired").unwrap();
        assert_eq!(host.handle(push()).status, 200);
        assert!(!blocked.exists());
        assert_eq!(host.metrics.retired_chunk_deletes, 2);
        assert!(host.manifest.garbage.is_empty());

        // With no ref update there is no CAS, so durable debt remains available
        // for recovery while this host remembers that cleanup already finished.
        assert_eq!(host.repository.state.load().unwrap(), published);
        assert_eq!(host.handle(push()).status, 200);
        assert_eq!(host.metrics.retired_chunk_deletes, 2);
    }

    #[test]
    fn snapshot_limits_include_ref_only_growth_and_indexes() {
        let mut refs: BTreeMap<_, _> = (0..MAX_REFS)
            .map(|index| (format!("refs/heads/{index}"), "a".repeat(40)))
            .collect();
        assert!(validate_snapshot_refs(&refs).is_ok());
        refs.insert("refs/heads/overflow".into(), "a".repeat(40));
        assert!(validate_snapshot_refs(&refs).is_err());
        let oversized = BTreeMap::from([("r".repeat(MAX_REF_BYTES), "a".repeat(40))]);
        assert!(validate_snapshot_refs(&oversized).is_err());
        assert!(validate_snapshot_bytes([(MAX_REPOSITORY_BYTES - 1024, 1024)].into_iter()).is_ok());
        assert!(validate_snapshot_bytes([(MAX_REPOSITORY_BYTES, 1)].into_iter()).is_err());
        assert!(validate_snapshot_bytes([(1, u64::MAX)].into_iter()).is_err());
    }

    #[test]
    fn checksum_reuse_requires_exact_file_bytes() {
        let path =
            std::env::temp_dir().join(format!("git-zero-byte-comparison-{}", std::process::id()));
        let mut bytes = vec![7u8; 128 * 1024 + 31];
        fs::write(&path, &bytes).unwrap();
        assert!(file_matches(&path, &bytes).unwrap());
        assert!(!file_matches(&path, &bytes[..bytes.len() - 1]).unwrap());
        bytes[64 * 1024 + 3] ^= 1;
        assert!(!file_matches(&path, &bytes).unwrap());
        fs::remove_file(path).unwrap();
        assert_eq!(
            incoming_pack(b"0008test0000PACKbytes"),
            Some(b"PACKbytes".as_slice())
        );
        assert_eq!(incoming_pack(b"0008test0000"), None);
        assert_eq!(incoming_pack(b"zzzzPACKbytes"), None);
    }

    #[test]
    fn large_old_pack_does_not_starve_small_appends() {
        let packs: Vec<_> = [COMPACTION_BYTES + 1, 100, 400, 200, 300]
            .into_iter()
            .map(|bytes| CandidatePack {
                stored: None,
                id: String::new(),
                level: 0,
                pack: PathBuf::new(),
                index: PathBuf::new(),
                bytes,
            })
            .collect();
        let selected = compaction_selection(&packs, 0);
        assert_eq!(selected, vec![1, 2, 3, 4]);
        assert!(
            selected
                .iter()
                .map(|index| packs[*index].bytes)
                .sum::<u64>()
                <= COMPACTION_BYTES
        );
    }
}
