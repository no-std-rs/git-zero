//! Native codec and local runner. Git subprocesses stay on this side of the
//! service boundary; a Worker needs a portable codec before it can host writes.
use crate::cloudflare::{HostBlobs, HostState, HttpServices};
use crate::http::{Request, Response};
use crate::storage::{
    BlobStore, Chunk, DurableStateStore, LocalServices, Manifest, R2BlobStore, StateStore,
    StoredPack,
};
use crate::{Repository, hash};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Instant;

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 256;
const MAX_REPOSITORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_REFS: usize = 16 * 1024;
const MAX_REF_BYTES: usize = 4 * 1024 * 1024;
const COMPACTION_BYTES: u64 = 8 * 1024 * 1024;
const FANOUT: usize = 4;
const NATIVE_CONFIG: &[u8] = b"[core]\nrepositoryformatversion = 0\nbare = true\nfsync = none\n[http]\nreceivepack = true\n[receive]\nunpackLimit = 0\nfsckObjects = true\ndenyNonFastForwards = true\ndenyDeleteCurrent = ignore\nautogc = false\n[gc]\nauto = 0\n[pack]\nthreads = 0\nwindow = 0\nuseBitmaps = true\n";

mod advertisement;
#[cfg(test)]
mod advertisement_tests;
mod history;
#[cfg(test)]
mod history_tests;

pub fn command(mut command: Command, input: &[u8]) -> io::Result<Output> {
    crate::diagnostics::observe("git_child", |diagnostic| {
        diagnostic.mark("begin");
        diagnostic.field(
            "service",
            command
                .get_args()
                .filter_map(|a| a.to_str())
                .find(|a| {
                    matches!(
                        *a,
                        "upload-pack"
                            | "receive-pack"
                            | "http-backend"
                            | "index-pack"
                            | "pack-objects"
                    )
                })
                .unwrap_or("other-git"),
        );
        diagnostic.field("stdin_bytes", input.len());

        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        diagnostic.mark("child_spawn_begin");
        let mut child = command.spawn()?;
        diagnostic.field("child_pid", child.id());
        diagnostic.mark("child_spawned");
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no child input"))?;
        let result = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(input));
            diagnostic.mark("child_wait_begin");
            let output = child.wait_with_output();
            diagnostic.mark("child_exit_observed_output_collected");
            if let Ok(ref result) = output {
                diagnostic.field("exit_code", serde_json::json!(result.status.code()));
                diagnostic.field("exit_success", result.status.success());
                diagnostic.field("stdout_bytes", result.stdout.len());
                diagnostic.field("stderr_bytes", result.stderr.len());
            }
            let written = writer
                .join()
                .map_err(|_| io::Error::other("child input thread failed"))?;
            // A rejected Git request can close stdin early. Its exit/status is the
            // useful diagnostic in that case.
            if let Ok(ref output) = output {
                if output.status.success() {
                    if written.is_err() {
                        diagnostic.mark("child_stdin_write_failed");
                    }
                    written?;
                }
            }
            output
        })?;
        if !result.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&result.stderr).into_owned(),
            ));
        }
        Ok(result)
    })
}

fn init(repo: &Path) -> io::Result<()> {
    // This is a disposable Git codec view, not the authoritative ref store.
    // Its fixed bare layout needs no hooks, templates or per-push Git process.
    for path in ["objects/pack", "objects/info", "refs"] {
        fs::create_dir_all(repo.join(path))?;
    }
    fs::write(repo.join("HEAD"), b"ref: refs/heads/main\n")?;
    // This disposable codec view has a fixed policy. Write it once instead of
    // spawning a config process for every setting on every quarantined push.
    // Quarantine refs and this view are disposable. Authoritative blob import
    // syncs validated pack/index bytes, then manifest CAS syncs the publication.
    // Do not pay for a second durability transaction on temporary codec refs.
    // Existing compressed entries/deltas are reused; a download never starts
    // a fresh delta search proportional to the size of the requested history.
    fs::write(repo.join("config"), NATIVE_CONFIG)?;
    Ok(())
}

pub fn refs(repo: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    match fs::read_to_string(repo.join("packed-refs")) {
        Ok(packed) => {
            for line in packed
                .lines()
                .filter(|line| !line.starts_with(['#', '^']) && !line.is_empty())
            {
                let (id, name) = line
                    .split_once(' ')
                    .ok_or_else(|| io::Error::other("invalid packed ref"))?;
                validate_ref(name, id)?;
                result.insert(name.to_string(), id.to_string());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut directories = vec![repo.join("refs")];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(path);
                continue;
            }
            if entry.file_name().to_string_lossy().ends_with(".lock") {
                continue;
            }
            if !kind.is_file() {
                return Err(io::Error::other("nonregular ref"));
            }
            let name = path
                .strip_prefix(repo)
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| io::Error::other("non UTF-8 ref"))?;
            let id = fs::read_to_string(&path)?;
            let id = id.trim();
            validate_ref(name, id)?;
            result.insert(name.to_string(), id.to_string());
        }
    }
    Ok(result)
}

fn validate_ref(name: &str, id: &str) -> io::Result<()> {
    git_zero_core::repository::validate_ref_name(name)
        .map_err(|_| io::Error::other("invalid ref name"))?;
    if id.len() != 40
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || id.bytes().all(|byte| byte == b'0')
    {
        return Err(io::Error::other("invalid ref object ID"));
    }
    Ok(())
}

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
    for (name, id) in refs {
        validate_ref(name, id)?;
    }
    Ok(())
}

/// Validate ALL metadata before either a full or selective object read.
fn validate_snapshot(manifest: &Manifest) -> io::Result<()> {
    validate_snapshot_refs(&manifest.refs)?;
    validate_snapshot_bytes(manifest.packs.iter().map(|p| (p.pack_bytes, p.index_bytes)))?;
    // Native filenames must never escape the disposable pack directory.
    for pack in &manifest.packs {
        if pack.id.is_empty()
            || !pack
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io::Error::other("invalid native pack identity"));
        }
    }
    crate::snapshot_chunks(manifest)?;
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

fn write_refs(repo: &Path, wanted: &BTreeMap<String, String>) -> io::Result<()> {
    let mut packed = String::from("# pack-refs with: sorted\n");
    for (name, id) in wanted {
        validate_ref(name, id)?;
        packed += &format!("{id} {name}\n");
    }
    // Requests are serialized. These derived files need no durability barrier:
    // the manifest was already synced and reconstructs the complete snapshot.
    fs::write(repo.join("packed-refs.next"), packed)?;
    fs::rename(repo.join("packed-refs.next"), repo.join("packed-refs"))?;
    fs::remove_dir_all(repo.join("refs"))?;
    fs::create_dir(repo.join("refs"))?;
    Ok(())
}

pub fn http_backend(repo: &Path, request: &Request) -> io::Result<Response> {
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    if ![
        "/repo.git/info/refs",
        "/repo.git/git-upload-pack",
        "/repo.git/git-receive-pack",
    ]
    .contains(&path)
    {
        return Ok(Response::error(404, "unknown route"));
    }
    let parent = repo
        .parent()
        .ok_or_else(|| io::Error::other("invalid repository path"))?;
    let mut process = Command::new("git");
    process
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", parent)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REQUEST_METHOD", &request.method)
        .env("REMOTE_USER", "local-benchmark")
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env(
            "CONTENT_TYPE",
            request
                .headers
                .get("content-type")
                .map(String::as_str)
                .unwrap_or(""),
        )
        .env("REMOTE_ADDR", "127.0.0.1");
    for (header, variable) in [
        ("git-protocol", "HTTP_GIT_PROTOCOL"),
        ("content-encoding", "HTTP_CONTENT_ENCODING"),
    ] {
        process.env(
            variable,
            request
                .headers
                .get(header)
                .map(String::as_str)
                .unwrap_or(""),
        );
    }
    let output = command(process, &request.body)?.stdout;
    let (boundary, skip) = output
        .windows(4)
        .position(|x| x == b"\r\n\r\n")
        .map(|p| (p, 4))
        .or_else(|| output.windows(2).position(|x| x == b"\n\n").map(|p| (p, 2)))
        .ok_or_else(|| io::Error::other("invalid CGI output"))?;
    let mut response = Response {
        status: 200,
        headers: BTreeMap::new(),
        body: output[boundary + skip..].into(),
    };
    for line in String::from_utf8_lossy(&output[..boundary]).lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid CGI header"))?;
        if name.eq_ignore_ascii_case("status") {
            response.status = value
                .split_whitespace()
                .next()
                .unwrap_or("500")
                .parse()
                .map_err(io::Error::other)?;
        } else if !name.eq_ignore_ascii_case("cache-control") {
            response
                .headers
                .insert(name.to_string(), value.trim().to_string());
        }
    }
    Ok(response)
}

fn git_rpc(repo: &Path, request: &Request) -> io::Result<Response> {
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    let (service, advertise) = match (request.method.as_str(), path) {
        ("POST", "/repo.git/git-receive-pack") => ("git-receive-pack", false),
        ("POST", "/repo.git/git-upload-pack") => ("git-upload-pack", false),
        ("GET", "/repo.git/info/refs") => {
            let service = query
                .split('&')
                .filter_map(|part| part.split_once('='))
                .find_map(|(key, value)| (key == "service").then_some(value));
            match service {
                Some("git-upload-pack") => ("git-upload-pack", true),
                Some("git-receive-pack") => ("git-receive-pack", true),
                _ => return http_backend(repo, request),
            }
        }
        _ => return Ok(Response::error(404, "unknown route")),
    };
    // Preserve the reference backend's HTTP validation and gzip decoding for
    // encoded requests; the common identity path needs only one Git process.
    if request
        .headers
        .get("content-encoding")
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
        || (!advertise
            && request.headers.get("content-type").map(String::as_str)
                != Some(&format!("application/x-{service}-request")))
    {
        return http_backend(repo, request);
    }
    let mut process = Command::new("git");
    if service == "git-upload-pack" && !advertise && has_haves(&request.body) {
        // A one-entry search creates useful thin deltas against client bases
        // even when incoming self-contained packs stored those objects whole.
        // Full downloads still reuse streams without a fresh delta search.
        process.args(["-c", "pack.window=1"]);
    }
    process
        .arg(service.trim_start_matches("git-"))
        .arg("--stateless-rpc");
    if advertise {
        process.arg("--advertise-refs");
    }
    process.arg(repo).env(
        "GIT_PROTOCOL",
        request
            .headers
            .get("git-protocol")
            .map(String::as_str)
            .unwrap_or(""),
    );
    let mut body = command(process, &request.body)?.stdout;
    if advertise {
        let payload = format!("# service={service}\n");
        let mut prefix = format!("{:04x}{payload}0000", payload.len() + 4).into_bytes();
        prefix.extend_from_slice(&body);
        body = prefix;
    }
    Ok(Response {
        status: 200,
        headers: BTreeMap::from([(
            "Content-Type".into(),
            format!(
                "application/x-{service}-{}",
                if advertise { "advertisement" } else { "result" }
            ),
        )]),
        body: body.into(),
    })
}

fn has_haves(mut body: &[u8]) -> bool {
    use git_zero_core::pktline::{Packet, read};
    while !body.is_empty() {
        // Protocol v2 adds delimiter/response-end packets to v0 framing.
        if body.starts_with(b"0001") || body.starts_with(b"0002") {
            body = &body[4..];
            continue;
        }
        match read(&mut body) {
            Ok(Some(Packet::Data(data))) => {
                if data.strip_prefix(b"have ").is_some_and(|id| {
                    let id = id.strip_suffix(b"\n").unwrap_or(id);
                    id.len() == 40 && id.iter().all(u8::is_ascii_hexdigit)
                }) {
                    return true;
                }
            }
            Ok(Some(Packet::Flush)) => {}
            _ => return false,
        }
    }
    false
}

fn incoming_pack(mut body: &[u8]) -> Option<&[u8]> {
    use git_zero_core::pktline::{Packet, read};
    loop {
        match read(&mut body).ok()?? {
            Packet::Data(_) => {}
            Packet::Flush => return body.starts_with(b"PACK").then_some(body),
        }
    }
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

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
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
    pub generation_witness_hits: u64,
    pub generation_witness_fallbacks: u64,
    pub upload_advertisement_shortcuts: u64,
    pub upload_advertisement_native_fallbacks: u64,
    pub upload_advertisement_native_bootstraps: u64,
    pub upload_advertisement_certificates: u64,
    pub upload_advertisement_type_checks: u64,
    pub upload_advertisement_certification_seconds: f64,
    pub upload_advertisement_template_bytes: u64,
    pub cache_retained_bytes: u64,
    pub history_lineage_bytes: u64,
    pub history_pending_lineage_bytes: u64,
    pub history_projections: u64,
    pub history_full_restores: u64,
    pub cache_evictions: u64,
    pub receive_noop_shortcuts: u64,
    pub gateway_receive_plans: u64,
    pub gateway_receive_unsupported: u64,
    pub gateway_receive_consumed: u64,
    pub gateway_read_plans: u64,
    pub gateway_read_unsupported: u64,
    pub gateway_read_consumed: u64,
    pub gateway_read_generation_changed: u64,
    pub gateway_read_paid_discards: u64,
    pub gateway_read_errors: u64,
    pub protected_chunk_admissions: u64,
    pub protected_chunk_hits: u64,
    pub protected_chunk_hit_bytes: u64,
    pub protected_chunk_evictions: u64,
    pub protected_chunk_pressure_evictions: u64,
    pub protected_chunk_reset_evictions: u64,
    pub protected_chunk_prunes: u64,
    pub protected_chunk_rejections: u64,
    pub protected_chunk_entries: u64,
    pub protected_chunk_bytes: u64,
    pub protected_chunk_owning_bytes: u64,
    pub joint_cache_entries: u64,
    pub joint_cache_retained_peak_bytes: u64,
}

struct CandidatePack {
    stored: Option<StoredPack>,
    id: String,
    level: u32,
    pack: PathBuf,
    index: PathBuf,
    bytes: u64,
    floor: u64,
}

#[derive(Clone, Copy)]
enum ViewScope {
    Invalid,
    Full {
        generation: u64,
    },
    Projection {
        generation: u64,
        cutoff: u64,
        binding: [u8; 32],
        packs: usize,
        bytes: u64,
    },
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct AuthorityFingerprint {
    generation: u64,
    published: [u8; 32],
    local: [u8; 32],
}

// Created only by an authenticated matched receive continuation after all Full
// bytes are verified/materialized. Owned, one-call preparation, never a cache.
struct ValidatedFullReceive {
    authority: AuthorityFingerprint,
    request_hash: String,
    stable_selection: String,
    key: crate::first_read::Key,
    nonce: String,
    prepare_start: Instant,
}

fn eligible_small_receive(request: &Request) -> bool {
    use git_zero_core::pktline::{Packet, read};
    if !advertisement::eligible_receive(request)
        || request.body.len() > crate::first_read::MAX_ORIGINAL
    {
        return false;
    }
    let mut body = request.body.as_slice();
    let mut commands = 0;
    while let Ok(Some(packet)) = read(&mut body) {
        match packet {
            Packet::Flush => {
                return commands > 0
                    && body.len() >= 12 + 20
                    && body.starts_with(b"PACK")
                    && matches!(u32::from_be_bytes(body[4..8].try_into().unwrap()), 2 | 3)
                    && u32::from_be_bytes(body[8..12].try_into().unwrap()) > 0;
            }
            Packet::Data(data) => {
                let data = data.strip_suffix(b"\n").unwrap_or(data);
                let mut parts = data.split(|b| *b == 0);
                let command = parts.next().unwrap_or_default();
                if let Some(caps) = parts.next() {
                    // Native send-pack uses NUL followed by one separator space.
                    let caps = caps.strip_prefix(b" ").unwrap_or(caps);
                    if commands != 0
                        || parts.next().is_some()
                        || caps.len() > 1024
                        || !caps.iter().all(|b| (32..127).contains(b))
                        || caps.split(|b| *b == b' ').any(|c| {
                            !matches!(
                                c,
                                b"report-status"
                                    | b"report-status-v2"
                                    | b"side-band-64k"
                                    | b"quiet"
                                    | b"atomic"
                                    | b"ofs-delta"
                                    | b"object-format=sha1"
                            ) && !c.starts_with(b"agent=")
                        })
                    {
                        return false;
                    }
                }
                if command.len() < 83
                    || command[40] != b' '
                    || command[81] != b' '
                    || !command[..40]
                        .iter()
                        .chain(&command[41..81])
                        .all(u8::is_ascii_hexdigit)
                    || !command[82..].starts_with(b"refs/")
                    || !command[82..].iter().all(|b| *b > 32 && *b != 127)
                {
                    return false;
                }
                if std::str::from_utf8(&command[82..])
                    .ok()
                    .is_none_or(|name| git_zero_core::repository::validate_ref_name(name).is_err())
                {
                    return false;
                }
                commands += 1;
            }
        }
    }
    false
}

/// Generic immutable GET-cache policy, independent of workload or key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectedChunkMode {
    Off,
    One,
    Multi,
}

pub struct NativeHost {
    root: PathBuf,
    view: PathBuf,
    repository: Repository<HostBlobs, HostState>,
    cloudflare: Option<Arc<HttpServices>>,
    cold_view: bool,
    cache: VecDeque<(String, Response)>,
    cache_bytes: usize,
    protected_chunks: Vec<crate::storage::VerifiedChunk>,
    protected_mode: ProtectedChunkMode,
    ordinary_chunk_admission: bool,
    advertisement: Option<advertisement::Certificate>,
    view_dirty: bool,
    scope: ViewScope,
    lineage: Option<history::Certificate>,
    pending_lineage_bytes: usize,
    pending_advertisement_bytes: usize,
    publication_active: bool,
    manifest: Manifest,
    authority_fingerprint: Option<AuthorityFingerprint>,
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
        services.enable_first_read(cold_view);
        services.enable_first_receive(cold_view);
        let mut repository = Repository::new(
            HostBlobs::Cloudflare(R2BlobStore::new(services.clone()), services.clone()),
            HostState::Cloudflare(DurableStateStore::new(services.clone())),
        );
        // Storage layout policy is independent of the 8 MiB compaction budget.
        // Readers use manifest lengths, so existing 8 MiB chunks remain valid.
        repository.chunk_bytes = 16 * 1024 * 1024;
        Self::with_services(root, repository, Some(services), cold_view)
    }

    /// Share the host's registration capability with its listening HTTP runner.
    pub fn cloudflare_services(&self) -> Option<Arc<HttpServices>> {
        self.cloudflare.clone()
    }

    /// Original boolean control retains its ONE/off semantics.
    pub fn enable_protected_chunk(&mut self, enabled: bool) {
        self.set_protected_chunk_mode(if enabled {
            ProtectedChunkMode::One
        } else {
            ProtectedChunkMode::Off
        });
    }

    pub fn set_protected_chunk_mode(&mut self, mode: ProtectedChunkMode) {
        self.protected_mode = if self.cloudflare.is_some() && self.cold_view {
            mode
        } else {
            ProtectedChunkMode::Off
        };
        while self.protected_chunks.len() > self.protected_limit() {
            self.evict_protected_chunk();
        }
        self.trim_cache(0);
    }

    fn protected_limit(&self) -> usize {
        match self.protected_mode {
            ProtectedChunkMode::Off => 0,
            ProtectedChunkMode::One => 1,
            ProtectedChunkMode::Multi => 32,
        }
    }

    fn with_services(
        root: PathBuf,
        repository: Repository<HostBlobs, HostState>,
        cloudflare: Option<Arc<HttpServices>>,
        cold_view: bool,
    ) -> io::Result<Self> {
        let view = root.join("view/repo.git");
        let protected_mode = if cloudflare.is_some() && cold_view {
            ProtectedChunkMode::Multi
        } else {
            ProtectedChunkMode::Off
        };
        let mut host = Self {
            root,
            view,
            repository,
            cloudflare,
            cold_view,
            cache: VecDeque::new(),
            cache_bytes: 0,
            protected_chunks: Vec::new(),
            protected_mode,
            ordinary_chunk_admission: false,
            advertisement: None,
            view_dirty: false,
            scope: ViewScope::Invalid,
            lineage: None,
            pending_lineage_bytes: 0,
            pending_advertisement_bytes: 0,
            publication_active: false,
            manifest: Manifest::default(),
            authority_fingerprint: None,
            metrics: Metrics::default(),
        };
        host.recover()?;
        Ok(host)
    }

    fn recover(&mut self) -> io::Result<()> {
        // Startup/observer/dirty-state reconciliation must never warm the slot.
        let admission = std::mem::replace(&mut self.ordinary_chunk_admission, false);
        let result = self.recover_inner(admission);
        self.ordinary_chunk_admission = admission;
        self.trim_cache(0);
        result
    }

    fn recover_inner(&mut self, ordinary: bool) -> io::Result<()> {
        self.authority_fingerprint = None;
        // Recovery includes ambiguous publications and external generations.
        // A later full native advertisement must certify this state afresh.
        self.advertisement = None;
        self.lineage = None;
        self.scope = ViewScope::Invalid;
        self.view_dirty = true;
        self.repository.state.stabilize()?;
        let manifest = self.repository.state.load()?;
        validate_snapshot_refs(&manifest.refs)?;
        validate_snapshot_bytes(manifest.packs.iter().map(|p| (p.pack_bytes, p.index_bytes)))?;
        self.rebuild_view(&manifest)?;
        self.metrics.retired_chunk_deletes +=
            self.repository.blobs.reclaim_unreferenced(&manifest)? as u64;
        self.manifest = manifest;
        // Observer/startup reconstruction is not a cache-policy mutation.
        if ordinary {
            self.prune_protected_chunks()?;
        }
        if self.cloudflare.is_some() {
            let digest = crate::first_read::manifest_digest(&self.manifest);
            self.authority_fingerprint = Some(AuthorityFingerprint {
                generation: self.manifest.generation,
                published: digest,
                local: digest,
            });
        }
        self.cache = VecDeque::new();
        self.cache_bytes = 0;
        self.view_dirty = false;
        Ok(())
    }

    fn rebuild_view(&mut self, manifest: &Manifest) -> io::Result<()> {
        self.materialize_view(manifest, None)?;
        if self.cloudflare.is_some() && self.cold_view {
            let identity = advertisement::identity(&self.view);
            if self
                .lineage
                .as_ref()
                .is_some_and(|proof| !proof.matches(manifest, identity))
            {
                self.lineage = None;
            }
            if self.lineage.is_none()
                && !self.publication_active
                && let Some(identity) = identity
                && let Some(bound) = history::Certificate::allocation_bound(
                    manifest.packs.len(),
                    manifest.refs.len(),
                )
                && manifest
                    .refs
                    .keys()
                    .all(|name| name.starts_with("refs/heads/"))
                && advertisement::branch_commits(&self.view, manifest)
            {
                if self.reserve_metadata(bound) {
                    self.lineage = history::Certificate::bootstrap(manifest, identity);
                }
                self.trim_cache(0);
            }
        }
        Ok(())
    }

    fn materialize_view(
        &mut self,
        manifest: &Manifest,
        projection: Option<&history::Projection>,
    ) -> io::Result<()> {
        self.materialize_view_prefetched(manifest, projection, None)
    }

    fn materialize_view_prefetched(
        &mut self,
        manifest: &Manifest,
        projection: Option<&history::Projection>,
        prefetched: Option<(Vec<Chunk>, crate::storage::VerifiedChunks)>,
    ) -> io::Result<()> {
        crate::diagnostics::observe("view_materialize", |diagnostic| {
            diagnostic.mark("begin");

            self.view_dirty = true;
            self.scope = ViewScope::Invalid;
            validate_snapshot(manifest)?;
            let all;
            let selected = if let Some(projection) = projection {
                &projection.packs
            } else {
                all = (0..manifest.packs.len()).collect::<Vec<_>>();
                &all
            };
            // Remove the old disposable view, then verify every selected byte
            // against exact ordered descriptors BEFORE writing any native file.
            if self.view.exists() {
                fs::remove_dir_all(&self.view)?;
            }
            let cached = self.protected_chunks.clone();
            let non_chunk_bytes = self.cache_bytes + self.metadata_bytes() - self.protected_bytes();
            let response_entries = self.cache.len();
            let limit = if self.ordinary_chunk_admission {
                self.protected_limit()
            } else {
                0
            };
            let chunks = &mut self.protected_chunks;
            let metrics = &mut self.metrics;
            let resolved = self.repository.read_snapshot_selection_cached(
                manifest,
                selected,
                &cached,
                prefetched,
                &mut |candidate| {
                    Self::admit_chunk(
                        chunks,
                        metrics,
                        candidate,
                        limit,
                        non_chunk_bytes,
                        response_entries,
                    );
                },
            )?;
            self.metrics.protected_chunk_hits += resolved.cache_hits as u64;
            self.metrics.protected_chunk_hit_bytes += resolved.cache_hit_bytes;
            self.note_joint_retention();
            let files = resolved.files;
            init(&self.view)?;
            for (pack, (data, index)) in selected.iter().map(|i| &manifest.packs[*i]).zip(files) {
                let directory = self.view.join("objects/pack");
                for (extension, chunks, bytes) in
                    [("pack", &pack.pack, &data), ("idx", &pack.index, &index)]
                {
                    let path = directory.join(format!("pack-{}.{extension}", pack.id));
                    if pack.bundle.is_empty() && chunks.len() == 1 {
                        self.repository
                            .blobs
                            .materialize_file(&chunks[0].key, &path, bytes)?;
                    } else {
                        fs::write(path, bytes)?;
                    }
                }
            }
            if let Some(projection) = projection {
                let refs = self
                    .lineage
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing lineage proof"))?
                    .projected_refs(manifest, projection.cutoff);
                write_refs(&self.view, &refs)?;
                self.scope = ViewScope::Projection {
                    generation: manifest.generation,
                    cutoff: projection.cutoff,
                    binding: projection.binding,
                    packs: selected.len(),
                    bytes: selected
                        .iter()
                        .map(|i| manifest.packs[*i].pack_bytes + manifest.packs[*i].index_bytes)
                        .sum(),
                };
            } else {
                write_refs(&self.view, &manifest.refs)?;
                self.scope = ViewScope::Full {
                    generation: manifest.generation,
                };
            }
            self.view_dirty = false;
            Ok(())
        })
    }

    fn cloud_read_view(&mut self) -> io::Result<()> {
        if self.cloudflare.is_some() && self.cold_view {
            let was_projection = matches!(self.scope, ViewScope::Projection { .. });
            self.rebuild_view(&self.manifest.clone())?;
            self.metrics.cloud_view_rehydrates += 1;
            self.metrics.history_full_restores += u64::from(was_projection);
        }
        Ok(())
    }

    fn require_full_view(&mut self) -> io::Result<()> {
        if !matches!(self.scope, ViewScope::Full { generation } if generation == self.manifest.generation)
        {
            self.cloud_read_view()?;
        }
        Ok(())
    }

    fn fetch_read_view(&mut self, request: &Request) -> io::Result<()> {
        crate::diagnostics::observe("fetch_view_prepare", |diagnostic| {
            diagnostic.mark("begin");

            if self.cloudflare.is_none() || !self.cold_view {
                return self.require_full_view();
            }
            validate_snapshot(&self.manifest)?;
            let identity = advertisement::identity(&self.view);
            if self
                .lineage
                .as_ref()
                .is_some_and(|proof| !proof.matches(&self.manifest, identity))
            {
                self.lineage = None;
            }
            let plan = self
                .lineage
                .as_ref()
                .and_then(|proof| proof.plan(request, &self.manifest, identity));
            if let Some(plan) = plan {
                // Always reconstruct a cold miss using the verified immutable slot
                // and ordinary verified GETs for all missing selected chunks.
                self.materialize_view(&self.manifest.clone(), Some(&plan))?;
                self.metrics.cloud_view_rehydrates += 1;
                self.metrics.history_projections += 1;
                return Ok(());
            }
            self.cloud_read_view()
        })
    }

    fn native_read(&mut self, request: &Request) -> io::Result<Response> {
        crate::diagnostics::observe("native_read", |diagnostic| {
            diagnostic.mark("begin");
            diagnostic.body("logical_request_body", &request.body);

            git_rpc(&self.view, request).inspect_err(|_| {
                self.view_dirty = true;
                self.scope = ViewScope::Invalid;
            })
        })
    }

    fn import_pack(
        &mut self,
        pack: &CandidatePack,
        data: &[u8],
        index: &[u8],
        pack_keys: Option<&[String]>,
        pending: &mut Option<crate::storage::PendingChunk>,
    ) -> io::Result<StoredPack> {
        if self.cloudflare.is_some() {
            // Only a subsequent new pack flushes the earlier tail. Reusing an
            // unchanged pack keeps the one fusion candidate pending.
            if let Some(previous) = pending.take() {
                let len = previous.chunk().len;
                self.repository.flush_pending(previous)?;
                self.metrics.blob_puts += 1;
                self.metrics.blob_put_bytes += len;
            }
            let staged =
                self.repository
                    .stage_bundled_pack(pack.id.clone(), data, index, pack.level)?;
            let (stored, tail) = staged.into_parts();
            *pending = Some(tail);
            return Ok(stored);
        }
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
            bundle: Vec::new(),
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
            floor: 0,
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
            let floor = selected
                .iter()
                .map(|i| packs[*i].floor)
                .min()
                .expect("nonempty compaction");
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
                floor,
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

    fn receive(&mut self, request: &Request) -> io::Result<Response> {
        self.receive_prepared(request, None)
    }

    fn receive_prepared(
        &mut self,
        request: &Request,
        prepared: Option<ValidatedFullReceive>,
    ) -> io::Result<Response> {
        // Native receive-pack returns exactly an empty successful result for
        // this complete no-command probe under the fixed policy. Authority was
        // checked by dispatch; validate ALL metadata, but consume no payload.
        // In particular, a partial view stays honestly partial until the real
        // nonempty push performs its ordinary single cold full reconstruction.
        if self.cloudflare.is_some()
            && self.cold_view
            && advertisement::eligible_receive(request)
            && request.body == b"0000"
            && !request.headers.contains_key("transfer-encoding")
            && request
                .headers
                .get("content-length")
                .is_none_or(|length| length == "4")
        {
            validate_snapshot(&self.manifest)?;
            if let Some(identity) = advertisement::identity(&self.view)
                && self
                    .lineage
                    .as_ref()
                    .is_none_or(|proof| proof.matches(&self.manifest, Some(identity)))
                && self
                    .advertisement
                    .as_ref()
                    .is_none_or(|proof| proof.matches(&self.manifest, Some(identity)))
            {
                self.metrics.receive_noop_shortcuts += 1;
                return Ok(Response {
                    status: 200,
                    headers: BTreeMap::from([(
                        "Content-Type".into(),
                        "application/x-git-receive-pack-result".into(),
                    )]),
                    body: Arc::from([]),
                });
            }
        }
        if let Some(prepared) = &prepared
            && self.stable_full_selection()? != prepared.stable_selection
        {
            return Err(io::Error::other("prepared receive selection changed"));
        }
        let lineage = self.lineage.take().filter(|proof| {
            advertisement::eligible_receive(request)
                && proof.matches(&self.manifest, advertisement::identity(&self.view))
        });
        self.pending_lineage_bytes = lineage.as_ref().map_or(0, |proof| proof.weight());
        if !self.trim_cache(0) {
            self.pending_lineage_bytes = 0;
            return Err(io::Error::other("receive metadata exceeds cache budget"));
        }
        self.publication_active = true;
        let result = self.receive_transaction(request, lineage, prepared);
        self.publication_active = false;
        self.pending_lineage_bytes = 0;
        self.pending_advertisement_bytes = 0;
        result
    }

    fn receive_transaction(
        &mut self,
        request: &Request,
        lineage: Option<history::Certificate>,
        prepared: Option<ValidatedFullReceive>,
    ) -> io::Result<Response> {
        // Take the certificate out while publication can fail. Only a strict,
        // confirmed native transaction restores/advances it at the end.
        let mut certificate = self.advertisement.take().filter(|certificate| {
            advertisement::eligible_receive(request)
                && certificate.matches(&self.manifest, advertisement::identity(&self.view))
        });
        self.pending_advertisement_bytes = certificate.as_ref().map_or(0, |proof| proof.weight());
        let prepare_start = if let Some(prepared) = prepared {
            if self.view_dirty
                || !matches!(self.scope, ViewScope::Full { generation } if generation == self.manifest.generation)
                || self.authority_fingerprint != Some(prepared.authority)
                || prepared.authority.local != crate::first_read::manifest_digest(&self.manifest)
                || prepared.request_hash != crate::first_read::request_hash(request)
                || prepared.nonce.len() != 32
                || self
                    .cloudflare
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing prepared provider"))?
                    .first_read_key()?
                    != Some(prepared.key)
            {
                return Err(io::Error::other(
                    "prepared receive no longer matches Full view",
                ));
            }
            prepared.prepare_start
        } else {
            let start = Instant::now();
            self.cloud_read_view()?;
            start
        };
        self.lineage = None;
        // Scalar cleanup can change provider debt before returning an error.
        // Preserve recovery even though publication has not started yet.
        let cleanup_dirty = self.view_dirty;
        if !self.manifest.garbage.is_empty() {
            self.view_dirty = true;
        }
        if !self
            .manifest
            .collect_garbage(&self.repository.blobs)
            .is_empty()
        {
            return Err(io::Error::other(
                "retired chunk cleanup must complete before another push",
            ));
        }
        self.view_dirty = cleanup_dirty;
        let mut previous = self.manifest.clone();
        previous.garbage.clear();
        let quarantine = Quarantine(self.root.join("quarantine"));
        if quarantine.0.exists() {
            fs::remove_dir_all(&quarantine.0)?;
        }
        let stage = quarantine.0.join("repo.git");
        init(&stage)?;
        fs::create_dir_all(stage.join("objects/info"))?;
        fs::write(
            stage.join("objects/info/alternates"),
            format!("{}\n", self.view.join("objects").display()),
        )?;
        write_refs(&stage, &previous.refs)?;
        self.metrics.receive_prepare_seconds += prepare_start.elapsed().as_secs_f64();
        let codec_start = Instant::now();
        // Hash large input streams while strict native validation runs. Reuse
        // only after comparing every byte of the immutable codec output. Thin
        // pack completion or compaction takes the ordinary output-hashing path.
        // Cloud bundles include the index, so pack-only hashes do not apply.
        let input_pack = incoming_pack(&request.body).filter(|bytes| bytes.len() >= 256 * 1024);
        let chunk_bytes = self.repository.chunk_bytes;
        let (response, input_keys) =
            if let Some(bytes) = input_pack.filter(|_| self.cloudflare.is_none()) {
                std::thread::scope(|scope| -> io::Result<_> {
                    let worker = scope.spawn(|| {
                        bytes
                            .chunks(chunk_bytes)
                            .map(|chunk| format!("chunks/{}", hash(chunk)))
                            .collect::<Vec<_>>()
                    });
                    let response = git_rpc(&stage, request);
                    let keys = worker
                        .join()
                        .map_err(|_| io::Error::other("pack checksum thread failed"))?;
                    Ok((response?, Some(keys)))
                })?
            } else {
                (git_rpc(&stage, request)?, None)
            };
        let next_refs = refs(&stage)?;
        self.metrics.receive_codec_seconds += codec_start.elapsed().as_secs_f64();
        if response.status != 200 || next_refs == previous.refs {
            // Git's HTTP transport sends a successful empty receive probe
            // before streaming a large push. No authoritative transaction or
            // storage write occurred: preserve only the exact unchanged proof.
            if response.status == 200 && next_refs == previous.refs {
                self.lineage = lineage
                    .filter(|proof| proof.matches(&previous, advertisement::identity(&self.view)));
            }
            self.advertisement = certificate;
            return Ok(response);
        }
        validate_snapshot_refs(&next_refs)?;
        self.view_dirty = true;
        let generation = previous
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("generation overflow"))?;
        let incoming_paths = fs::read_dir(stage.join("objects/pack"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        let plan_capacity = previous.packs.len() + incoming_paths.len();
        let plan_floor_bytes = plan_capacity * size_of::<u64>();
        if !self.reserve_metadata(plan_floor_bytes) {
            return Err(io::Error::other(
                "publication metadata exceeds cache budget",
            ));
        }
        self.pending_lineage_bytes += plan_floor_bytes;
        self.trim_cache(0);
        let mut packs = Vec::with_capacity(plan_capacity);
        packs.extend(previous.packs.iter().enumerate().map(|(i, p)| {
            let mut pack = self.native_pack(p);
            pack.floor = lineage.as_ref().map_or(0, |proof| proof.floor(i));
            pack
        }));
        for path in incoming_paths {
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
                    floor: generation,
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
            generation,
            refs: next_refs,
            packs: Vec::new(),
            garbage: Vec::new(),
        };
        let lineage_bound = lineage
            .as_ref()
            .and_then(|_| history::Certificate::allocation_bound(packs.len(), next.refs.len()))
            .filter(|bound| self.reserve_metadata(*bound));
        if let Some(bound) = lineage_bound {
            self.pending_lineage_bytes += bound;
            self.trim_cache(0);
        }
        let mut floors = lineage_bound.map(|_| Vec::with_capacity(packs.len()));
        let mut pending = None;
        for pack in packs {
            if let Some(floors) = &mut floors {
                floors.push(pack.floor);
            }
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
                let stored = self.import_pack(&pack, bytes, &index, keys, &mut pending)?;
                let deferred = pending.as_ref().map(|tail| tail.chunk().len);
                self.metrics.blob_puts +=
                    stored.chunks().count() as u64 - u64::from(deferred.is_some());
                self.metrics.blob_put_bytes +=
                    (bytes.len() + index.len()) as u64 - deferred.unwrap_or(0);
                for (source, extension) in [(&pack.pack, "pack"), (&pack.index, "idx")] {
                    let destination = self
                        .view
                        .join(format!("objects/pack/pack-{}.{extension}", pack.id));
                    if !destination.exists() {
                        fs::hard_link(source, destination)?;
                    }
                }
                stored
            };
            next.packs.push(stored);
        }
        self.pending_lineage_bytes -= plan_floor_bytes;
        self.metrics.receive_storage_seconds += storage_start.elapsed().as_secs_f64();
        let publication_start = Instant::now();
        next.retire_replaced(&previous);
        let candidate = lineage.as_ref().zip(floors).and_then(|(proof, floors)| {
            proof.advance(
                &previous,
                &next,
                advertisement::identity(&self.view),
                floors,
            )
        });
        drop(lineage);
        self.pending_lineage_bytes = candidate.as_ref().map_or(0, |proof| proof.weight());
        // A manifest write can become visible before an I/O error is reported.
        // Recover from authoritative state before serving any later request.
        self.view_dirty = true;
        let deferred = pending.as_ref().map(|tail| tail.chunk().len);
        self.authority_fingerprint = None;
        let publication =
            self.repository
                .publish_staged_retiring(&previous, next.clone(), pending)?;
        if let Some(len) = deferred {
            self.metrics.blob_puts += 1;
            self.metrics.blob_put_bytes += len;
        }
        let retirement = match publication {
            crate::storage::RetiringCas::Published(receipt) => receipt,
            _ => return Err(io::Error::other("concurrent generation update")),
        };
        let published_digest = crate::first_read::manifest_digest(&next);
        // Publication is authoritative. A restart reconstructs refs from it if
        // the process dies while updating this disposable native view.
        write_refs(&self.view, &next.refs)?;
        self.manifest = next;
        self.prune_protected_chunks()?;
        self.prune_native_packs()?;
        self.metrics.retired_chunk_deletes += retirement
            .outcomes
            .iter()
            .filter(|outcome| outcome.deleted)
            .count() as u64;
        // Failed deletions remain in the durable manifest and are retried
        // before accepting more data; publication itself is already durable.
        self.manifest.garbage = retirement
            .outcomes
            .into_iter()
            .filter(|outcome| !outcome.deleted)
            .map(|outcome| outcome.key)
            .collect();
        if self.cloudflare.is_some() {
            self.authority_fingerprint = Some(AuthorityFingerprint {
                generation: self.manifest.generation,
                published: published_digest,
                local: crate::first_read::manifest_digest(&self.manifest),
            });
        }
        self.metrics.receive_publication_seconds += publication_start.elapsed().as_secs_f64();
        // A v2 DO can require physical reconciliation after a failed delete.
        // Publication still succeeded; recover before the next request instead
        // of retrying scalar debt against an uncertain catalog indefinitely.
        self.view_dirty = self.cloudflare.is_some() && !self.manifest.garbage.is_empty();
        self.cache = VecDeque::new();
        self.cache_bytes = 0;
        self.scope = ViewScope::Full {
            generation: self.manifest.generation,
        };
        self.lineage = candidate;
        self.pending_lineage_bytes = 0;
        self.advertisement = certificate.take().and_then(|mut certificate| {
            (certificate.matches(&previous, advertisement::identity(&self.view))
                && certificate.advance(&previous, &self.manifest))
            .then_some(certificate)
        });
        Ok(response)
    }

    fn dispatch(&mut self, request: &Request) -> io::Result<Response> {
        if let Some(services) = &self.cloudflare {
            let receive_discovery = request.target.split_once('?').is_some_and(|(path, query)| {
                path == "/repo.git/info/refs"
                    && query
                        .split('&')
                        .filter_map(|part| part.split_once('='))
                        .find_map(|(key, value)| (key == "service").then_some(value))
                        == Some("git-receive-pack")
            });
            services.set_phase(
                if receive_discovery
                    || (request.method == "POST"
                        && request.target.split('?').next() == Some("/repo.git/git-receive-pack"))
                {
                    "push"
                } else {
                    "fetch"
                },
            )?;
        }
        if self.view_dirty {
            self.recover()?;
        }
        if let Some(services) = &self.cloudflare {
            if services.generation_witness_matches(request, self.manifest.generation)? {
                self.metrics.generation_witness_hits += 1;
            } else {
                self.metrics.generation_witness_fallbacks += 1;
                let manifest = self.repository.state.load()?;
                if manifest.generation != self.manifest.generation {
                    self.recover()?;
                }
            }
        }
        let path = request
            .target
            .split_once('?')
            .map(|(path, _)| path)
            .unwrap_or(&request.target);
        if request.method == "POST" && path == "/repo.git/git-receive-pack" {
            return self.receive(request);
        }
        if request.method == "POST" && path == "/repo.git/git-upload-pack" {
            let key = self.cache_key(request);
            if let Some((_, response)) = self.cache.iter().find(|(k, _)| k == &key) {
                self.metrics.cache_hits += 1;
                return Ok(response.clone());
            }
            self.metrics.cache_misses += 1;
            self.fetch_read_view(request)?;
            let codec_start = Instant::now();
            let response = self.native_read(request)?;
            self.metrics.upload_codec_seconds += codec_start.elapsed().as_secs_f64();
            self.cache_insert(key, &response);
            return Ok(response);
        }
        if request.method == "GET" && path == "/repo.git/info/refs" {
            let key = self.cache_key(request);
            if let Some((_, response)) = self.cache.iter().find(|(entry, _)| entry == &key) {
                self.metrics.discovery_cache_hits += 1;
                return Ok(response.clone());
            }
            let query = request
                .target
                .split_once('?')
                .map(|(_, query)| query)
                .unwrap_or("");
            let receive = query
                .split('&')
                .filter_map(|part| part.split_once('='))
                .find_map(|(key, value)| (key == "service").then_some(value))
                == Some("git-receive-pack");
            let protocol = request
                .headers
                .get("git-protocol")
                .map(String::as_str)
                .unwrap_or("");
            let response = if receive && (protocol.is_empty() || protocol == "version=0") {
                self.receive_advertisement()?
            } else {
                self.upload_advertisement(request)?
            };
            self.cache_insert(key, &response);
            return Ok(response);
        }
        self.require_full_view()?;
        self.native_read(request)
    }

    fn upload_advertisement(&mut self, request: &Request) -> io::Result<Response> {
        let eligible =
            self.cloudflare.is_some() && self.cold_view && advertisement::eligible_request(request);
        let identity = eligible
            .then(|| advertisement::identity(&self.view))
            .flatten();
        if eligible {
            if self
                .advertisement
                .as_ref()
                .is_some_and(|certificate| !certificate.matches(&self.manifest, identity))
            {
                self.advertisement = None;
            }
            if let Some(certificate) = &self.advertisement {
                // Discovery omits object bytes, never complete metadata checks.
                validate_snapshot(&self.manifest)?;
                if let Ok(response) = certificate.encode(&self.manifest) {
                    self.metrics.upload_advertisement_shortcuts += 1;
                    return Ok(response);
                }
                self.advertisement = None;
            }
        }
        let upload = request.target.split_once('?').is_some_and(|(_, query)| {
            query
                .split('&')
                .filter_map(|part| part.split_once('='))
                .find_map(|(key, value)| (key == "service").then_some(value))
                == Some("git-upload-pack")
        });
        if upload {
            self.metrics.upload_advertisement_native_fallbacks += 1;
        }
        if eligible {
            self.metrics.upload_advertisement_native_bootstraps += 1;
        }
        self.cloud_read_view()?;
        let response = self.native_read(request)?;
        if let Some(identity) = identity
            && eligible
            && Some(identity) == advertisement::identity(&self.view)
        {
            if !self.reserve_metadata(advertisement::Certificate::allocation_bound()) {
                return Ok(response);
            }
            let certificate =
                advertisement::Certificate::learn(&self.manifest, &response, identity);
            if certificate.is_some() {
                self.metrics.upload_advertisement_type_checks += 1;
                let started = Instant::now();
                self.advertisement = certificate
                    .filter(|_| advertisement::branch_commits(&self.view, &self.manifest));
                self.metrics.upload_advertisement_certification_seconds +=
                    started.elapsed().as_secs_f64();
            }
            if self.advertisement.is_some() {
                self.metrics.upload_advertisement_certificates += 1;
                self.trim_cache(0);
            }
        }
        Ok(response)
    }

    fn advertisement_bytes(&self) -> usize {
        self.advertisement
            .as_ref()
            .map_or(0, |certificate| certificate.weight())
    }

    fn receive_advertisement(&self) -> io::Result<Response> {
        fn packet(output: &mut Vec<u8>, text: &str) -> io::Result<()> {
            if text.len() + 4 > 65520 {
                return Err(io::Error::other("ref advertisement packet too long"));
            }
            write!(output, "{:04x}{text}", text.len() + 4)
        }
        let mut body = Vec::new();
        packet(&mut body, "# service=git-receive-pack\n")?;
        body.extend_from_slice(b"0000");
        let capabilities = "report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format=sha1 agent=git-zero-host/0.1";
        if self.manifest.refs.is_empty() {
            packet(
                &mut body,
                &format!("{} capabilities^{{}}\0{capabilities}\n", "0".repeat(40)),
            )?;
        } else {
            for (index, (name, id)) in self.manifest.refs.iter().enumerate() {
                if index == 0 {
                    packet(&mut body, &format!("{id} {name}\0{capabilities}\n"))?;
                } else {
                    packet(&mut body, &format!("{id} {name}\n"))?;
                }
            }
        }
        body.extend_from_slice(b"0000");
        Ok(Response {
            status: 200,
            headers: BTreeMap::from([(
                "Content-Type".into(),
                "application/x-git-receive-pack-advertisement".into(),
            )]),
            body: body.into(),
        })
    }

    fn cache_key(&self, request: &Request) -> String {
        let mut digest = Sha256::new();
        digest.update(self.manifest.generation.to_le_bytes());
        for field in [
            request.method.as_bytes(),
            request.target.as_bytes(),
            &request.body,
        ] {
            digest.update((field.len() as u64).to_le_bytes());
            digest.update(field);
        }
        for (name, value) in request
            .headers
            .iter()
            .filter(|(name, _)| !crate::first_read::is_transport_header(name))
        {
            for field in [name.as_bytes(), value.as_bytes()] {
                digest.update((field.len() as u64).to_le_bytes());
                digest.update(field);
            }
        }
        format!("{:x}", digest.finalize())
    }

    fn cache_weight(key_capacity: usize, response: &Response) -> usize {
        key_capacity
            + response.body.len()
            + 2 * size_of::<usize>()
            + response
                .headers
                .iter()
                // A conservative owning-allocation bound for std's opaque
                // BTree nodes: one maximum-size node per header entry.
                .map(|(name, value)| {
                    name.capacity()
                        + value.capacity()
                        + 12 * size_of::<(String, String)>()
                        + 16 * size_of::<usize>()
                })
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
        let upper_weight = Self::cache_weight(key.capacity(), response);
        if upper_weight + self.metadata_bytes() - self.protected_bytes() > CACHE_BYTES {
            return;
        }
        // Reserve against original capacities before cloning; then charge the
        // exact cloned allocations that eviction will later subtract.
        if !self.trim_cache(upper_weight) {
            return;
        }
        let retained = response.clone();
        let weight = Self::cache_weight(key.capacity(), &retained);
        // Charge both old and new rings across a possible growth allocation.
        if self.cache.len() == self.cache.capacity() {
            let ring_bound = (self.cache.capacity().max(4) * 2) * size_of::<(String, Response)>();
            if !self.trim_cache(weight + ring_bound) {
                return;
            }
            self.cache.reserve(1);
        }
        if !self.trim_cache(weight) {
            return;
        }
        self.cache_bytes += weight;
        self.cache.push_back((key, retained));
        self.note_joint_retention();
    }

    fn lineage_bytes(&self) -> usize {
        self.lineage.as_ref().map_or(0, |proof| proof.weight())
    }

    fn metadata_bytes(&self) -> usize {
        self.advertisement_bytes()
            + self.lineage_bytes()
            + self.pending_lineage_bytes
            + self.pending_advertisement_bytes
            + self.cache.capacity() * size_of::<(String, Response)>()
            + size_of::<VecDeque<(String, Response)>>()
            + size_of::<ViewScope>()
            + size_of::<Option<AuthorityFingerprint>>()
            + size_of::<Option<advertisement::Certificate>>()
            + size_of::<Option<history::Certificate>>()
            + 2 * size_of::<usize>()
            + size_of::<Vec<crate::storage::VerifiedChunk>>()
            + size_of::<ProtectedChunkMode>()
            + 2 * size_of::<bool>()
            + self.protected_bytes()
            + self.cloudflare.as_ref().map_or(0, |services| {
                HttpServices::retirement_proof_storage_bytes()
                    + services.socket_storage_bytes()
                    + size_of::<std::sync::Mutex<Option<crate::first_read::Key>>>()
                    + 2 * size_of::<std::sync::atomic::AtomicBool>()
            })
    }

    fn protected_bytes(&self) -> usize {
        Self::chunk_bytes(&self.protected_chunks)
    }

    fn chunk_bytes(chunks: &Vec<crate::storage::VerifiedChunk>) -> usize {
        chunks.capacity() * size_of::<crate::storage::VerifiedChunk>()
            + usize::from(chunks.capacity() != 0) * 256
            + chunks.iter().map(|v| v.retained_bytes()).sum::<usize>()
    }

    fn joint_cache_entries(&self) -> usize {
        self.cache.len() + self.protected_chunks.len()
    }

    fn note_joint_retention(&mut self) {
        self.metrics.joint_cache_retained_peak_bytes = self
            .metrics
            .joint_cache_retained_peak_bytes
            .max((self.cache_bytes + self.metadata_bytes()) as u64);
    }

    // Newest secondary first, then the anchor. Release empty backing capacity.
    fn evict_protected_chunk(&mut self) {
        if self.protected_chunks.pop().is_some() {
            self.metrics.protected_chunk_evictions += 1;
            self.metrics.protected_chunk_pressure_evictions += 1;
        }
        if self.protected_chunks.is_empty() {
            self.protected_chunks = Vec::new();
        }
    }

    fn clear_protected_chunks(&mut self) {
        self.metrics.protected_chunk_evictions += self.protected_chunks.len() as u64;
        self.metrics.protected_chunk_reset_evictions += self.protected_chunks.len() as u64;
        self.protected_chunks = Vec::new();
    }

    #[cfg(test)]
    fn admit_protected_chunk(&mut self, candidate: crate::storage::VerifiedChunk) {
        let limit = if self.ordinary_chunk_admission {
            self.protected_limit()
        } else {
            0
        };
        let non_chunk_bytes = self.cache_bytes + self.metadata_bytes() - self.protected_bytes();
        Self::admit_chunk(
            &mut self.protected_chunks,
            &mut self.metrics,
            &candidate,
            limit,
            non_chunk_bytes,
            self.cache.len(),
        );
        self.note_joint_retention();
    }

    fn admit_chunk(
        chunks: &mut Vec<crate::storage::VerifiedChunk>,
        metrics: &mut Metrics,
        candidate: &crate::storage::VerifiedChunk,
        limit: usize,
        non_chunk_bytes: usize,
        response_entries: usize,
    ) {
        if limit == 0
            || candidate.bytes().len() > CACHE_ENTRY_BYTES
            || chunks
                .iter()
                .any(|old| old.descriptor() == candidate.descriptor())
        {
            return;
        }
        if chunks.len() >= limit || response_entries + chunks.len() >= CACHE_ENTRIES {
            metrics.protected_chunk_rejections += 1;
            return;
        }
        // Reserve old+new backing ownership before a possible Vec allocation.
        let growth = if chunks.len() == chunks.capacity() {
            (chunks.len() + 1) * size_of::<crate::storage::VerifiedChunk>() + 256
        } else {
            0
        };
        let chunk_needed = Self::chunk_bytes(chunks) + candidate.retained_bytes() + growth;
        // Unallocated admission heuristic only: no response cap or owned reserve.
        let secondary_headroom = 15 * 1024 * 1024;
        if non_chunk_bytes + chunk_needed > CACHE_BYTES
            || (!chunks.is_empty() && chunk_needed > CACHE_BYTES - secondary_headroom)
        {
            metrics.protected_chunk_rejections += 1;
            return;
        }
        metrics.joint_cache_retained_peak_bytes = metrics
            .joint_cache_retained_peak_bytes
            .max((non_chunk_bytes + chunk_needed) as u64);
        if growth != 0 {
            chunks.reserve_exact(1);
        }
        chunks.push(candidate.clone());
        metrics.protected_chunk_admissions += 1;
        metrics.joint_cache_retained_peak_bytes = metrics
            .joint_cache_retained_peak_bytes
            .max((non_chunk_bytes + Self::chunk_bytes(chunks)) as u64);
    }

    fn prune_protected_chunks(&mut self) -> io::Result<()> {
        let live = crate::snapshot_chunks(&self.manifest)?;
        let before = self.protected_chunks.len();
        self.protected_chunks
            .retain(|v| live.contains(v.descriptor()));
        self.metrics.protected_chunk_prunes += (before - self.protected_chunks.len()) as u64;
        if self.protected_chunks.is_empty() {
            self.protected_chunks = Vec::new();
        }
        self.note_joint_retention();
        Ok(())
    }

    fn reserve_metadata(&mut self, incoming: usize) -> bool {
        self.trim_joint(incoming, false)
    }

    fn trim_cache(&mut self, incoming: usize) -> bool {
        self.trim_joint(incoming, incoming != 0)
    }

    fn trim_joint(&mut self, incoming: usize, entry: bool) -> bool {
        while self.cache_bytes + self.metadata_bytes() + incoming > CACHE_BYTES
            || self.joint_cache_entries() + usize::from(entry) > CACHE_ENTRIES
        {
            if !self.protected_chunks.is_empty() {
                self.evict_protected_chunk();
            } else if let Some((old_key, old)) = self.cache.pop_front() {
                self.cache_bytes -= Self::cache_weight(old_key.capacity(), &old);
                self.metrics.cache_evictions += 1;
            } else {
                return false;
            }
        }
        self.note_joint_retention();
        true
    }

    fn stable_full_selection(&self) -> io::Result<String> {
        let chunks = crate::snapshot_chunks(&self.manifest)?;
        let selected: Vec<_> = (0..self.manifest.packs.len()).collect();
        Ok(crate::first_read::stable_selection_hash(
            &selected, None, &chunks,
        ))
    }

    fn gateway_plan(
        &self,
        request: &Request,
    ) -> io::Result<Option<(crate::first_read::Plan, Option<history::Projection>)>> {
        if !self.cold_view
            || self.cloudflare.is_none()
            || self.view_dirty
            || self.publication_active
        {
            return Ok(None);
        }
        validate_snapshot(&self.manifest)?;
        let Some(authority) = self.authority_fingerprint.as_ref().filter(|binding| {
            binding.generation == self.manifest.generation
                && binding.local == crate::first_read::manifest_digest(&self.manifest)
        }) else {
            return Ok(None);
        };
        let projection = if request.target == "/repo.git/git-receive-pack" {
            if !eligible_small_receive(request) {
                return Ok(None);
            }
            None
        } else {
            let identity = advertisement::identity(&self.view);
            let Some(selection) = self
                .lineage
                .as_ref()
                .and_then(|proof| proof.selection(request, &self.manifest, identity))
            else {
                return Ok(None);
            };
            (selection.packs.len() < self.manifest.packs.len()).then_some(selection)
        };
        let selected = projection
            .as_ref()
            .map(|p| p.packs.clone())
            .unwrap_or_else(|| (0..self.manifest.packs.len()).collect());
        let chunks: Vec<_> = selected
            .iter()
            .flat_map(|i| self.manifest.packs[*i].chunks().cloned())
            .collect();
        let cached: Vec<_> = self
            .protected_chunks
            .iter()
            .map(|v| v.descriptor())
            .collect();
        let missing: Vec<_> = chunks
            .iter()
            .filter(|c| !cached.contains(c))
            .cloned()
            .collect();
        let count = crate::cloudflare::bulk_batch_len(&missing);
        if count == 0 {
            return Ok(None);
        }
        let first = missing[..count].to_vec();
        let stable = crate::first_read::stable_selection_hash(
            &selected,
            projection.as_ref().map(|p| (p.cutoff, p.binding)),
            &chunks,
        );
        Ok(Some((
            crate::first_read::Plan {
                generation: self.manifest.generation,
                manifest: authority
                    .published
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
                selection: crate::first_read::cached_selection_hash(
                    &stable, &chunks, &cached, &first,
                ),
                first,
            },
            projection,
        )))
    }

    fn gateway_private(&mut self, request: Request) -> io::Result<Response> {
        use crate::first_read as fr;
        let services = self
            .cloudflare
            .clone()
            .ok_or_else(|| io::Error::other("private protocol unavailable"))?;
        let key = services
            .first_read_key()?
            .ok_or_else(|| io::Error::other("private protocol unavailable"))?;
        if request.method != "POST" || request.body.len() > fr::MAX_FRAME {
            return Err(io::Error::other("private protocol method or frame bound"));
        }
        if request.target == fr::PLAN_PATH {
            key.verify("git-zero-fetch-plan-request-v1", &request)?;
            let (meta, body, bulk): (fr::Planning, _, _) = fr::decode(request.body)?;
            fr::identity(
                meta.version,
                &meta.session,
                &meta.nonce,
                &meta.repo,
                &key,
                services.repo(),
            )?;
            if !bulk.is_empty() {
                return Err(io::Error::other("unexpected plan payload"));
            }
            let mut logical = meta.request.request(body)?;
            let receive = logical.target == "/repo.git/git-receive-pack";
            if receive && !key.receive {
                return Err(io::Error::other("receive preparation not negotiated"));
            }
            let request_hash = fr::request_hash(&logical);
            let witness = services
                .witness_for(&logical, self.manifest.generation)?
                .ok_or_else(|| io::Error::other("missing witness capability"))?;
            logical
                .headers
                .insert("x-git-zero-internal-generation-witness".into(), witness);
            let hit = !receive
                && self
                    .cache
                    .iter()
                    .any(|(k, _)| k == &self.cache_key(&logical));
            let plan = if hit {
                None
            } else {
                self.gateway_plan(&logical)?.map(|(p, _)| p)
            };
            if receive {
                self.metrics.gateway_receive_plans += 1;
                self.metrics.gateway_receive_unsupported += u64::from(plan.is_none());
            } else {
                self.metrics.gateway_read_plans += 1;
                self.metrics.gateway_read_unsupported += u64::from(plan.is_none());
            }
            let result = fr::Planned {
                version: 1,
                session: meta.session,
                nonce: meta.nonce,
                request_hash,
                status: if plan.is_some() {
                    "supported"
                } else {
                    "unsupported"
                },
                plan,
            };
            if services.first_read_key()? != Some(key) {
                return Err(io::Error::other("registration changed during plan"));
            }
            key.response("git-zero-fetch-plan-response-v1", fr::encode(&result)?)
        } else {
            key.verify("git-zero-fetch-continue-v1", &request)?;
            let (meta, body, bulk): (fr::Continuing, _, _) = fr::decode(request.body)?;
            fr::identity(
                meta.version,
                &meta.session,
                &meta.nonce,
                &meta.repo,
                &key,
                services.repo(),
            )?;
            let mut logical = meta.request.request(body)?;
            let receive = logical.target == "/repo.git/git-receive-pack";
            if receive && !key.receive {
                return Err(io::Error::other("receive preparation not negotiated"));
            }
            if fr::request_hash(&logical) != meta.request_hash
                || meta.generation > 9_007_199_254_740_991
            {
                return Err(io::Error::other("continuation identity mismatch"));
            }
            logical.headers.insert(
                "x-git-zero-internal-generation-witness".into(),
                meta.witness,
            );
            if !services.generation_witness_matches(&logical, meta.generation)? {
                return Err(io::Error::other("continuation witness mismatch"));
            }
            if !meta.matched {
                if !bulk.is_empty() || meta.generation == meta.plan.generation {
                    return Err(io::Error::other("invalid mismatch continuation"));
                }
                self.metrics.gateway_read_generation_changed += 1;
                return Ok(self.handle(logical));
            }
            if meta.generation != meta.plan.generation {
                return Err(io::Error::other("prefetch generation mismatch"));
            }
            // Authenticate the entire frame first; then prove every real byte
            // against exact signed chunk keys before any fallback or files.
            let prepare_start = Instant::now();
            let verified = crate::cloudflare::decode_bulk_response(&meta.plan.first, &bulk)?;
            drop(bulk);
            let current = self.gateway_plan(&logical)?;
            let cache_hit = !receive
                && self
                    .cache
                    .iter()
                    .any(|(k, _)| k == &self.cache_key(&logical));
            let Some((plan, projection)) = current.filter(|(p, _)| *p == meta.plan && !cache_hit)
            else {
                drop(verified);
                self.metrics.gateway_read_paid_discards += 1;
                return Ok(self.handle(logical));
            };
            if services.first_read_key()? != Some(key) {
                return Err(io::Error::other("registration changed during continuation"));
            }
            self.metrics.requests += 1;
            self.metrics.input_bytes += logical.body.len() as u64;
            self.metrics.generation_witness_hits += 1;
            self.ordinary_chunk_admission = true;
            if receive {
                services.set_phase("push")?;
                let was_projection = matches!(self.scope, ViewScope::Projection { .. });
                self.materialize_view_prefetched(
                    &self.manifest.clone(),
                    None,
                    Some((plan.first.clone(), verified)),
                )?;
                self.metrics.cloud_view_rehydrates += 1;
                self.metrics.history_full_restores += u64::from(was_projection);
                let prepared = ValidatedFullReceive {
                    authority: self
                        .authority_fingerprint
                        .ok_or_else(|| io::Error::other("missing prepared authority"))?,
                    request_hash: fr::request_hash(&logical),
                    stable_selection: self.stable_full_selection()?,
                    key,
                    nonce: meta.nonce,
                    prepare_start,
                };
                self.metrics.gateway_receive_consumed += 1;
                let response = self.receive_prepared(&logical, Some(prepared))?;
                self.metrics.output_bytes += response.body.len() as u64;
                return Ok(response);
            }
            self.metrics.cache_misses += 1;
            services.set_phase("fetch")?;
            let key = self.cache_key(&logical);
            let was_projection = matches!(self.scope, ViewScope::Projection { .. });
            self.materialize_view_prefetched(
                &self.manifest.clone(),
                projection.as_ref(),
                Some((plan.first, verified)),
            )?;
            self.metrics.cloud_view_rehydrates += 1;
            self.metrics.history_projections += u64::from(projection.is_some());
            self.metrics.history_full_restores += u64::from(projection.is_none() && was_projection);
            self.metrics.gateway_read_consumed += 1;
            let start = Instant::now();
            let response = self.native_read(&logical)?;
            self.metrics.upload_codec_seconds += start.elapsed().as_secs_f64();
            self.cache_insert(key, &response);
            self.metrics.output_bytes += response.body.len() as u64;
            Ok(response)
        }
    }

    fn request_error_cleanup(&mut self) {
        self.advertisement = None;
        self.lineage = None;
        self.clear_protected_chunks();
        self.trim_cache(0);
        if self.view_dirty || !matches!(self.scope, ViewScope::Full { .. }) {
            self.view_dirty = true;
            self.scope = ViewScope::Invalid;
        }
    }

    pub fn handle(&mut self, request: Request) -> Response {
        let previous = self.ordinary_chunk_admission;
        self.ordinary_chunk_admission = request.method == "POST"
            && matches!(
                request.target.as_str(),
                "/repo.git/git-upload-pack" | "/repo.git/git-receive-pack"
            )
            || request.method == "GET" && request.target.starts_with("/repo.git/info/refs?");
        // HttpServices can change registration/socket policy between calls.
        // Re-evaluate its reservation before any new cross-request owner.
        self.trim_cache(0);
        let response = self.handle_inner(request);
        self.ordinary_chunk_admission = previous;
        self.trim_cache(0);
        response
    }

    fn handle_inner(&mut self, request: Request) -> Response {
        if matches!(
            request.target.as_str(),
            crate::first_read::PLAN_PATH | crate::first_read::CONTINUE_PATH
        ) {
            let requests_before = self.metrics.requests;
            return self.gateway_private(request).unwrap_or_else(|error| {
                self.metrics.gateway_read_errors += 1;
                eprintln!("private request failed: {error}");
                let response = Response::error(503, "private request failed");
                // Planning/auth calls are private overhead. Once a continuation
                // begins its logical request, errors count output like dispatch.
                if self.metrics.requests != requests_before {
                    self.request_error_cleanup();
                    self.metrics.output_bytes += response.body.len() as u64;
                }
                response
            });
        }
        if request.method == "GET"
            && matches!(request.target.as_str(), "/metrics" | "/_object-stats")
        {
            let result = (|| -> io::Result<serde_json::Value> {
                if let Some(services) = &self.cloudflare {
                    services.set_phase("stats")?;
                }
                if request.target == "/_object-stats" {
                    if self.view_dirty {
                        self.recover()?;
                    }
                    self.require_full_view()?;
                }
                self.metrics.history_lineage_bytes = self.lineage_bytes() as u64;
                self.metrics.history_pending_lineage_bytes = self.pending_lineage_bytes as u64;
                self.metrics.upload_advertisement_template_bytes =
                    self.advertisement_bytes() as u64;
                self.metrics.cache_bytes = self.cache_bytes as u64;
                self.metrics.cache_retained_bytes =
                    (self.cache_bytes + self.metadata_bytes()) as u64;
                self.metrics.cache_budget_bytes = CACHE_BYTES as u64;
                self.metrics.cache_entries = self.cache.len() as u64;
                self.metrics.protected_chunk_bytes = self
                    .protected_chunks
                    .iter()
                    .map(|v| v.bytes().len() as u64)
                    .sum();
                self.metrics.protected_chunk_entries = self.protected_chunks.len() as u64;
                self.metrics.protected_chunk_owning_bytes = self.protected_bytes() as u64;
                self.metrics.joint_cache_entries = self.joint_cache_entries() as u64;
                self.metrics.cache_entry_limit = CACHE_ENTRIES as u64;
                let mut value = serde_json::to_value(&self.metrics)?;
                value["native_view_policy"] = serde_json::json!(if self.cold_view {
                    "strict-v0-lineage-selection-on-cold-cache-miss"
                } else {
                    "persistent-native-view-up-to128MiB-packs-and-indexes"
                });
                let total_bytes: u64 = self.manifest.packs.iter().fold(0u64, |sum, p| {
                    sum.saturating_add(p.pack_bytes)
                        .saturating_add(p.index_bytes)
                });
                value["native_view"] = match self.scope {
                    ViewScope::Invalid => serde_json::json!({"scope":"invalid"}),
                    ViewScope::Full { generation } => {
                        serde_json::json!({"scope":"full", "generation":generation, "selected_packs":self.manifest.packs.len(),"total_packs":self.manifest.packs.len(),"selected_bytes":total_bytes,"total_bytes":total_bytes})
                    }
                    ViewScope::Projection {
                        generation,
                        cutoff,
                        binding,
                        packs,
                        bytes,
                    } => {
                        serde_json::json!({"scope":"projection", "generation":generation,"cutoff":cutoff,"selected_packs":packs,"total_packs":self.manifest.packs.len(),"selected_bytes":bytes,"total_bytes":total_bytes,"snapshot_binding":binding.iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
                    }
                };
                if request.target == "/_object-stats" {
                    value["manifest"] = serde_json::to_value(&self.manifest)?;
                    value["objects"] = crate::objects::inventory(&self.view, &self.manifest)?;
                }
                if let Some(services) = &self.cloudflare {
                    value["native_socket_rpc"] = services.socket_diagnostics()?;
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
            self.request_error_cleanup();
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

    fn verified_payload(bytes: Vec<u8>) -> crate::storage::VerifiedChunk {
        // Give a short payload a larger backing allocation to prove charging
        // follows capacity rather than length or a logical view.
        let descriptor = Chunk {
            key: format!("chunks/{}", hash(&bytes)),
            len: bytes.len() as u64,
        };
        crate::storage::VerifiedChunks::verify(std::slice::from_ref(&descriptor), vec![bytes])
            .unwrap()
            .into_handles(std::slice::from_ref(&descriptor))
            .unwrap()
            .remove(0)
    }

    #[test]
    fn protected_payload_is_get_only_and_pressure_evicts_it_for_a_productive_response() {
        let root =
            std::env::temp_dir().join(format!("git-zero-protected-budget-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.protected_mode = ProtectedChunkMode::One;
        let payload = verified_payload(vec![7; CACHE_ENTRY_BYTES]);
        host.admit_protected_chunk(payload.clone());
        assert!(host.protected_chunks.is_empty(), "setup cannot admit");
        host.ordinary_chunk_admission = true;
        host.admit_protected_chunk(payload);
        assert_eq!(host.metrics.protected_chunk_admissions, 1);
        assert!(host.protected_bytes() > CACHE_ENTRY_BYTES);
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Arc::from(vec![9; CACHE_ENTRY_BYTES]),
        };
        host.cache_insert("max-response".into(), &response);
        assert!(
            host.protected_chunks.is_empty(),
            "slot is evictable under the same budget"
        );
        assert_eq!(host.cache.len(), 1);
        assert_eq!(host.metrics.protected_chunk_evictions, 1);
        assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        assert!(host.metrics.joint_cache_retained_peak_bytes <= CACHE_BYTES as u64);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn protected_slot_consumes_one_entry_and_full_token_identity_ignores_admission() {
        let root =
            std::env::temp_dir().join(format!("git-zero-protected-entry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.protected_mode = ProtectedChunkMode::One;
        host.ordinary_chunk_admission = true;
        let stable = host.stable_full_selection().unwrap();
        host.admit_protected_chunk(verified_payload(vec![7; 100]));
        for index in 0..300 {
            host.cache_insert(
                index.to_string(),
                &Response {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: Arc::from([1u8]),
                },
            );
            assert!(host.joint_cache_entries() <= CACHE_ENTRIES);
            assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        }
        assert!(host.protected_chunks.is_empty());
        assert_eq!(host.cache.len(), 256);
        assert_eq!(stable, host.stable_full_selection().unwrap());
        host.cache = VecDeque::new();
        host.cache_bytes = 0;
        assert_eq!(host.joint_cache_entries(), 0);
        host.request_error_cleanup();
        assert!(host.protected_chunks.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multi_chunk_first_fit_prunes_full_authority_and_promotes_without_replacement() {
        let root =
            std::env::temp_dir().join(format!("git-zero-multi-policy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.protected_mode = ProtectedChunkMode::Multi;
        host.ordinary_chunk_admission = true;
        let anchor = verified_payload(vec![1; 9 * 1024 * 1024]);
        let secondary = verified_payload(vec![2; 4 * 1024 * 1024]);
        let rejected = verified_payload(vec![3; 5 * 1024 * 1024]);
        let tiny = verified_payload(vec![4; 100]);
        host.admit_protected_chunk(anchor.clone());
        host.admit_protected_chunk(secondary.clone());
        let entries = host
            .protected_chunks
            .iter()
            .map(|v| v.descriptor().clone())
            .collect::<Vec<_>>();
        host.admit_protected_chunk(rejected);
        assert_eq!(
            host.protected_chunks
                .iter()
                .map(|v| v.descriptor().clone())
                .collect::<Vec<_>>(),
            entries
        );
        assert_eq!(host.metrics.protected_chunk_rejections, 1);
        host.admit_protected_chunk(tiny.clone());
        assert_eq!(
            host.protected_chunks.len(),
            3,
            "late small fitting item is considered"
        );
        // Proposed or projected manifests have no pruning side effect. Only
        // the explicit trusted Full install hook performs this operation.
        let mut next = Manifest::default();
        next.packs.push(StoredPack {
            id: "live".into(),
            bundle: vec![secondary.descriptor().clone(), tiny.descriptor().clone()],
            pack_bytes: secondary.descriptor().len,
            index_bytes: tiny.descriptor().len,
            ..StoredPack::default()
        });
        assert_eq!(host.protected_chunks.len(), 3);
        host.manifest = next;
        host.prune_protected_chunks().unwrap();
        assert_eq!(host.metrics.protected_chunk_prunes, 1);
        assert_eq!(
            host.protected_chunks[0].descriptor(),
            secondary.descriptor()
        );
        host.admit_protected_chunk(anchor);
        assert_eq!(
            host.protected_chunks[0].descriptor(),
            secondary.descriptor()
        );
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Arc::from(vec![5; CACHE_ENTRY_BYTES]),
        };
        host.cache_insert("eligible-above14m".into(), &response);
        assert_eq!(host.cache.len(), 1);
        assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        assert!(host.metrics.joint_cache_retained_peak_bytes <= CACHE_BYTES as u64);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multi_chunk_pool_and_joint_entry_limits_reclaim_newest_secondary_first() {
        let root =
            std::env::temp_dir().join(format!("git-zero-multi-entries-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.protected_mode = ProtectedChunkMode::Multi;
        host.ordinary_chunk_admission = true;
        for index in 0..32u8 {
            host.admit_protected_chunk(verified_payload(vec![index; 100]));
        }
        assert_eq!(host.protected_chunks.len(), 32);
        let anchor = host.protected_chunks[0].descriptor().clone();
        let newest = host.protected_chunks[31].descriptor().clone();
        host.admit_protected_chunk(verified_payload(vec![99; 100]));
        assert_eq!(host.protected_chunks.len(), 32);
        assert_eq!(host.metrics.protected_chunk_rejections, 1);
        for index in 0..224 {
            host.cache_insert(
                index.to_string(),
                &Response {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: Arc::from([1u8]),
                },
            );
        }
        assert_eq!(host.joint_cache_entries(), 256);
        assert_eq!(host.protected_chunks.len(), 32);
        host.cache_insert(
            "pressure-one-more".into(),
            &Response {
                status: 200,
                headers: BTreeMap::new(),
                body: Arc::from([1u8]),
            },
        );
        assert_eq!(host.protected_chunks.len(), 31);
        assert_eq!(host.protected_chunks[0].descriptor(), &anchor);
        assert!(
            !host
                .protected_chunks
                .iter()
                .any(|v| v.descriptor() == &newest)
        );
        assert_eq!(host.metrics.protected_chunk_pressure_evictions, 1);
        assert_eq!(host.joint_cache_entries(), 256);
        assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observer_full_recovery_cannot_admit_or_prune_the_immutable_pool() {
        let root =
            std::env::temp_dir().join(format!("git-zero-multi-observer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.protected_mode = ProtectedChunkMode::Multi;
        host.ordinary_chunk_admission = true;
        host.admit_protected_chunk(verified_payload(vec![1; 100]));
        host.ordinary_chunk_admission = false;
        host.recover().unwrap();
        assert_eq!(host.protected_chunks.len(), 1);
        assert_eq!(host.metrics.protected_chunk_admissions, 1);
        assert_eq!(host.metrics.protected_chunk_prunes, 0);
        host.ordinary_chunk_admission = true;
        host.recover().unwrap();
        assert!(host.protected_chunks.is_empty());
        assert_eq!(host.metrics.protected_chunk_prunes, 1);
        assert_eq!(host.metrics.protected_chunk_admissions, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn small_receive_classifier_only_prepares_supported_nonempty_command_pack_requests() {
        fn request(command: &[u8], tail: &[u8]) -> Request {
            let mut body = format!("{:04x}", command.len() + 4).into_bytes();
            body.extend(command);
            body.extend(b"0000");
            body.extend(tail);
            Request {
                method: "POST".into(),
                target: "/repo.git/git-receive-pack".into(),
                headers: BTreeMap::from([(
                    "content-type".into(),
                    "application/x-git-receive-pack-request".into(),
                )]),
                body,
            }
        }
        let command = format!(
            "{} {} refs/heads/main\0 report-status-v2 side-band-64k quiet object-format=sha1 agent=git/2.53.0",
            "0".repeat(40),
            "1".repeat(40)
        );
        let mut pack = b"PACK".to_vec();
        pack.extend(2u32.to_be_bytes());
        pack.extend(1u32.to_be_bytes());
        pack.extend([0; 20]);
        let mut good = request(command.as_bytes(), &pack);
        assert!(eligible_small_receive(&good));
        assert!(!eligible_small_receive(&request(command.as_bytes(), b"")));
        assert!(!eligible_small_receive(&request(b"invalid", &pack)));
        assert!(!eligible_small_receive(&request(
            command.replace("quiet", "push-options").as_bytes(),
            &pack
        )));
        assert!(!eligible_small_receive(&request(
            command
                .replace("refs/heads/main", "refs/heads/a..b")
                .as_bytes(),
            &pack
        )));
        let mut bad = pack.clone();
        bad[7] = 4;
        assert!(!eligible_small_receive(&request(command.as_bytes(), &bad)));
        bad = pack.clone();
        bad[11] = 0;
        assert!(!eligible_small_receive(&request(command.as_bytes(), &bad)));
        assert!(!eligible_small_receive(&request(
            command.as_bytes(),
            &pack[..31]
        )));
        good.body = b"0000".to_vec();
        assert!(!eligible_small_receive(&good));
        good = request(command.as_bytes(), &pack);
        good.headers
            .insert("git-protocol".into(), "version=2".into());
        assert!(!eligible_small_receive(&good));
        good.headers.remove("git-protocol");
        good.body.resize(crate::first_read::MAX_ORIGINAL + 1, 0);
        assert!(!eligible_small_receive(&good));
    }

    #[test]
    fn gateway_cache_normalization_preserves_semantics_and_ignores_only_named_transport_fields() {
        let root = std::env::temp_dir().join(format!("git-zero-r15-cache-{}", std::process::id()));
        let host = NativeHost::open(root.clone()).unwrap();
        let base = Request {
            method: "POST".into(),
            target: "/repo.git/git-upload-pack".into(),
            headers: BTreeMap::new(),
            body: b"request".to_vec(),
        };
        let key = host.cache_key(&base);
        for name in [
            "host",
            "connection",
            "content-length",
            "transfer-encoding",
            "accept-encoding",
            "cf-worker",
            "cf-connecting-ip",
            crate::first_read::AUTH_HEADER,
        ] {
            let mut other = base.clone();
            other.headers.insert(name.into(), "transport".into());
            assert_eq!(key, host.cache_key(&other), "{name}");
        }
        for name in [
            "authorization",
            "cookie",
            "git-protocol",
            "content-type",
            "content-encoding",
            "accept",
            "user-agent",
            "cf-custom",
            "x-custom",
            "x-git-zero-internal-generation-witness",
        ] {
            let mut other = base.clone();
            other.headers.insert(name.into(), "semantic".into());
            assert_ne!(key, host.cache_key(&other), "{name}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deferred_catalog_proof_slot_is_reserved_inside_the_existing_cache_budget() {
        let root = std::env::temp_dir().join(format!(
            "git-zero-proof-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.cache = VecDeque::with_capacity(4);
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: vec![b'x'; CACHE_ENTRY_BYTES - 1024].into(),
        };
        host.cache_insert("first".into(), &response);
        host.cache_insert("second".into(), &response);
        let key = String::from("remaining");
        let mut padding = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Arc::from([]),
        };
        let overhead = NativeHost::cache_weight(key.capacity(), &padding);
        let remaining = CACHE_BYTES - host.metadata_bytes() - host.cache_bytes - overhead;
        padding.body = vec![b'y'; remaining].into();
        host.cache_insert(key, &padding);
        assert_eq!(host.cache_bytes + host.metadata_bytes(), CACHE_BYTES);
        let metadata = host.metadata_bytes();
        host.cloudflare = Some(Arc::new(
            HttpServices::new("http://127.0.0.1:1", "proof", "token").unwrap(),
        ));
        assert_eq!(
            host.metadata_bytes() - metadata,
            HttpServices::retirement_proof_storage_bytes()
                + host.cloudflare.as_ref().unwrap().socket_storage_bytes()
                + size_of::<std::sync::Mutex<Option<crate::first_read::Key>>>()
                + 2 * size_of::<std::sync::atomic::AtomicBool>()
        );
        host.trim_cache(0);
        assert_eq!(host.metrics.cache_evictions, 1);
        assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        assert_eq!(CACHE_BYTES, 32 * 1024 * 1024);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ambiguous_early_scalar_cleanup_marks_view_dirty_before_returning() {
        use std::io::{BufRead, BufReader};
        let root = std::env::temp_dir().join(format!(
            "git-zero-scalar-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut host = NativeHost::open(root.clone()).unwrap();
        host.manifest.garbage = vec![format!("chunks/{}", hash(b"old"))];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let services = Arc::new(
            HttpServices::new(
                &format!("http://{}", listener.local_addr().unwrap()),
                "cleanup",
                "token",
            )
            .unwrap(),
        );
        let key = host.manifest.garbage[0].clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with(&format!("DELETE /_services/blob/{key} ")));
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            // The DELETE may already have committed; its incomplete reply
            // must force recovery on the following request.
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
                .unwrap();
        });
        host.repository.blobs = HostBlobs::Cloudflare(R2BlobStore::new(services.clone()), services);
        let request = Request {
            method: "POST".into(),
            target: "/repo.git/git-receive-pack".into(),
            headers: BTreeMap::new(),
            body: vec![],
        };
        assert!(!host.view_dirty);
        assert!(host.receive_transaction(&request, None, None).is_err());
        assert!(host.view_dirty);
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fused_partial_failure_retains_debt_and_lost_committed_reply_recovers_before_read() {
        use crate::storage::{RetirementOutcome, RetirementReceipt};
        use std::io::{BufRead, BufReader};
        for lose_reply in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "git-zero-retire-recovery-{}-{}-{lose_reply}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut host = NativeHost::open(root.clone()).unwrap();
            let source = root.join("source.git");
            init(&source).unwrap();
            let output = |args: &[&str], input: &[u8]| {
                let mut process = Command::new("git");
                process.arg("-C").arg(&source).args(args);
                String::from_utf8(command(process, input).unwrap().stdout)
                    .unwrap()
                    .trim()
                    .to_owned()
            };
            let mut previous = "0".repeat(40);
            let mut pushes = Vec::new();
            for revision in 0..4 {
                let blob = output(
                    &["hash-object", "-w", "--stdin"],
                    format!("revision {revision}\n").as_bytes(),
                );
                let tree = output(
                    &["mktree"],
                    format!("100644 blob {blob}\tpayload\n").as_bytes(),
                );
                let mut args = vec![
                    "-c",
                    "user.name=Contract",
                    "-c",
                    "user.email=contract@example.invalid",
                    "commit-tree",
                    &tree,
                ];
                if revision != 0 {
                    args.extend(["-p", &previous]);
                }
                let tip = output(&args, format!("revision {revision}\n").as_bytes());
                let line =
                    format!("{previous} {tip} refs/heads/main\0report-status atomic ofs-delta\n");
                let mut body = format!("{:04x}{line}0000", line.len() + 4).into_bytes();
                let mut revisions = format!("{tip}\n");
                if revision != 0 {
                    revisions.push_str(&format!("^{previous}\n"));
                }
                let mut pack = Command::new("git");
                pack.arg("-C").arg(&source).args([
                    "pack-objects",
                    "--stdout",
                    "--revs",
                    "--thin",
                    "--delta-base-offset",
                ]);
                body.extend(command(pack, revisions.as_bytes()).unwrap().stdout);
                pushes.push(Request {
                    method: "POST".into(),
                    target: "/repo.git/git-receive-pack".into(),
                    headers: BTreeMap::from([(
                        "content-type".into(),
                        "application/x-git-receive-pack-request".into(),
                    )]),
                    body,
                });
                previous = tip;
            }
            for request in pushes.drain(..3) {
                assert_eq!(host.handle(request).status, 200);
            }
            assert_eq!(host.manifest.generation, 3);
            let native = git_rpc(
                &host.view,
                &Request {
                    method: "GET".into(),
                    target: "/repo.git/info/refs?service=git-upload-pack".into(),
                    headers: BTreeMap::new(),
                    body: vec![],
                },
            )
            .unwrap();
            host.advertisement = advertisement::Certificate::learn(
                &host.manifest,
                &native,
                advertisement::identity(&host.view).unwrap(),
            );
            assert!(host.advertisement.is_some());
            host.lineage = history::Certificate::bootstrap(
                &host.manifest,
                advertisement::identity(&host.view).unwrap(),
            );
            assert!(host.lineage.is_some());
            let local = LocalServices::new(root.join("services")).unwrap();
            let authoritative = local.state.clone();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let services = Arc::new(
                HttpServices::new(
                    &format!("http://{}", listener.local_addr().unwrap()),
                    "retire",
                    "token",
                )
                .unwrap(),
            );
            let server = std::thread::spawn(move || {
                let mut retired = Vec::new();
                for step in 0..if lose_reply { 3 } else { 2 } {
                    let (stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let target = line.clone();
                    let mut length = 0;
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.strip_prefix("Content-Length: ") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let reply = match step {
                        0 => {
                            assert!(target.starts_with("POST /_services/register "));
                            serde_json::json!({"registered":true,"manifest_retire":{"version":1}})
                        }
                        1 => {
                            assert!(
                                target.starts_with("POST /_services/manifest/retire "),
                                "{target}"
                            );
                            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                            let next: Manifest =
                                serde_json::from_value(value["next"].clone()).unwrap();
                            assert_eq!(value["expected"], 3);
                            assert_eq!(next.generation, 4);
                            retired = next
                                .retirement_keys()
                                .into_iter()
                                .map(str::to_owned)
                                .collect();
                            assert!(retired.len() >= 3);
                            assert!(local.state.compare_and_swap(3, &next).unwrap());
                            let outcomes = retired
                                .iter()
                                .enumerate()
                                .map(|(i, key)| {
                                    if i != 0 {
                                        local.blobs.delete(key).unwrap();
                                    }
                                    RetirementOutcome {
                                        key: key.clone(),
                                        deleted: i != 0,
                                    }
                                })
                                .collect();
                            if lose_reply {
                                continue;
                            }
                            serde_json::json!({"version":1,"published":true,"retirement":RetirementReceipt {generation:4,outcomes}})
                        }
                        2 => {
                            assert!(
                                target.starts_with("GET /_services/manifest "),
                                "recovery must load rather than republish: {target}"
                            );
                            serde_json::to_value(local.state.load().unwrap()).unwrap()
                        }
                        _ => unreachable!(),
                    };
                    let reply = serde_json::to_vec(&reply).unwrap();
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        reply.len()
                    )
                    .unwrap();
                    reader.get_mut().write_all(&reply).unwrap();
                }
                retired
            });
            services.register("http://127.0.0.1:1").unwrap();
            host.repository.state = HostState::Cloudflare(DurableStateStore::new(services));
            let before = host.metrics.retired_chunk_deletes;
            let response = host.handle(pushes.pop().unwrap());
            assert_eq!(response.status, if lose_reply { 503 } else { 200 });
            assert_eq!(host.view_dirty, lose_reply);
            assert_eq!(host.advertisement.is_none(), lose_reply);
            assert_eq!(host.lineage.is_none(), lose_reply);
            assert_eq!(host.pending_lineage_bytes, 0);
            assert_eq!(host.pending_advertisement_bytes, 0);
            assert_eq!(authoritative.load().unwrap().generation, 4);
            if !lose_reply {
                assert_eq!(host.manifest.garbage.len(), 1);
                assert_eq!(
                    host.metrics.retired_chunk_deletes - before,
                    (authoritative.load().unwrap().garbage.len() - 1) as u64
                );
            }
            let response = host.handle(Request {
                method: "GET".into(),
                target: "/repo.git/info/refs?service=git-receive-pack".into(),
                headers: BTreeMap::new(),
                body: vec![],
            });
            assert_eq!(response.status, 200);
            assert!(!host.view_dirty);
            if lose_reply {
                assert!(host.advertisement.is_none());
                assert!(host.lineage.is_none());
            }
            assert!(matches!(host.scope, ViewScope::Full { generation: 4 }));
            assert_eq!(host.manifest.generation, 4);
            assert_eq!(refs(&host.view).unwrap()["refs/heads/main"], previous);
            let mut fsck = Command::new("git");
            fsck.arg("-C").arg(&host.view).args(["fsck", "--strict"]);
            command(fsck, &[]).unwrap();
            let retired = server.join().unwrap();
            assert_eq!(authoritative.load().unwrap().garbage, retired);
            if lose_reply {
                assert!(host.repository.blobs.get(&retired[0]).is_err());
            } else {
                assert_eq!(host.manifest.garbage, vec![retired[0].clone()]);
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn witness_only_skips_dispatch_load_and_never_skips_dirty_recovery() {
        use hmac::{Hmac, Mac};
        use std::io::{BufRead, BufReader};
        let root = std::env::temp_dir().join(format!(
            "git-zero-witness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut host = NativeHost::open(root.clone()).unwrap();
        // Keep the real local StateStore/recovery path, using an authenticated
        // registration fixture only to install the private bridge capability.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let services = Arc::new(
            HttpServices::new(
                &format!("http://{}", listener.local_addr().unwrap()),
                "demo",
                "token",
            )
            .unwrap(),
        );
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /_services/register "));
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["generation_witness"],
                1
            );
            let response = serde_json::to_vec(
                &serde_json::json!({"registered": true, "generation_witness": {
                    "version": 1, "key": "0b".repeat(32), "session": "01".repeat(16)
                }}),
            )
            .unwrap();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                response.len()
            )
            .unwrap();
            reader.get_mut().write_all(&response).unwrap();
        });
        services.register("http://127.0.0.1:1234").unwrap();
        server.join().unwrap();
        host.cloudflare = Some(services);
        let mut request = Request {
            method: "GET".into(),
            target: "/repo.git/info/refs?service=git-receive-pack".into(),
            headers: BTreeMap::new(),
            body: vec![],
        };
        let message = serde_json::to_vec(&(
            "git-zero-generation-witness",
            1,
            "01".repeat(16),
            "demo",
            "GET",
            &request.target,
            0,
        ))
        .unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&[11; 32]).unwrap();
        mac.update(&message);
        let signature: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let proof = format!("1:{}:0:{signature}", "01".repeat(16));
        let state = root.join("services/manifest.json");
        fs::write(&state, b"corrupt authoritative state").unwrap();
        // Missing, forged and unsupported proofs must attempt the actual load.
        for header in [None, Some("forged"), Some("2:unsupported")] {
            request.headers.clear();
            if let Some(header) = header {
                request.headers.insert(
                    "x-git-zero-internal-generation-witness".into(),
                    header.into(),
                );
            }
            assert_eq!(host.handle(request.clone()).status, 503);
        }
        assert_eq!(host.metrics.generation_witness_fallbacks, 3);
        request
            .headers
            .insert("x-git-zero-internal-generation-witness".into(), proof);
        assert_eq!(host.handle(request.clone()).status, 200);
        assert_eq!(host.metrics.generation_witness_hits, 1);
        // A matching proof never authorizes reuse of a dirty view.
        host.view_dirty = true;
        assert_eq!(host.handle(request.clone()).status, 503);
        assert_eq!(host.metrics.generation_witness_hits, 1);
        let next = Manifest {
            generation: 1,
            ..Manifest::default()
        };
        fs::write(&state, serde_json::to_vec(&next).unwrap()).unwrap();
        assert_eq!(host.handle(request.clone()).status, 200);
        assert!(!host.view_dirty);
        assert_eq!(host.manifest.generation, 1);
        assert_eq!(host.metrics.generation_witness_hits, 1);
        assert_eq!(host.metrics.generation_witness_fallbacks, 4);
        // A queued old proof after publication uses a fresh authoritative load.
        fs::write(&state, b"corrupt again").unwrap();
        assert_eq!(host.handle(request).status, 503);
        assert_eq!(host.metrics.generation_witness_fallbacks, 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_snapshot_rebuild_stays_dirty_without_materializing_partial_packs() {
        let root = std::env::temp_dir().join(format!(
            "git-zero-rebuild-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut host = NativeHost::open(root.clone()).unwrap();
        let valid = host
            .repository
            .store_pack("valid".into(), b"pack", b"index", 0)
            .unwrap();
        let missing = StoredPack {
            id: "missing".into(),
            pack: vec![Chunk {
                key: format!("chunks/{}", hash(b"missing")),
                len: 7,
            }],
            pack_bytes: 7,
            ..StoredPack::default()
        };
        let manifest = Manifest {
            packs: vec![valid, missing],
            ..Manifest::default()
        };
        assert_eq!(
            host.rebuild_view(&manifest).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(host.view_dirty);
        assert!(!host.view.exists());
        // A same-length corrupt later pack also rejects the entire snapshot
        // before materializing the valid first pack or either index.
        host.repository
            .blobs
            .put(&manifest.packs[1].pack[0].key, b"missinh")
            .unwrap();
        assert!(
            host.rebuild_view(&manifest)
                .unwrap_err()
                .to_string()
                .contains("corrupt chunk")
        );
        assert!(host.view_dirty);
        assert!(!host.view.exists());
        host.rebuild_view(&Manifest::default()).unwrap();
        assert!(!host.view_dirty);
        fs::remove_dir_all(root).unwrap();
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
    fn fetch_haves_use_packet_boundaries_in_both_protocols() {
        let id = "a".repeat(40);
        let have = format!("have {id}\n");
        let packet = format!("{:04x}{have}", have.len() + 4);
        assert!(has_haves(format!("0000{packet}0000").as_bytes()));
        assert!(has_haves(format!("0001{packet}0002").as_bytes()));
        assert!(!has_haves(format!("have {id}\n").as_bytes()));
        assert!(!has_haves(
            format!("{:04x}x{have}", have.len() + 5).as_bytes()
        ));
        assert!(!has_haves(b"0009have "));
        assert!(!has_haves(&packet.as_bytes()[..packet.len() - 1]));
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
    fn cloud_chunk_policy_does_not_expand_compaction_budget() {
        let root =
            std::env::temp_dir().join(format!("git-zero-compaction-bound-{}", std::process::id()));
        let mut host = NativeHost::open(root.clone()).unwrap();
        let mut packs: Vec<_> = [8 * 1024 * 1024 + 1, 1, 2, 3]
            .into_iter()
            .map(|bytes| CandidatePack {
                stored: None,
                id: String::new(),
                level: 0,
                // No file may be opened for a job beyond the unchanged budget.
                pack: root.join("must-not-read.pack"),
                index: root.join("must-not-read.idx"),
                bytes,
                floor: 0,
            })
            .collect();
        host.compact(&root, &mut packs).unwrap();
        assert_eq!(packs.len(), 4);
        assert_eq!(host.metrics.compactions, 0);
        assert_eq!(host.metrics.compaction_input_bytes, 0);
        fs::remove_dir_all(root).unwrap();
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
                floor: 0,
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
