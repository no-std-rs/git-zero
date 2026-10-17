//! Rust-only paired smart HTTP benchmark, including optional local workerd services.
use git_zero_host::benchmark_cost::{
    INTERLEAVED_FLOW, INTERLEAVED_OBJECTIVE, INTERLEAVED_SEED, interleaved_schedule,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use std::{collections::HashSet, os::unix::fs::MetadataExt};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RUN_SUCCEEDED: AtomicBool = AtomicBool::new(false);
static PROGRESS: OnceLock<ProgressLog> = OnceLock::new();

struct ProgressLog {
    journal: PathBuf,
    incomplete: PathBuf,
    metadata: Value,
}

fn progress_event(event: Value) -> Result<()> {
    if let Some(progress) = PROGRESS.get() {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&progress.journal)?;
        serde_json::to_writer(&mut file, &event)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn preserve_incomplete_report(error: &dyn std::error::Error) -> Result<()> {
    let Some(progress) = PROGRESS.get() else {
        return Ok(());
    };
    let mut completed = BTreeMap::new();
    let mut operations = Vec::new();
    let mut failures = Vec::new();
    if progress.journal.is_file() {
        for line in BufReader::new(fs::File::open(&progress.journal)?).lines() {
            let event: Value = serde_json::from_str(&line?)?;
            match event["kind"].as_str() {
                Some("backend_complete") => {
                    let key = (event["backend"].to_string(), event["sample"].to_string());
                    completed.insert(key, event["rows"].clone());
                }
                Some("operation") => operations.push(event["row"].clone()),
                Some("operation_failed") => failures.push(event),
                _ => {}
            }
        }
    }
    let mut raw = Vec::new();
    for rows in completed.values() {
        raw.extend(rows.as_array().into_iter().flatten().cloned());
    }
    for mut row in operations {
        let key = (row["backend"].to_string(), row["sample"].to_string());
        if !completed.contains_key(&key) {
            row["backend_validation_complete"] = json!(false);
            raw.push(row);
        }
    }
    let report = json!({
        "schema_version":if progress.metadata["flow"]==INTERLEAVED_FLOW {4}else{3},"complete":false,"error":error.to_string(),
        "run":progress.metadata,"raw":raw,"failed_operations":failures,
        "journal":progress.journal,
        "limits":["Incomplete run: completed request timings are retained for diagnosis. Rows from an unfinished backend have not passed all subsequent content, storage and accounting checks. Failed service counter snapshots must not be used for cost estimates."]
    });
    fs::write(
        &progress.incomplete,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    eprintln!("Incomplete report: {}", progress.incomplete.display());
    eprintln!("Operation journal: {}", progress.journal.display());
    Ok(())
}
const REFERENCE_CONFIG: &[(&str, &str)] = &[
    ("receive.unpackLimit", "0"),
    ("transfer.unpackLimit", "0"),
    ("receive.autogc", "false"),
    ("gc.auto", "0"),
    ("receive.fsckObjects", "true"),
    ("core.fsync", "committed"),
    ("core.fsyncMethod", "fsync"),
    ("core.multiPackIndex", "true"),
    ("repack.writeBitmaps", "true"),
    ("pack.writeReverseIndex", "true"),
    ("pack.threads", "0"),
    ("pack.window", "10"),
    ("pack.depth", "50"),
    ("http.receivepack", "true"),
];

struct Temporary {
    root: PathBuf,
    keep: bool,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if self.keep || !RUN_SUCCEEDED.load(Ordering::Relaxed) {
            eprintln!("Fixtures retained: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
struct Host {
    child: Child,
    url: String,
    native_url: String,
    cloudflare_repo: Option<String>,
    cloudflare_services: Option<git_zero_host::cloudflare::HttpServices>,
}
struct CloudflareSettings {
    origin: String,
    token: String,
    view_cache: String,
    repo_prefix: String,
    run_id: String,
}
struct BackendOptions<'a> {
    binary: &'a Path,
    cloudflare: Option<&'a CloudflareSettings>,
    reference_bitmap_after_push: bool,
    monthly_runs: u64,
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(services) = &self.cloudflare_services {
            let _ = services.unregister();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    fn start(
        binary: &Path,
        store: &Path,
        reference: bool,
        log: &Path,
        cloudflare: Option<(&CloudflareSettings, &str)>,
    ) -> Result<Self> {
        let cloudflare_services = cloudflare
            .map(|(settings, repo)| {
                git_zero_host::cloudflare::HttpServices::new(
                    &settings.origin,
                    repo,
                    &settings.token,
                )
            })
            .transpose()?;
        let mut command = Command::new(binary);
        if reference {
            command.arg("--reference");
        } else if let Some((settings, repo)) = cloudflare {
            command
                .args(["--cloudflare", &settings.origin, "--repo", repo])
                .args([
                    "--token",
                    &settings.token,
                    "--view-cache",
                    &settings.view_cache,
                ]);
        }
        let child = command
            .arg(store)
            .arg("0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(log)?)
            .spawn()?;
        let mut host = Self {
            child,
            url: String::new(),
            native_url: String::new(),
            cloudflare_repo: cloudflare.map(|(_, repo)| repo.to_owned()),
            cloudflare_services,
        };
        let stdout = host.child.stdout.take().ok_or("missing server stdout")?;
        let (send, recv) = mpsc::channel();
        thread::spawn(move || {
            let mut sent = false;
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if !sent && line.starts_with("http://127.0.0.1:") => {
                        let _ = send.send(Ok(line));
                        sent = true;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = send.send(Err(error.to_string()));
                        return;
                    }
                }
            }
            if !sent {
                let _ = send.send(Err("server exited without a URL".into()));
            }
        });
        host.native_url = recv
            .recv_timeout(Duration::from_secs(30))?
            .map_err(|error| format!("{error}; inspect {}", log.display()))?;
        host.url = host.native_url.clone();
        if let Some((settings, repo)) = cloudflare {
            // The host registers before announcing readiness. Avoid a second
            // registry write; the runner only unregisters its own namespace.
            host.url = format!("{}/{repo}/repo.git", settings.origin.trim_end_matches('/'));
        }
        Ok(host)
    }
    fn metrics(&self) -> Option<Value> {
        self.json("/metrics")
    }
    fn object_stats(&self) -> Result<Value> {
        self.json("/_object-stats")
            .ok_or_else(|| "native object statistics endpoint unavailable".into())
    }
    fn json(&self, target: &str) -> Option<Value> {
        let authority = self.native_url.strip_prefix("http://")?.split('/').next()?;
        let mut socket = TcpStream::connect(authority).ok()?;
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        write!(
            socket,
            "GET {target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut response = String::new();
        socket
            .take(8 * 1024 * 1024)
            .read_to_string(&mut response)
            .ok()?;
        let (headers, body) = response.split_once("\r\n\r\n")?;
        if !headers.lines().next()?.contains(" 200 ") {
            return None;
        }
        serde_json::from_str(body).ok()
    }
}

struct GitResult {
    wall_ms: f64,
    stdout: Vec<u8>,
    resources: Value,
}
fn git(root: &Path, cwd: &Path, args: &[&str], measure: bool) -> Result<GitResult> {
    let timing = root.join(format!("time-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let use_time = measure && Path::new("/usr/bin/time").exists();
    let mut command = Command::new(if use_time { "/usr/bin/time" } else { "git" });
    if use_time {
        command
            .args(["-f", "%U %S %M", "-o"])
            .arg(&timing)
            .arg("git");
    }
    command
        .args([
            "-c",
            "protocol.version=0",
            "-c",
            "http.postBuffer=268435456",
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    let started = Instant::now();
    let output = command.output()?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let resources = if use_time {
        let text = fs::read_to_string(&timing)?;
        let usage: Vec<f64> = text
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        fs::remove_file(timing)?;
        json!({"client_cpu_user_ms":usage[0]*1000., "client_cpu_system_ms":usage[1]*1000., "client_max_rss_kib":usage[2]})
    } else {
        json!(null)
    };
    Ok(GitResult {
        wall_ms,
        stdout: output.stdout,
        resources,
    })
}
fn git_text(root: &Path, cwd: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git(root, cwd, args, false)?.stdout)?
        .trim()
        .to_owned())
}
#[derive(Default)]
struct FileFootprint {
    logical_bytes: u64,
    files: u64,
    #[cfg(unix)]
    inodes: HashSet<(u64, u64)>,
    #[cfg(unix)]
    unique_inode_file_bytes: u64,
    #[cfg(unix)]
    allocated_unique_inode_bytes: u64,
}
impl FileFootprint {
    fn add(&mut self, metadata: &fs::Metadata) {
        self.logical_bytes += metadata.len();
        self.files += 1;
        #[cfg(unix)]
        if self.inodes.insert((metadata.dev(), metadata.ino())) {
            self.unique_inode_file_bytes += metadata.len();
            self.allocated_unique_inode_bytes += metadata.blocks() * 512;
        }
    }
    fn json(&self) -> Value {
        let mut result = json!({"logical_bytes":self.logical_bytes,"files":self.files});
        #[cfg(unix)]
        {
            result["unique_inode_file_bytes"] = json!(self.unique_inode_file_bytes);
            result["allocated_unique_inode_bytes"] = json!(self.allocated_unique_inode_bytes);
        }
        result
    }
}
fn storage_footprint(directory: &Path, backend: &str) -> Result<Value> {
    let (mut all, mut authority, mut view) = (
        FileFootprint::default(),
        FileFootprint::default(),
        FileFootprint::default(),
    );
    let mut pending = vec![directory.to_owned()];
    let authority_directory = directory.join("services");
    let view_directory = directory.join("view");
    // One directory walk per snapshot; each file's metadata is read once.
    // The root inode set deduplicates hardlinks across services and native view.
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                pending.push(path);
                continue;
            }
            let metadata = entry.metadata()?;
            all.add(&metadata);
            if path.starts_with(&authority_directory) {
                authority.add(&metadata);
            } else if path.starts_with(&view_directory) {
                view.add(&metadata);
            }
        }
    }
    let mut result = all.json();
    if backend == "prototype" {
        let manifest: Value =
            serde_json::from_slice(&fs::read(directory.join("services/manifest.json"))?)?;
        let active_bytes: u64 = manifest["packs"]
            .as_array()
            .ok_or("invalid manifest packs")?
            .iter()
            .map(|pack| {
                pack["pack_bytes"].as_u64().unwrap_or(0) + pack["index_bytes"].as_u64().unwrap_or(0)
            })
            .sum();
        result["authoritative_services"] = authority.json();
        result["disposable_native_view"] = view.json();
        result["active_manifest_pack_and_index_bytes"] = json!(active_bytes);
        result["active_manifest_pack_count"] =
            json!(manifest["packs"].as_array().map_or(0, Vec::len));
        result["pending_retired_chunk_count"] =
            json!(manifest["garbage"].as_array().map_or(0, Vec::len));
    }
    Ok(result)
}
fn storage_snapshot(directory: &Path, backend: &str, host: &Host) -> Result<Value> {
    if host.cloudflare_repo.is_none() {
        return storage_footprint(directory, backend);
    }
    let stats = host.object_stats()?;
    let usage = &stats["cloudflare_services"];
    let manifest = &stats["manifest"];
    let packs = manifest["packs"]
        .as_array()
        .ok_or("cloud object statistics contain no manifest packs")?;
    let active_bytes: u64 = packs
        .iter()
        .map(|pack| {
            pack["pack_bytes"].as_u64().unwrap_or(0) + pack["index_bytes"].as_u64().unwrap_or(0)
        })
        .sum();
    let live_bytes = usage["r2_live_bytes"]
        .as_u64()
        .ok_or("cloud service statistics contain no actual R2 live-byte inventory")?;
    let live_objects = usage["r2_live_objects"]
        .as_u64()
        .ok_or("cloud service statistics contain no actual R2 object inventory")?;
    Ok(json!({
        "authoritative_services":{"logical_bytes":live_bytes,"objects":live_objects,
            "generation":manifest["generation"],"refs":manifest["refs"].as_object().map_or(0, serde_json::Map::len),
            "active_manifest_pack_and_index_bytes":active_bytes,"active_manifest_pack_count":packs.len()},
        "local_codec_cache":storage_footprint(directory, backend)?,
        "scope":"Authoritative inventory is actual local-workerd R2 objects; local codec filesystem is a disposable cache, not additional cloud storage."
    }))
}
fn content(file: usize, bytes: usize) -> Vec<u8> {
    let mut state = 0x9e3779b97f4a7c15u64 ^ (file as u64 + 1);
    (0..bytes)
        .map(|position| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if position % 1024 < 512 {
                b"fn item() { return 42; }\n"[position % 25]
            } else {
                state as u8
            }
        })
        .collect()
}
struct Fixture {
    source: PathBuf,
    base: String,
    tip: String,
    append_tips: Vec<String>,
    fetch_probes: Vec<FetchProbe>,
    metadata: Value,
}
struct FetchProbe {
    reference: String,
    tip: String,
    have: Option<String>,
}
fn fixture(root: &Path, mib: f64, pushes: usize, random_fetches: usize) -> Result<Fixture> {
    let source = root.join("source");
    git(
        root,
        root,
        &[
            "init",
            "--initial-branch=main",
            "--object-format=sha1",
            source.to_str().ok_or("non-UTF8 temp path")?,
        ],
        false,
    )?;
    git(root, &source, &["config", "user.name", "Benchmark"], false)?;
    git(
        root,
        &source,
        &["config", "user.email", "benchmark@example.invalid"],
        false,
    )?;
    fs::create_dir(source.join("data"))?;
    let files = ((mib * 16.).ceil() as usize).max(16);
    let bytes = (mib * 1024. * 1024.) as usize / files;
    for file in 0..files {
        fs::write(
            source.join(format!("data/{file}.dat")),
            content(file, bytes),
        )?;
    }
    git(root, &source, &["add", "."], false)?;
    git(
        root,
        &source,
        &["commit", "-m", "Deterministic base"],
        false,
    )?;
    let base = git_text(root, &source, &["rev-parse", "HEAD"])?;
    for file in 0..(files / 8).max(2) {
        let mut data = content(file, bytes);
        let change = format!("incremental change {file}\n");
        data[1024..1024 + change.len()].copy_from_slice(change.as_bytes());
        fs::write(source.join(format!("data/{file}.dat")), data)?;
    }
    git(
        root,
        &source,
        &["commit", "-am", "Deterministic incremental update"],
        false,
    )?;
    let tip = git_text(root, &source, &["rev-parse", "HEAD"])?;
    let mut append_tips = Vec::new();
    for generation in 0..pushes - 2 {
        let path = source.join(format!("data/{}.dat", generation % files));
        let mut data = fs::read(&path)?;
        let change = format!("compaction workload generation {generation}\n");
        data[2048..2048 + change.len()].copy_from_slice(change.as_bytes());
        fs::write(path, data)?;
        git(
            root,
            &source,
            &["commit", "-am", &format!("Append generation {generation}")],
            false,
        )?;
        append_tips.push(git_text(root, &source, &["rev-parse", "HEAD"])?);
    }
    let history: Vec<_> = std::iter::once(&base)
        .chain(std::iter::once(&tip))
        .chain(append_tips.iter())
        .collect();
    let final_tip = history.last().ok_or("missing history")?.to_string();
    for (index, historical_tip) in history.iter().enumerate() {
        // Local seed fetches must request advertised tips. These fixture-only
        // refs are never published to either measured server.
        git(
            root,
            &source,
            &[
                "update-ref",
                &format!("refs/heads/history-{index}"),
                historical_tip,
            ],
            false,
        )?;
    }
    let mut fetch_probes = Vec::new();
    for index in 0..random_fetches {
        // Distinct branch tips defeat exact-request caches; varied ancestors
        // exercise real reachability walks and thin-pack selection.
        let fork = (index * 7 + index / history.len()) % history.len();
        git(
            root,
            &source,
            &["checkout", "--quiet", history[fork]],
            false,
        )?;
        let path = source.join(format!("data/{}.dat", (index * 11) % files));
        let mut data = fs::read(&path)?;
        let change = format!("varied fetch probe {index} fork {fork}\n");
        data[3072..3072 + change.len()].copy_from_slice(change.as_bytes());
        fs::write(path, data)?;
        git(
            root,
            &source,
            &["commit", "-am", &format!("Fetch probe {index}")],
            false,
        )?;
        let probe_tip = git_text(root, &source, &["rev-parse", "HEAD"])?;
        let reference = format!("refs/heads/probe-{index}");
        git(
            root,
            &source,
            &["update-ref", &reference, &probe_tip],
            false,
        )?;
        let have = if index % 4 == 0 {
            None
        } else {
            Some(history[(index * 13) % (fork + 1)].to_string())
        };
        fetch_probes.push(FetchProbe {
            reference,
            tip: probe_tip,
            have,
        });
    }
    git(root, &source, &["checkout", "--quiet", "main"], false)?;
    if git_text(root, &source, &["rev-parse", "HEAD"])? != final_tip {
        return Err("fixture main moved while creating fetch probes".into());
    }
    git(root, &source, &["gc", "--prune=now"], false)?;
    let metadata = json!({"base":base,"tip":tip,"append_tips":append_tips,"files":files,"bytes_per_file":bytes,
        "fetch_probes":fetch_probes.iter().map(|probe|json!({"reference":probe.reference,"want":probe.tip,"have":probe.have})).collect::<Vec<_>>(),
        "working_tree_bytes":files*bytes,"objects":git_text(root,&source,&["count-objects","-v"])?});
    Ok(Fixture {
        source,
        base,
        tip,
        append_tips,
        fetch_probes,
        metadata,
    })
}
fn interleaved_fixture(root: &Path, mib: f64) -> Result<Fixture> {
    let source = root.join("source");
    git(
        root,
        root,
        &[
            "init",
            "--initial-branch=main",
            "--object-format=sha1",
            source.to_str().ok_or("non-UTF8 source")?,
        ],
        false,
    )?;
    git(root, &source, &["config", "user.name", "Benchmark"], false)?;
    git(
        root,
        &source,
        &["config", "user.email", "benchmark@example.invalid"],
        false,
    )?;
    fs::create_dir(source.join("data"))?;
    // Disable delta encoding for this file in both receiving implementations.
    // Independent random halves already require about 4 MiB of new compressed
    // bytes; the attribute also prevents a misleading tiny replacement pack.
    fs::write(source.join(".gitattributes"), "data/large.dat -delta\n")?;
    let large_bytes = 8 * 1024 * 1024;
    fs::write(source.join("data/large.dat"), content(10_001, large_bytes))?;
    let files = (mib as usize * 1024 * 1024 - large_bytes) / 65536;
    for file in 0..files {
        fs::write(
            source.join(format!("data/{file}.dat")),
            content(file, 65536),
        )?;
    }
    let mut generations = Vec::new();
    let mut tips = Vec::new();
    let mut large_content_seed = 10_001;
    for generation in 1..=32 {
        let replacement = generation >= 5 && generation % 4 == 1;
        if replacement {
            large_content_seed = 10_000 + generation;
            fs::write(
                source.join("data/large.dat"),
                content(large_content_seed, large_bytes),
            )?;
        } else if generation > 1 {
            let path = source.join(format!("data/{}.dat", generation % files));
            let mut bytes = fs::read(&path)?;
            let change = format!("interleaved generation {generation:02}\n");
            bytes[2048..2048 + change.len()].copy_from_slice(change.as_bytes());
            fs::write(path, bytes)?;
        }
        git(root, &source, &["add", "."], false)?;
        git(
            root,
            &source,
            &[
                "commit",
                "-m",
                &format!("Interleaved generation {generation:02}"),
            ],
            false,
        )?;
        let tip = git_text(root, &source, &["rev-parse", "HEAD"])?;
        let tree = git_text(root, &source, &["rev-parse", "HEAD^{tree}"])?;
        git(
            root,
            &source,
            &[
                "update-ref",
                &format!("refs/heads/history-{generation}"),
                &tip,
            ],
            false,
        )?;
        generations.push(json!({"generation":generation,"tip":tip,"tree":tree,
            "large_push":generation==1||replacement,"replacement_logical_bytes":if replacement {large_bytes}else{0},
            "large_content_seed":large_content_seed}));
        tips.push(tip);
    }
    git(root, &source, &["gc", "--prune=now"], false)?;
    let schedule = interleaved_schedule(&tips)?;
    let schedule_sha256 = git_zero_host::hash(&serde_json::to_vec(&schedule)?);
    let metadata = json!({"flow":INTERLEAVED_FLOW,"objective_identity":INTERLEAVED_OBJECTIVE,
        "fixture_version":"independent-large-halves-v1","seed":INTERLEAVED_SEED,
        "generations":generations,"schedule":schedule,"schedule_sha256":schedule_sha256,
        "working_tree_bytes":mib as usize*1024*1024,"files":files+1,"bytes_per_file":65536,
        "large_file_bytes":large_bytes,"large_file_delta_encoding":false,
        "replacement_generations":[5,9,13,17,21,25,29],"compressible_fraction":0.5,
        "objects":git_text(root,&source,&["count-objects","-v"])?});
    Ok(Fixture {
        source,
        base: tips[0].clone(),
        tip: tips[1].clone(),
        append_tips: tips[2..].to_vec(),
        fetch_probes: Vec::new(),
        metadata,
    })
}

fn init_fetch_client(root: &Path, client: &Path, source: &Path, have: Option<&str>) -> Result<()> {
    git(
        root,
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            client.to_str().ok_or("non-UTF8 client")?,
        ],
        false,
    )?;
    git(root, client, &["config", "fetch.unpackLimit", "0"], false)?;
    git(root, client, &["config", "gc.auto", "0"], false)?;
    if let Some(have) = have {
        git(
            root,
            client,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                source.to_str().ok_or("non-UTF8 source")?,
                &format!("{have}:refs/heads/seed"),
            ],
            false,
        )?;
    }
    Ok(())
}

fn response_cache(metrics: &Value) -> Value {
    json!({"resident_bytes":metrics["cache_bytes"],"budget_bytes":metrics["cache_budget_bytes"],
        "entries":metrics["cache_entries"],"entry_limit":metrics["cache_entry_limit"]})
}

fn check_response_cache(cache: &Value) -> Result<()> {
    if cache["budget_bytes"] != 32 * 1024 * 1024
        || cache["entry_limit"] != 256
        || cache["resident_bytes"]
            .as_u64()
            .is_none_or(|n| n > 32 * 1024 * 1024)
        || cache["entries"].as_u64().is_none_or(|n| n > 256)
    {
        return Err("response cache configuration changed or budget exceeded".into());
    }
    Ok(())
}

fn run_interleaved_backend(
    root: &Path,
    options: &BackendOptions<'_>,
    fixture: &Fixture,
    backend: &str,
    sample: i64,
) -> Result<Vec<Value>> {
    let contention_before = contention_snapshot();
    let directory = root.join(format!("{sample}-{backend}"));
    fs::create_dir(&directory)?;
    let store = directory.join(if backend == "git" {
        "repo.git"
    } else {
        "store"
    });
    if backend == "git" {
        git(
            root,
            root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                "--object-format=sha1",
                store.to_str().ok_or("non-UTF8 store")?,
            ],
            false,
        )?;
        for (key, value) in REFERENCE_CONFIG {
            git(root, &store, &["config", key, value], false)?;
        }
    }
    let cloudflare = options.cloudflare.filter(|_| backend != "git");
    let repo = cloudflare.map(|settings| {
        format!(
            "{}-{}-{}",
            settings.repo_prefix,
            settings.run_id,
            if sample < 0 {
                "warmup".to_owned()
            } else {
                sample.to_string()
            }
        )
    });
    let host = Host::start(
        options.binary,
        &store,
        backend == "git",
        &directory.join("host.log"),
        cloudflare.zip(repo.as_deref()),
    )?;
    let latest = directory.join("latest.git");
    init_fetch_client(root, &latest, &fixture.source, None)?;
    let mut rows = Vec::new();
    let mut previous_generation = 0;
    let mut final_storage = Value::Null;
    for operation in fixture.metadata["schedule"]
        .as_array()
        .ok_or("missing schedule")?
    {
        let key = operation["key"].as_str().ok_or("missing key")?;
        let want = operation["want"].as_str().ok_or("missing want")?;
        let generation = operation["generation"]
            .as_u64()
            .ok_or("missing generation")?;
        let group = operation["group"].as_str().ok_or("missing group")?;
        if operation["kind"] == "push" {
            let mut row = measure(
                root,
                &host,
                backend,
                sample,
                key,
                &fixture.source,
                &[
                    "push",
                    "--atomic",
                    "--quiet",
                    &host.url,
                    &format!("{want}:refs/heads/main"),
                    &format!("{want}:refs/heads/history-{generation}"),
                ],
            )?;
            row["operation"] = operation.clone();
            row["push_wire_wall_ms"] = row["wall_ms"].clone();
            let wire_ms = row["wall_ms"].as_f64().ok_or("missing push timing")?;
            let maintenance_row = if backend == "git" {
                Some(maintenance(root, &store, sample, key)?)
            } else {
                None
            };
            let maintenance_ms = maintenance_row
                .as_ref()
                .and_then(|r| r["wall_ms"].as_f64())
                .unwrap_or(0.);
            row["ready_wall_ms"] = json!(wire_ms + maintenance_ms);
            row["maintenance_wall_ms"] = json!(maintenance_ms);
            let metrics = host.metrics().ok_or("missing host metrics")?;
            if backend != "git" {
                let cache = response_cache(&metrics);
                check_response_cache(&cache)?;
                if cache["resident_bytes"] != 0 || cache["entries"] != 0 {
                    return Err("push failed to invalidate response cache".into());
                }
                row["server_cache_after_push"] = cache;
                let delta = &row["service_metrics_delta"];
                row["compaction_delta"] = json!({"count":delta["compactions"],"input_bytes":delta["compaction_input_bytes"],"output_bytes":delta["compaction_output_bytes"],"retired_chunk_deletes":delta["retired_chunk_deletes"]});
            }
            row["push_wire_bytes"] = row["service_metrics_delta"]["input_bytes"].clone();
            let minimum_wire = if generation == 1 {
                fixture.metadata["working_tree_bytes"]
                    .as_f64()
                    .ok_or("missing fixture size")?
                    * 0.4
            } else {
                3. * 1024. * 1024.
            };
            if group == "large_push"
                && row["push_wire_bytes"]
                    .as_f64()
                    .is_none_or(|bytes| bytes < minimum_wire)
            {
                return Err(format!("large push {generation} did not transfer its required {minimum_wire} new wire bytes").into());
            }
            final_storage = if host.cloudflare_repo.is_some() {
                let usage = &metrics["cloudflare_services"];
                let current=usage["manifest_generation"].as_u64().ok_or("missing actual manifest generation in service metrics; update the local Worker")?;
                if current <= previous_generation {
                    return Err("push did not advance manifest generation".into());
                }
                row["manifest_generation_before"] = json!(previous_generation);
                row["manifest_generation_after"] = json!(current);
                previous_generation = current;
                for key in ["r2_live_bytes", "r2_live_objects"] {
                    if usage[key].as_u64().is_none() {
                        return Err(format!("missing actual {key} inventory").into());
                    }
                }
                json!({"authoritative_services":{"logical_bytes":usage["r2_live_bytes"],"objects":usage["r2_live_objects"],"generation":current},"scope":"Actual R2 inventory and authoritative SQLite generation from the existing observer metrics RPC; full reachable-object inventory is verified once at completion."})
            } else {
                storage_snapshot(&store, backend, &host)?
            };
            row["storage_after_push"] = final_storage.clone();
            progress_event(json!({"kind":"operation_verified","row":row}))?;
            rows.push(row);
            if let Some(row) = maintenance_row {
                rows.push(row);
            }
        } else {
            let client = if group == "latest_fetch" {
                latest.clone()
            } else {
                directory.join(format!("{key}.git"))
            };
            if group != "latest_fetch" {
                init_fetch_client(root, &client, &fixture.source, operation["have"].as_str())?;
            }
            let packs_before = client_packs(&client)?;
            let mut row = measure(
                root,
                &host,
                backend,
                sample,
                key,
                &client,
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--keep",
                    &host.url,
                    &format!(
                        "{}:refs/heads/result",
                        operation["reference"].as_str().ok_or("missing ref")?
                    ),
                ],
            )?;
            row["operation"] = operation.clone();
            let received: u64 = client_packs(&client)?
                .iter()
                .filter(|(name, _)| !packs_before.contains_key(*name))
                .map(|(_, bytes)| bytes)
                .sum();
            if received == 0 {
                return Err(format!("{key} transferred no pack").into());
            }
            if git_text(root, &client, &["rev-parse", "refs/heads/result"])? != want {
                return Err(format!("{key} want mismatch").into());
            }
            let tree = git_text(root, &client, &["rev-parse", "refs/heads/result^{tree}"])?;
            let expected_tree = git_text(
                root,
                &fixture.source,
                &["rev-parse", &format!("{want}^{{tree}}")],
            )?;
            if tree != expected_tree {
                return Err(format!("{key} tree mismatch").into());
            }
            git(root, &client, &["fsck", "--strict"], false)?;
            git(root, &client, &["archive", "refs/heads/result"], false)?;
            row["verified_want"] = json!(want);
            row["verified_tree"] = json!(tree);
            row["strict_fsck"] = json!(true);
            row["archive_verified"] = json!(true);
            row["client_received_pack_bytes"] = json!(received);
            row["http_response_body_bytes"] = row["service_metrics_delta"]["output_bytes"].clone();
            if backend != "git" {
                let metrics = host.metrics().ok_or("missing metrics after fetch")?;
                let cache = response_cache(&metrics);
                check_response_cache(&cache)?;
                row["server_cache_after_fetch"] = cache;
                if row["service_metrics_delta"]["blob_puts"].as_f64() != Some(0.) {
                    return Err("fetch wrote authoritative blobs".into());
                }
            }
            if host.cloudflare_repo.is_some() {
                let before = &row["cloudflare_services_before"];
                let after = &row["cloudflare_services_after"];
                let delta = &row["cloudflare_workload_delta"];
                if before["manifest_generation"].as_u64() != Some(previous_generation)
                    || after["manifest_generation"].as_u64() != Some(previous_generation)
                    || before["r2_live_objects"] != after["r2_live_objects"]
                    || before["r2_live_bytes"] != after["r2_live_bytes"]
                    || delta["r2_class_a"] != 0
                    || delta["r2_deletes"] != 0
                    || delta["r2_written_bytes"] != 0
                    || delta["do_rows_written"] != 0
                {
                    return Err(format!("{key} changed authoritative storage").into());
                }
                if cloudflare.is_some_and(|settings| settings.view_cache == "cold")
                    && group == "latest_fetch"
                    && (delta["r2_class_b"].as_u64().is_none_or(|n| n == 0)
                        || row["service_metrics_delta"]["cloudflare_services"]["operations"]["r2_get"]["attempts"].as_f64().is_none_or(|n|n<=0.)
                        || delta["r2_read_bytes"].as_u64().is_none_or(|n| n == 0))
                {
                    return Err("cold latest fetch bypassed actual R2 reads".into());
                }
                row["read_authority_unchanged"] = json!(true);
                row["manifest_generation"] = json!(previous_generation);
            }
            progress_event(json!({"kind":"operation_verified","row":row}))?;
            rows.push(row);
            if group != "latest_fetch" {
                fs::remove_dir_all(&client)?;
            }
        }
    }
    let final_objects = if host.cloudflare_repo.is_some() {
        Some(host.object_stats()?)
    } else {
        None
    };
    let contention_after = contention_snapshot();
    for row in &mut rows {
        row["backend_validation_complete"] = json!(true);
        row["run_contention_before"] = contention_before.clone();
        row["run_contention_after"] = contention_after.clone();
        row["final_storage"] = final_storage.clone();
    }
    if let Some(first) = rows.first_mut() {
        if let Some(objects) = final_objects {
            let usage: git_zero_host::accounting::ServiceUsage =
                serde_json::from_value(objects["cloudflare_services"].clone())?;
            first["estimated_service_costs"] = serde_json::to_value(
                git_zero_host::cost::estimate_service_usage(&usage, options.monthly_runs)?,
            )?;
            first["final_object_stats"] = objects;
        }
    }
    progress_event(
        json!({"kind":"backend_complete","backend":backend,"sample":sample,"rows":rows}),
    )?;
    Ok(rows)
}

fn metrics_delta(before: &Option<Value>, after: &Option<Value>) -> Value {
    fn delta(before: &Value, after: &Value) -> Option<Value> {
        match after {
            Value::Number(value) => Some(json!(value.as_f64()? - before.as_f64().unwrap_or(0.))),
            Value::Object(values) => Some(Value::Object(
                values
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), delta(&before[key], value)?)))
                    .collect(),
            )),
            _ => None,
        }
    }
    match (before, after) {
        (Some(before), Some(after)) => delta(before, after).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
fn cloudflare_workload_delta(before: &Option<Value>, after: &Option<Value>) -> Result<Value> {
    let decode = |snapshot: &Option<Value>| -> Result<git_zero_host::accounting::ServiceUsage> {
        let usage = snapshot
            .as_ref()
            .and_then(|value| value.get("cloudflare_services"))
            .ok_or("missing actual Cloudflare service counters")?;
        Ok(serde_json::from_value(usage.clone())?)
    };
    let earlier = decode(before)?;
    let later = decode(after)?;
    let delta = later.checked_workload_delta(&earlier).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "{error}; earlier scope={:?} epoch={:?} counters={:?}; later scope={:?} epoch={:?} counters={:?}",
                earlier.counter_scope, earlier.counter_epoch, earlier.calls,
                later.counter_scope, later.counter_epoch, later.calls,
            ),
        )
    })?;
    Ok(serde_json::to_value(delta)?)
}
fn contention_snapshot() -> Value {
    json!({
        "timestamp_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|value|value.as_secs()),
        "loadavg":fs::read_to_string("/proc/loadavg").ok().map(|text|text.trim().to_owned()),
        "cpu_pressure":fs::read_to_string("/proc/pressure/cpu").ok().map(|text|text.trim().to_owned()),
    })
}
fn measure(
    root: &Path,
    host: &Host,
    backend: &str,
    sample: i64,
    workload: &str,
    cwd: &Path,
    args: &[&str],
) -> Result<Value> {
    let before = host.metrics();
    let result = git(root, cwd, args, true);
    let after = host.metrics();
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            progress_event(
                json!({"kind":"operation_failed","backend":backend,"sample":sample,
                "workload":workload,"cloudflare_repo":host.cloudflare_repo,
                "before":before,"after":after,"error":error.to_string()}),
            )?;
            return Err(error);
        }
    };
    let mut row = json!({"backend":backend,"sample":sample,"warmup":sample<0,"workload":workload,
        "wall_ms":result.wall_ms,"client_resources":result.resources,"service_metrics_delta":metrics_delta(&before,&after)});
    if let Some(repo) = &host.cloudflare_repo {
        row["cloudflare_repo"] = json!(repo);
        row["cloudflare_services_before"] = before
            .as_ref()
            .map(|value| &value["cloudflare_services"])
            .cloned()
            .unwrap_or(Value::Null);
        row["cloudflare_services_after"] = after
            .as_ref()
            .map(|value| &value["cloudflare_services"])
            .cloned()
            .unwrap_or(Value::Null);
        row["cloudflare_services_delta"] =
            row["service_metrics_delta"]["cloudflare_services"].clone();
        match cloudflare_workload_delta(&before, &after) {
            Ok(delta) => row["cloudflare_workload_delta"] = delta,
            Err(error) => {
                row["counter_integrity"] = json!(false);
                progress_event(
                    json!({"kind":"operation_failed","row":row,"error":error.to_string()}),
                )?;
                return Err(error);
            }
        }
        row["counter_integrity"] = json!(true);
    }
    progress_event(json!({"kind":"operation","row":row}))?;
    Ok(row)
}
fn maintenance(root: &Path, store: &Path, sample: i64, after: &str) -> Result<Value> {
    let result = git(
        root,
        store,
        &["repack", "-adb", "--write-bitmap-index"],
        true,
    )?;
    let row = json!({"backend":"git","sample":sample,"warmup":sample<0,
        "workload":format!("maintenance_after_{after}"),"wall_ms":result.wall_ms,"client_resources":result.resources});
    progress_event(json!({"kind":"operation","row":row}))?;
    Ok(row)
}
fn record_push_storage(rows: &mut [Value], store: &Path, backend: &str, host: &Host) -> Result<()> {
    rows.last_mut().ok_or("missing push result")?["storage_after_push"] =
        storage_snapshot(store, backend, host)?;
    Ok(())
}
fn client_packs(client: &Path) -> Result<BTreeMap<String, u64>> {
    fs::read_dir(client.join("objects/pack"))?
        .filter_map(|entry| match entry {
            Ok(entry) if entry.path().extension().is_some_and(|ext| ext == "pack") => {
                Some(entry.metadata().map(|metadata| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        metadata.len(),
                    )
                }))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}
fn varied_fetches(
    root: &Path,
    directory: &Path,
    host: &Host,
    fixture: &Fixture,
    backend: &str,
    sample: i64,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for (index, probe) in fixture.fetch_probes.iter().enumerate() {
        let client = directory.join(format!("fetch-probe-{index}.git"));
        git(
            root,
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                client.to_str().ok_or("non-UTF8 probe path")?,
            ],
            false,
        )?;
        git(root, &client, &["config", "fetch.unpackLimit", "1"], false)?;
        git(root, &client, &["config", "gc.auto", "0"], false)?;
        if let Some(have) = &probe.have {
            git(
                root,
                &client,
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    fixture.source.to_str().ok_or("non-UTF8 source")?,
                    &format!("{have}:refs/heads/seed"),
                ],
                false,
            )?;
        }
        let packs_before = client_packs(&client)?;
        let mut row = measure(
            root,
            host,
            backend,
            sample,
            "random_fetch",
            &client,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--keep",
                &host.url,
                &format!("{}:refs/heads/result", probe.reference),
            ],
        )?;
        let cache_metrics = host.metrics();
        let cache = cache_metrics.as_ref().map_or(Value::Null, |metrics| {
            json!({"resident_bytes":metrics["cache_bytes"],"budget_bytes":metrics["cache_budget_bytes"],
                "entries":metrics["cache_entries"],"entry_limit":metrics["cache_entry_limit"]})
        });
        if let (Some(bytes), Some(budget)) = (
            cache["resident_bytes"].as_u64(),
            cache["budget_bytes"].as_u64(),
        ) {
            if bytes > budget {
                return Err(format!(
                    "fetch probe {index} exceeded response cache budget: {bytes} > {budget}"
                )
                .into());
            }
        }
        if let (Some(entries), Some(limit)) =
            (cache["entries"].as_u64(), cache["entry_limit"].as_u64())
        {
            if entries > limit {
                return Err(format!(
                    "fetch probe {index} exceeded response cache entry limit: {entries} > {limit}"
                )
                .into());
            }
        }
        let received_pack_bytes: u64 = client_packs(&client)?
            .iter()
            .filter(|(name, _)| !packs_before.contains_key(*name))
            .map(|(_, bytes)| bytes)
            .sum();
        if received_pack_bytes == 0 {
            return Err(format!("fetch probe {index} transferred no pack").into());
        }
        if git_text(root, &client, &["rev-parse", "refs/heads/result"])? != probe.tip {
            return Err(format!("fetch probe {index} ref mismatch").into());
        }
        git(root, &client, &["fsck", "--strict"], false)?;
        if git_text(root, &client, &["rev-parse", "refs/heads/result^{tree}"])?
            != git_text(
                root,
                &fixture.source,
                &["rev-parse", &format!("{}^{{tree}}", probe.tip)],
            )?
        {
            return Err(format!("fetch probe {index} tree mismatch").into());
        }
        git(root, &client, &["archive", "refs/heads/result"], false)?;
        row["probe"] =
            json!({"index":index,"want":probe.tip,"have":probe.have,"reference":probe.reference});
        row["client_received_pack_bytes"] = json!(received_pack_bytes);
        row["http_response_body_bytes"] = row["service_metrics_delta"]["output_bytes"].clone();
        row["server_cache_after_fetch"] = cache;
        rows.push(row);
    }
    Ok(rows)
}
fn run_backend(
    root: &Path,
    options: &BackendOptions<'_>,
    fixture: &Fixture,
    backend: &str,
    sample: i64,
) -> Result<Vec<Value>> {
    let contention_before = contention_snapshot();
    let directory = root.join(format!("{sample}-{backend}"));
    fs::create_dir(&directory)?;
    let store = directory.join(if backend == "git" {
        "repo.git"
    } else {
        "store"
    });
    if backend == "git" {
        git(
            root,
            root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                "--object-format=sha1",
                store.to_str().ok_or("non-UTF8 store")?,
            ],
            false,
        )?;
        for (key, value) in REFERENCE_CONFIG {
            git(root, &store, &["config", key, value], false)?;
        }
    }
    let cloudflare = options.cloudflare.filter(|_| backend != "git");
    let cloudflare_repo = cloudflare.map(|settings| {
        format!(
            "{}-{}-{}",
            settings.repo_prefix,
            settings.run_id,
            if sample < 0 {
                "warmup".to_owned()
            } else {
                sample.to_string()
            }
        )
    });
    let host = Host::start(
        options.binary,
        &store,
        backend == "git",
        &directory.join("host.log"),
        cloudflare.zip(cloudflare_repo.as_deref()),
    )?;
    let clone = directory.join("clone");
    let mut rows = vec![measure(
        root,
        &host,
        backend,
        sample,
        "initial_push",
        &fixture.source,
        &[
            "push",
            &host.url,
            &format!("{}:refs/heads/main", fixture.base),
        ],
    )?];
    record_push_storage(&mut rows, &store, backend, &host)?;
    if backend == "git" && options.reference_bitmap_after_push {
        rows.push(maintenance(root, &store, sample, "initial_push")?);
    }
    rows.push(measure(
        root,
        &host,
        backend,
        sample,
        "fresh_clone",
        root,
        &[
            "clone",
            "--quiet",
            &host.url,
            clone.to_str().ok_or("non-UTF8 clone path")?,
        ],
    )?);
    if git_text(root, &clone, &["rev-parse", "HEAD"])? != fixture.base {
        return Err("initial clone ref mismatch".into());
    }
    git(root, &clone, &["fsck", "--strict"], false)?;
    let warm_clone = directory.join("clone-warm");
    rows.push(measure(
        root,
        &host,
        backend,
        sample,
        "warm_clone",
        root,
        &[
            "clone",
            "--quiet",
            &host.url,
            warm_clone.to_str().ok_or("non-UTF8 clone path")?,
        ],
    )?);
    if git_text(root, &warm_clone, &["rev-parse", "HEAD"])? != fixture.base {
        return Err("warm clone ref mismatch".into());
    }
    git(root, &warm_clone, &["fsck", "--strict"], false)?;
    rows.push(measure(
        root,
        &host,
        backend,
        sample,
        "incremental_push",
        &fixture.source,
        &[
            "push",
            &host.url,
            &format!("{}:refs/heads/main", fixture.tip),
        ],
    )?);
    record_push_storage(&mut rows, &store, backend, &host)?;
    if backend == "git" && options.reference_bitmap_after_push {
        rows.push(maintenance(root, &store, sample, "incremental_push")?);
    }
    rows.push(measure(
        root,
        &host,
        backend,
        sample,
        "incremental_fetch",
        &clone,
        &["fetch", "--quiet", "origin"],
    )?);
    if git_text(root, &clone, &["rev-parse", "refs/remotes/origin/main"])? != fixture.tip {
        return Err("fetch ref mismatch".into());
    }
    git(root, &clone, &["fsck", "--strict"], false)?;
    if git_text(root, &clone, &["rev-parse", "origin/main^{tree}"])?
        != git_text(
            root,
            &fixture.source,
            &["rev-parse", &format!("{}^{{tree}}", fixture.tip)],
        )?
    {
        return Err("fetched content mismatch".into());
    }
    git(root, &clone, &["archive", "origin/main"], false)?; // Inflate reachable file contents outside timings.
    for (index, tip) in fixture.append_tips.iter().enumerate() {
        let workload = format!("append_push_{}", index + 1);
        rows.push(measure(
            root,
            &host,
            backend,
            sample,
            &workload,
            &fixture.source,
            &["push", &host.url, &format!("{tip}:refs/heads/main")],
        )?);
        record_push_storage(&mut rows, &store, backend, &host)?;
        if backend == "git" && options.reference_bitmap_after_push {
            rows.push(maintenance(root, &store, sample, &workload)?);
        }
    }
    git(root, &clone, &["fetch", "--quiet", "origin"], false)?;
    if git_text(root, &clone, &["rev-parse", "origin/main"])?
        != *fixture.append_tips.last().unwrap_or(&fixture.tip)
    {
        return Err("post-compaction fetch ref mismatch".into());
    }
    git(root, &clone, &["fsck", "--strict"], false)?;
    git(root, &clone, &["archive", "origin/main"], false)?;
    let push_storage = storage_snapshot(&store, backend, &host)?;
    let push_metrics = host.metrics();
    let push_objects = if host.cloudflare_repo.is_some() {
        Some(host.object_stats()?)
    } else {
        None
    };
    if !fixture.fetch_probes.is_empty() {
        // Publish the read workload after all measured pushes. Probe population
        // and reference bitmap preparation are excluded from timed requests.
        let mut args = vec!["push".to_owned(), "--quiet".to_owned(), host.url.clone()];
        args.extend(
            fixture
                .fetch_probes
                .iter()
                .map(|probe| format!("{}:{}", probe.tip, probe.reference)),
        );
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        git(root, &fixture.source, &args, false)?;
        if backend == "git" {
            maintenance(root, &store, sample, "fetch_fixture_setup")?;
        }
    }
    let read_storage_before = storage_snapshot(&store, backend, &host)?;
    let read_metrics_before = host.metrics();
    rows.extend(varied_fetches(
        root, &directory, &host, fixture, backend, sample,
    )?);
    let read_storage_after = storage_snapshot(&store, backend, &host)?;
    let metrics = host.metrics();
    let read_metrics_delta = metrics_delta(&read_metrics_before, &metrics);
    let read_workload_delta = if host.cloudflare_repo.is_some() {
        cloudflare_workload_delta(&read_metrics_before, &metrics)?
    } else {
        Value::Null
    };
    let hits = read_metrics_delta["cache_hits"].as_f64().unwrap_or(0.);
    let misses = read_metrics_delta["cache_misses"].as_f64().unwrap_or(0.);
    let read_cache = json!({"hits":hits,"misses":misses,
        "hit_rate":if hits+misses>0. {Some(hits/(hits+misses))} else {None},
        "resident_bytes":metrics.as_ref().map(|value|&value["cache_bytes"]),
        "budget_bytes":metrics.as_ref().map(|value|&value["cache_budget_bytes"]),
        "entries":metrics.as_ref().map(|value|&value["cache_entries"]),
        "entry_limit":metrics.as_ref().map(|value|&value["cache_entry_limit"])});
    let unchanged_storage = if host.cloudflare_repo.is_some() {
        read_storage_before["authoritative_services"]
            == read_storage_after["authoritative_services"]
    } else {
        read_storage_before == read_storage_after
    };
    if !unchanged_storage {
        return Err(format!("{backend} random fetches changed server storage: before {read_storage_before}, after {read_storage_after}").into());
    }
    if backend != "git"
        && read_metrics_delta["blob_puts"]
            .as_f64()
            .is_some_and(|puts| puts != 0.)
    {
        return Err("random fetches wrote authoritative blobs".into());
    }
    if let Some(settings) = cloudflare {
        let operations = &read_metrics_delta["cloudflare_services"]["operations"];
        for operation in ["r2_put", "r2_delete"] {
            if operations[operation]["attempts"]
                .as_f64()
                .is_some_and(|attempts| attempts != 0.)
            {
                return Err(format!(
                    "random fetches performed authoritative {operation} operations"
                )
                .into());
            }
        }
        if settings.view_cache == "cold"
            && !fixture.fetch_probes.is_empty()
            && (!operations["r2_get"]["attempts"]
                .as_f64()
                .is_some_and(|attempts| attempts > 0.)
                || !read_workload_delta["r2_read_bytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes > 0))
        {
            return Err("cold cloud fetches performed no actual R2 GETs; native view bypass would invalidate service accounting".into());
        }
    }
    let final_objects = if host.cloudflare_repo.is_some() {
        Some(host.object_stats()?)
    } else {
        None
    };
    let cost_estimate = if let Some(stats) = &final_objects {
        let usage: git_zero_host::accounting::ServiceUsage =
            serde_json::from_value(stats["cloudflare_services"].clone())?;
        Some(serde_json::to_value(
            git_zero_host::cost::estimate_service_usage(&usage, options.monthly_runs)?,
        )?)
    } else {
        None
    };
    let contention_after = contention_snapshot();
    for row in &mut rows {
        row["backend_validation_complete"] = json!(true);
        row["run_contention_before"] = contention_before.clone();
        row["run_contention_after"] = contention_after.clone();
        row["push_sequence_storage"] = push_storage.clone();
        row["push_sequence_service_metrics"] = push_metrics.clone().unwrap_or(Value::Null);
        row["random_fetch_storage_before"] = read_storage_before.clone();
        row["random_fetch_storage_after"] = read_storage_after.clone();
        row["random_fetch_service_metrics_delta"] = read_metrics_delta.clone();
        if host.cloudflare_repo.is_some() {
            row["random_fetch_cloudflare_workload_delta"] = read_workload_delta.clone();
        }
        row["random_fetch_cache"] = read_cache.clone();
        row["final_storage"] = read_storage_after.clone();
        row["final_service_metrics"] = metrics.clone().unwrap_or(Value::Null);
    }
    if let Some(row) = rows.first_mut() {
        if let Some(objects) = push_objects {
            row["push_sequence_object_stats"] = objects;
        }
        if let Some(objects) = final_objects {
            row["final_object_stats"] = objects;
        }
        if let Some(cost) = cost_estimate {
            row["estimated_service_costs"] = cost;
        }
    }
    progress_event(
        json!({"kind":"backend_complete","backend":backend,"sample":sample,"rows":rows}),
    )?;
    Ok(rows)
}
fn summary(rows: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    for backend in ["prototype", "cloudflare-local", "git"] {
        for workload in [
            "large_push",
            "small_push",
            "large_push_ready",
            "small_push_ready",
            "latest_fetch",
            "history_no_haves",
            "history_with_haves",
            "latest_recheck",
            "reference_bitmap_maintenance",
            "initial_push",
            "fresh_clone",
            "warm_clone",
            "incremental_push",
            "incremental_fetch",
            "random_fetch",
            "random_fetch_without_haves",
            "random_fetch_with_haves",
            "append_push",
            "maintenance_after_initial_push",
            "maintenance_after_incremental_push",
            "maintenance_after_append_push",
        ] {
            let mut times: Vec<f64> = rows
                .iter()
                .filter(|row| {
                    let name = row["workload"].as_str().unwrap_or("");
                    let matches_workload = if workload == "reference_bitmap_maintenance" {
                        name.starts_with("maintenance_after_push_")
                    } else if [
                        "large_push",
                        "small_push",
                        "large_push_ready",
                        "small_push_ready",
                        "latest_fetch",
                        "history_no_haves",
                        "history_with_haves",
                        "latest_recheck",
                    ]
                    .contains(&workload)
                    {
                        row["operation"]["group"]
                            == workload.strip_suffix("_ready").unwrap_or(workload)
                    } else if workload == "random_fetch_without_haves" {
                        name == "random_fetch" && row["probe"]["have"].is_null()
                    } else if workload == "random_fetch_with_haves" {
                        name == "random_fetch" && !row["probe"]["have"].is_null()
                    } else if workload == "append_push"
                        || workload == "maintenance_after_append_push"
                    {
                        name.starts_with(&format!("{workload}_"))
                    } else {
                        name == workload
                    };
                    row["backend"] == backend && matches_workload && row["warmup"] == false
                })
                .filter_map(|row| {
                    row[if workload.ends_with("_ready") {
                        "ready_wall_ms"
                    } else {
                        "wall_ms"
                    }]
                    .as_f64()
                })
                .collect();
            if times.is_empty() {
                continue;
            }
            times.sort_by(f64::total_cmp);
            let middle = times.len() / 2;
            let median = if times.len() % 2 == 0 {
                (times[middle - 1] + times[middle]) / 2.
            } else {
                times[middle]
            };
            let p95 = times[((times.len() as f64 * 0.95).ceil() as usize - 1).min(times.len() - 1)];
            println!("{backend:10} {workload:36} median {median:9.2} ms  p95 {p95:9.2} ms");
            result.push(json!({"backend":backend,"workload":workload,"samples":times.len(),"median_ms":median,"p95_ms":p95,
                "min_ms":times[0],"max_ms":times[times.len()-1],"sum_ms":times.iter().sum::<f64>()}));
        }
    }
    result
}
fn main() -> Result<()> {
    let result = run();
    if let Err(error) = &result {
        let report = preserve_incomplete_report(error.as_ref());
        if let Err(report_error) = report {
            eprintln!("Could not write incomplete report: {report_error}");
        }
    }
    result
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let option = |name: &str, default: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    };
    let flow = option("--flow", "batch");
    let interleaved = flow == INTERLEAVED_FLOW;
    let stage = option("--stage", "full");
    let samples: usize = option(
        "--samples",
        if interleaved {
            if stage == "screen" { "2" } else { "5" }
        } else {
            "4"
        },
    )
    .parse()?;
    let mib: f64 = option("--fixture-mib", if interleaved { "16" } else { "4" }).parse()?;
    let pushes: usize = option("--pushes", if interleaved { "32" } else { "5" }).parse()?;
    let random_fetches: usize =
        option("--random-fetches", if interleaved { "56" } else { "16" }).parse()?;
    let reference_maintenance = option("--reference-maintenance", "bitmap");
    let cloudflare_endpoint = option("--cloudflare-endpoint", "");
    let cloudflare_token = option("--cloudflare-token", "git-zero-local-services");
    let view_cache = option("--view-cache", "cold");
    let cloudflare_repo_prefix = option("--cloudflare-repo-prefix", "bench");
    let monthly_runs: u64 = option("--monthly-runs", "1000").parse()?;
    if !["batch", INTERLEAVED_FLOW].contains(&flow.as_str())
        || (interleaved
            && (!matches!(mib, 16. | 64.)
                || pushes != 32
                || random_fetches != 56
                || reference_maintenance != "bitmap"
                || monthly_runs != 1000
                || !["full", "screen"].contains(&stage.as_str())
                || samples != if stage == "screen" { 2 } else { 5 }))
        || !(2..=100).contains(&samples)
        || !(0.25..=64.).contains(&mib)
        || !(2..=256).contains(&pushes)
        || random_fetches > 256
        || !["bitmap", "retained"].contains(&reference_maintenance.as_str())
        || !["cold", "warm"].contains(&view_cache.as_str())
        || !(1..=1_000_000_000).contains(&monthly_runs)
        || cloudflare_repo_prefix.is_empty()
        || cloudflare_repo_prefix.len() > 32
        || !cloudflare_repo_prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "use --samples 2..100 --fixture-mib 0.25..64 --pushes 2..256 --random-fetches 0..256 --reference-maintenance bitmap|retained; optional --cloudflare-endpoint LOOPBACK_ORIGIN --cloudflare-token TOKEN --view-cache cold|warm --cloudflare-repo-prefix NAME(1..32) --monthly-runs 1..1000000000"
                .into(),
        );
    }
    let output = PathBuf::from(option("--output", "repository-host-benchmark.json"));
    let binary = env::current_exe()?.with_file_name("repository-host");
    if !binary.is_file() {
        return Err("build both binaries: cargo build --release -p git-zero-host --bins".into());
    }
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let cloudflare = if cloudflare_endpoint.is_empty() {
        None
    } else {
        Some(CloudflareSettings {
            origin: cloudflare_endpoint.trim_end_matches('/').to_owned(),
            token: cloudflare_token,
            view_cache,
            repo_prefix: cloudflare_repo_prefix,
            run_id: format!("{}-{timestamp:x}", std::process::id()),
        })
    };
    let worker_source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("cloudflare/src/index.js");
    let worker_source = if cloudflare.is_some() {
        Some(json!({"path":worker_source_path,
            "sha256":git_zero_host::hash(&fs::read(&worker_source_path)?),
            "scope":"Local Worker source file; operator must ensure Wrangler has loaded this file before freezing the benchmark."}))
    } else {
        None
    };
    let temp = Temporary {
        root: env::temp_dir().join(format!(
            "git-zero-host-bench-{}-{timestamp}",
            std::process::id()
        )),
        keep: args.iter().any(|arg| arg == "--keep"),
    };
    fs::create_dir(&temp.root)?;
    let report_name = output
        .file_name()
        .ok_or("output has no filename")?
        .to_string_lossy();
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let progress = ProgressLog {
        journal: parent.join(format!("{report_name}.{timestamp}.progress.jsonl")),
        incomplete: parent.join(format!("{report_name}.{timestamp}.incomplete.json")),
        metadata: json!({"flow":flow,"stage":stage,"output":output,"fixtures":temp.root,"host_binary":binary,
            "host_binary_sha256":git_zero_host::hash(&fs::read(&binary)?),"worker_source":worker_source,
            "samples":samples,"fixture_mib":mib,"pushes":pushes,"random_fetches":random_fetches,
            "cloudflare":cloudflare.as_ref().map(|settings|json!({"endpoint":settings.origin,
                "view_cache":settings.view_cache,"repository_prefix":settings.repo_prefix,"run_id":settings.run_id})),
            "contention_at_start":contention_snapshot()}),
    };
    PROGRESS
        .set(progress)
        .map_err(|_| "progress already initialized")?;
    progress_event(json!({"kind":"run_start"}))?;
    let data = if interleaved {
        interleaved_fixture(&temp.root, mib)?
    } else {
        fixture(&temp.root, mib, pushes, random_fetches)?
    };
    let options = BackendOptions {
        binary: &binary,
        cloudflare: cloudflare.as_ref(),
        reference_bitmap_after_push: reference_maintenance == "bitmap",
        monthly_runs,
    };
    let prototype = if cloudflare.is_some() {
        "cloudflare-local"
    } else {
        "prototype"
    };
    let run_backend = if interleaved {
        run_interleaved_backend
    } else {
        run_backend
    };
    let mut rows = Vec::new();
    for backend in [prototype, "git"] {
        rows.extend(run_backend(&temp.root, &options, &data, backend, -1)?);
    }
    for sample in 0..samples {
        for backend in if sample % 2 == 0 {
            [prototype, "git"]
        } else {
            ["git", prototype]
        } {
            eprintln!("sample {}/{samples}: {backend}", sample + 1);
            rows.extend(run_backend(
                &temp.root,
                &options,
                &data,
                backend,
                sample as i64,
            )?);
        }
    }
    let summaries = summary(&rows);
    if let Some(source) = &worker_source {
        let current_hash = git_zero_host::hash(&fs::read(&worker_source_path)?);
        if source["sha256"] != current_hash {
            return Err(
                "Worker source changed during the benchmark; mixed-runtime results are invalid"
                    .into(),
            );
        }
    }
    let cloudflare_runs:Vec<_> = rows.iter().filter(|row|row.get("final_object_stats").is_some()).map(|row|json!({
        "sample":row["sample"],"warmup":row["warmup"],"repository":row["cloudflare_repo"],
        "push_sequence_object_stats":row["push_sequence_object_stats"],
        "final_object_stats":row["final_object_stats"],"estimated_service_costs":row["estimated_service_costs"]
    })).collect();
    let host_sha256 = git_zero_host::hash(&fs::read(&binary)?);
    let rustc_version = Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let cpu_info = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu = cpu_info.lines().find_map(|line| {
        line.strip_prefix("model name")
            .and_then(|value| value.split_once(':'))
            .map(|(_, value)| value.trim())
    });
    let mut report = json!({"schema_version":if interleaved {4}else{3},"complete":true,"timestamp_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "machine":{"os":env::consts::OS,"arch":env::consts::ARCH,"cpu":cpu,"logical_cpus":thread::available_parallelism()?.get(),
            "kernel":fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|text|text.trim().to_owned()),
        "memory":fs::read_to_string("/proc/meminfo").ok().and_then(|text|text.lines().next().map(str::to_owned)),
            "contention_at_report":contention_snapshot(),
            "git":git_text(&temp.root,&temp.root,&["--version"])?,"rustc":rustc_version},
        "config":{"samples":samples,"fixture_mib":mib,"host_binary":binary,"host_binary_sha256":host_sha256,"debug_assertions":cfg!(debug_assertions),
        "cloudflare_local":cloudflare.as_ref().map(|settings|json!({"endpoint":settings.origin,"worker_source":worker_source,
            "view_cache":settings.view_cache,"repository_prefix":settings.repo_prefix,"run_id":settings.run_id,
            "monthly_workload_repetitions":monthly_runs,
            "execution_model":"Local workerd Worker gateway and actual R2/Durable Object service calls; native Rust/Git codec runs separately on loopback. No deployed Worker codec or paid cloud resources.",
            "cache_model":if settings.view_cache=="cold" {"Rehydrate the native codec view from actual R2 GETs for cold misses"} else {"Retain native codec view across requests; filesystem reads are a declared local repository cache"},
            "cost_trace":"Observed service operations include fixture publication and integrity verification; observer-phase telemetry is excluded by the pricing model. Workload repetitions scale operations, while observed peak storage is held constant for 30 days."})),
        "protocol":0,"http_post_buffer":268435456,"reference":REFERENCE_CONFIG,"warmups_per_backend":1,
        "reference_push_maintenance":reference_maintenance,
        "reference_maintenance":if reference_maintenance == "bitmap" {"git repack -adb --write-bitmap-index after each push; separate timings"} else {"Retain accepted packs; no repack after measured pushes"},
        "reference_random_fetch_preparation":"git repack -adb --write-bitmap-index after untimed probe population in both reference maintenance modes",
            "order":"AB BA alternating paired samples; fresh repositories each run",
            "push_generations_per_run":pushes,"random_fetches_per_run":random_fetches,
            "cache_workloads":"fresh_clone cold response cache; warm_clone identical request; random_fetch distinct branch wants and deterministic varied ancestor haves",
            "random_fetch_setup":"Probe branches populated after timed pushes; Git bitmap repack and fresh bare-client init/local ancestor seeding excluded from timings. Both clients retain downloads with fetch --keep and fetch.unpackLimit=1; every timed request must transfer a nonempty pack.",
            "storage_checks":"Every push records storage; push_sequence_storage precedes probe population; random fetches assert identical before/after server logical bytes and file counts."},
    "fixture":data.metadata,"raw":rows,"summary":summaries,"cloudflare_runs":cloudflare_runs,"limits":[
        "Synthetic corpus, loopback, hot OS caches, one client, protocol v0; optional Cloudflare mode uses local workerd emulation, not paid cloud services.",
        "Tuned stock Git baseline; these measurements cannot establish superiority over every Git server.",
        "Prototype inline maintenance is inside push; any reference bitmap maintenance is separate. Retained mode performs none after measured pushes.",
        "GNU time reports Git client CPU and maximum RSS; it excludes the independent host process and server Git children.",
        "Server metrics are recorded only when /metrics is available; absent metrics are null. Logical storage excludes filesystem allocation overhead.",
        "Unix unique_inode_file_bytes counts each device/inode once, deduplicating native-view hardlinks. allocated_unique_inode_bytes sums st_blocks*512 once per inode. Directory metadata is excluded; filesystem compression and reflink/shared-extent physical allocation cannot be fully inferred from these counters.",
        "Random-fetch client_received_pack_bytes measures installed nonempty packs after thin-pack completion; HTTP response body bytes include protocol framing and advertisements.",
        "Random-fetch and append-push summaries pool request latencies across paired runs; raw records retain each case and generation. Probe population is outside receiving measurements.",
        "Per-trial load-average and CPU-pressure snapshots are outside timed requests and show surrounding contention; they do not capture every in-request scheduling spike.",
        "Completion durability follows the host and recorded Git fsync settings; no simulated power-loss test is performed.",
        "Both servers currently use native Git codec subprocesses; this local prototype does not demonstrate a deployable Worker codec.",
        "Cloudflare-local costs price actual emulator service operation counts with explicit traffic/storage assumptions. Local codec timings are not measured cloud CPU, latency, cold-start or full hosting bills."
        ,"An operation journal is written outside measured requests. Failures preserve it, an incomplete diagnostic report and local fixtures; successful runs remove the journal."
    ]});
    if interleaved {
        report["config"]["flow"] = json!(INTERLEAVED_FLOW);
        report["config"]["objective_identity"] = json!(INTERLEAVED_OBJECTIVE);
        report["config"]["stage"] = json!(stage);
        report["config"]["reference_random_fetch_preparation"] = json!(
            "No setup pushes; bitmap after each measured atomic push, before its latest fetch"
        );
        report["config"]["cache_workloads"] = json!(
            "Persistent latest client; distinct published historical refs; independently seeded latest recheck"
        );
        report["config"]["random_fetch_setup"] = json!(
            "All 56 timed fetches transfer nonempty packs; fetch.unpackLimit=0; fresh-client ancestor seeds are local and outside timers"
        );
        report["config"]["storage_checks"] = json!(
            "Every push records generation and cache invalidation; every read compares actual R2 inventory and complete counter deltas"
        );
        report["config"]["push_latency_guard"] =
            json!("wire push plus separately timed reference bitmap maintenance");
        report["config"]["response_cache_max_response_bytes"] = json!(16 * 1024 * 1024);
        report["limits"].as_array_mut().unwrap().retain(|v| {
            !v.as_str().is_some_and(|s| {
                s.contains("Random-fetch and append-push") || s.contains("Retained mode")
            })
        });
        report["limits"].as_array_mut().unwrap().push(json!("Interleaved v1: 32 atomic pushes + 56 timed reads; history only uses refs already published by measured pushes. Push guard includes readiness maintenance. No post-batch fixture pushes."));
    }
    fs::write(
        &output,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    RUN_SUCCEEDED.store(true, Ordering::Relaxed);
    if let Some(progress) = PROGRESS.get() {
        let _ = fs::remove_file(&progress.journal);
    }
    println!("Report: {}", output.display());
    Ok(())
}
