//! Diagnostic executor; frozen fixture/operation semantics, no benchmark edits.
use super::*;
use git_zero_host::accounting::{ServiceUsage, UsageCounters};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader},
    sync::atomic::{AtomicU64, Ordering},
};
const ORIGIN: &str = "http://127.0.0.1:8853";
const REPO: &str = "fetchdiag3";
const GATEWAY: &str = "http://127.0.0.1:8853/fetchdiag3/repo.git";
const WRANGLER: &str = "/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js";
const NATIVE_HASH: &str = "3ccf43bb5cca872294191fef57df9d4469ddd5dad41e9b5abf4cd6e270d1f060";
const RAW_CAP: u64 = 8 * 1024 * 1024 * 1024;
static COMMAND: AtomicU64 = AtomicU64::new(1);

fn stream_hash(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn executable(name: &str) -> Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p.canonicalize()?);
        }
    }
    Err(format!("missing executable {name}").into())
}
fn journal(root: &Path, value: &Value) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("operations.jsonl"))?;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    file.write_all(&bytes)
}
fn footprint(root: &Path) -> io::Result<u64> {
    let mut stack = vec![root.to_owned()];
    let mut bytes = 0u64;
    while let Some(p) = stack.pop() {
        for entry in fs::read_dir(p)? {
            let e = entry?;
            if e.file_type()?.is_dir() {
                stack.push(e.path());
            } else {
                bytes = bytes.saturating_add(e.metadata()?.len());
            }
        }
    }
    Ok(bytes)
}
fn capacity(root: &Path, cap: u64, min_memory: u64) -> Result<Value> {
    let mut value = resource_preflight(root)?;
    value["large_fixture_bytes"] = Value::Null;
    value["preflight_scope"] = json!("capacity only; fixture size is not measured here");
    let bytes = footprint(root)?;
    if bytes > cap
        || value["available_memory_bytes"]
            .as_u64()
            .is_none_or(|n| n < min_memory)
    {
        return Err("case capacity bound failed; stop".into());
    }
    value["case_logical_bytes"] = json!(bytes);
    value["case_byte_cap"] = json!(cap);
    Ok(value)
}
fn verify_sources() -> Result<Value> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let frozen = Path::new(IMMUTABLE).join("source/examples/repository-host");
    let mut files = Vec::new();
    for rel in [
        "src/native.rs",
        "src/http.rs",
        "src/diagnostics.rs",
        "src/cloudflare.rs",
        "src/socket_rpc.rs",
        "src/lib.rs",
        "src/bin/hosting-bench.rs",
        "src/benchmark_cost.rs",
        "Cargo.toml",
        "../../Cargo.lock",
        "cloudflare/wrangler.toml",
        "cloudflare/src/index.js",
        "cloudflare/src/catalog-pages.js",
        "cloudflare/src/first-read.js",
        "cloudflare/src/inline-catalog.js",
        "cloudflare/src/retirement-proof.js",
        "cloudflare/src/service-socket.js",
    ] {
        let path = source.join(rel);
        if fs::read(&path)? != fs::read(frozen.join(rel))? {
            return Err(format!("approved source changed: {rel}").into());
        }
        files.push(json!({"path":path.canonicalize()?,"sha256":stream_hash(&path)?}));
    }
    let native = std::env::current_exe()?.with_file_name("repository-host");
    if stream_hash(&native)? != NATIVE_HASH {
        return Err("Native ELF differs from approved stage1".into());
    }
    let mut binaries = Vec::new();
    for path in [
        native,
        std::env::current_exe()?,
        executable("git")?,
        executable("node")?,
        PathBuf::from(WRANGLER),
        Path::new(WRANGLER)
            .ancestors()
            .nth(3)
            .ok_or("Wrangler path")?
            .join("@cloudflare/workerd-linux-64/bin/workerd"),
    ] {
        binaries.push(json!({"path":path.canonicalize()?,"sha256":stream_hash(&path)?}));
    }
    for rel in [
        "src/bin/fetch-phase-supervisor.rs",
        "src/bin/fetch-phase-supervisor/replay.rs",
    ] {
        let path = source.join(rel);
        files.push(json!({"path":path,"sha256":stream_hash(&path)?}));
    }
    Ok(
        json!({"native_source":STAGE1,"native_sha256":NATIVE_HASH,"source_files":files,"actual_binaries":binaries,"cpu_ms":null}),
    )
}
fn audit_tree(root: &Path, name: &str) -> Result<Value> {
    let me = process(std::process::id() as i32)?;
    let mut all = Vec::new();
    for entry in fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if let Ok(p) = process(pid) {
            all.push(p);
        }
    }
    let mut owned = vec![me.pid];
    for _ in 0..=all.len() {
        let n = owned.len();
        for p in &all {
            if owned.contains(&p.parent) && !owned.contains(&p.pid) {
                owned.push(p.pid);
            }
        }
        if n == owned.len() {
            break;
        }
    }
    let mut rows = Vec::new();
    let mut hashes = BTreeMap::new();
    for p in all.iter().filter(|p| owned.contains(&p.pid)) {
        if p.group != me.group {
            return Err("case descendant escaped owned PGID".into());
        }
        let exe = fs::read_link(format!("/proc/{}/exe", p.pid)).ok();
        let digest = if let Some(ref path) = exe {
            if !hashes.contains_key(path) {
                hashes.insert(path.clone(), stream_hash(path).ok());
            }
            hashes[path].clone()
        } else {
            None
        };
        let mut row = process_row(p);
        row["exe"] = json!(exe);
        row["exe_sha256"] = json!(digest);
        rows.push(row);
    }
    let value = json!({"case_pgid":me.group,"all_descendants_inherit":true,"processes":rows});
    save(root, name, &value)?;
    Ok(value)
}
struct CommandOutput {
    prefix: Vec<u8>,
    bytes: u64,
    sha256: String,
}
fn pipe_capture(mut pipe: impl Read, path: PathBuf) -> io::Result<CommandOutput> {
    let mut file = File::create(path)?;
    let mut prefix = Vec::new();
    let mut total = 0;
    let mut hash = Sha256::new();
    let mut buf = [0; 8192];
    loop {
        let n = pipe.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        total += n as u64;
        let keep = n.min(OUTPUT_CAP - prefix.len());
        file.write_all(&buf[..keep])?;
        prefix.extend_from_slice(&buf[..keep]);
    }
    Ok(CommandOutput {
        prefix,
        bytes: total,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn command(root: &Path, label: &str, cmd: &mut Command) -> Result<CommandOutput> {
    let id = COMMAND.fetch_add(1, Ordering::Relaxed);
    let stem = format!("cmd-{id:04}-{label}");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()); // PGID intentionally inherited.
    let mut child = cmd.spawn()?;
    let p = process(child.id() as i32)?;
    let group = process(std::process::id() as i32)?.group;
    save(
        root,
        &format!("{stem}.started.json"),
        &json!({"process":process_row(&p),"complete":false,"after_counters":null}),
    )?;
    if p.group != group {
        return Err("client command escaped case PGID".into());
    }
    let out = child.stdout.take().ok_or("stdout")?;
    let err = child.stderr.take().ok_or("stderr")?;
    let start = Instant::now();
    let result = thread::scope(|scope| -> io::Result<_> {
        let stdout = scope.spawn(|| pipe_capture(out, root.join(format!("{stem}.stdout"))));
        let stderr = scope.spawn(|| pipe_capture(err, root.join(format!("{stem}.stderr"))));
        let status = child.wait()?;
        let out = stdout
            .join()
            .map_err(|_| io::Error::other("stdout drainer panic"))??;
        let err = stderr
            .join()
            .map_err(|_| io::Error::other("stderr drainer panic"))??;
        Ok((status, out, err))
    });
    let (status, out, err) = result?;
    save(
        root,
        &format!("{stem}.json"),
        &json!({"pid":p.pid,"pgid":p.group,"label":label,"exit_code":status.code(),"signal":status.signal(),"success":status.success(),"elapsed_ms":start.elapsed().as_secs_f64()*1000.,
 "stdout_bytes":out.bytes,"stdout_sha256":out.sha256,"stdout_prefix_bytes":out.prefix.len(),"stderr_bytes":err.bytes,"stderr_sha256":err.sha256,"stderr_prefix_bytes":err.prefix.len(),"streams_complete":true,"cpu_ms":null}),
    )?;
    if !status.success() {
        return Err(format!("{label} failed; actual status/stdout/stderr retained").into());
    }
    Ok(out)
}
fn git(root: &Path, label: &str, cwd: &Path, args: &[&str]) -> Result<CommandOutput> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args([
            "-c",
            "protocol.version=0",
            "-c",
            "http.postBuffer=268435456",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    command(root, label, &mut cmd)
}
fn text(root: &Path, label: &str, cwd: &Path, args: &[&str]) -> Result<String> {
    let out = git(root, label, cwd, args)?;
    if out.bytes > OUTPUT_CAP as u64 {
        return Err("text output cap".into());
    }
    Ok(String::from_utf8(out.prefix)?.trim().into())
}
// Exact frozen content() algorithm from hosting-bench.rs (including state steps
// on compressible bytes). Large fixture geometry/32 commits below are unchanged.
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
fn geometry(plan: &Value) -> Result<(usize, usize, usize, usize)> {
    let f = &plan["logical_fixture"];
    let working = f["working_tree_bytes"].as_u64().ok_or("fixture bytes")? as usize;
    let large = f["large_file_bytes"].as_u64().ok_or("large bytes")? as usize;
    let small = f["bytes_per_file"].as_u64().ok_or("file bytes")? as usize;
    let count = f["generations"].as_array().ok_or("generation list")?.len();
    let tiny = plan["tiny"] == true;
    if tiny {
        if working > 65536 || large > working || small < 4096 || count > 3 || count == 0 {
            return Err("tiny fixture exceeds authorization".into());
        }
    } else if !matches!(working, 16777216 | 67108864)
        || large != 8388608
        || small != 65536
        || count != 32
    {
        return Err("sealed large geometry differs".into());
    }
    if (working - large) % small != 0 || (working - large) / small == 0 {
        return Err("invalid fixture geometry".into());
    }
    Ok((working, large, small, count))
}
fn fixture(root: &Path, plan: &Value) -> Result<PathBuf> {
    let source = root.join("source");
    let (working, large, small, count) = geometry(plan)?;
    let files = (working - large) / small;
    git(
        root,
        "fixture-init",
        root,
        &[
            "init",
            "--initial-branch=main",
            "--object-format=sha1",
            source.to_str().ok_or("source path")?,
        ],
    )?;
    git(
        root,
        "fixture-name",
        &source,
        &["config", "user.name", "Benchmark"],
    )?;
    git(
        root,
        "fixture-email",
        &source,
        &["config", "user.email", "benchmark@example.invalid"],
    )?;
    fs::create_dir(source.join("data"))?;
    fs::write(source.join(".gitattributes"), "data/large.dat -delta\n")?;
    fs::write(source.join("data/large.dat"), content(10001, large))?;
    for file in 0..files {
        fs::write(
            source.join(format!("data/{file}.dat")),
            content(file, small),
        )?;
    }
    capacity(
        root.parent().ok_or("case prefix")?,
        plan["raw_byte_cap"].as_u64().unwrap_or(RAW_CAP),
        plan["minimum_memory_bytes"].as_u64().unwrap_or(RAW_CAP),
    )?;
    let expected = plan["logical_fixture"]["generations"]
        .as_array()
        .ok_or("expected generations")?;
    let mut got = Vec::new();
    for generation in 1..=count {
        let replacement = generation >= 5 && generation % 4 == 1;
        if replacement {
            fs::write(
                source.join("data/large.dat"),
                content(10000 + generation, large),
            )?;
        } else if generation > 1 {
            let path = source.join(format!("data/{}.dat", generation % files));
            let mut bytes = fs::read(&path)?;
            let change = format!("interleaved generation {generation:02}\n");
            bytes[2048..2048 + change.len()].copy_from_slice(change.as_bytes());
            fs::write(path, bytes)?;
        }
        git(root, "fixture-add", &source, &["add", "."])?;
        git(
            root,
            "fixture-commit",
            &source,
            &[
                "commit",
                "-m",
                &format!("Interleaved generation {generation:02}"),
            ],
        )?;
        let tip = text(root, "fixture-tip", &source, &["rev-parse", "HEAD"])?;
        let tree = text(root, "fixture-tree", &source, &["rev-parse", "HEAD^{tree}"])?;
        let pass =
            expected[generation - 1]["tip"] == tip && expected[generation - 1]["tree"] == tree;
        got.push(json!({"generation":generation,"tip":tip,"tree":tree,"expected_tip":expected[generation-1]["tip"],"expected_tree":expected[generation-1]["tree"],"pass":pass}));
        save(
            root,
            "fixture-verification.json",
            &json!({"verified_generations":got,"all_expected_verified":false,"stop_on_mismatch":true}),
        )?;
        if !pass {
            return Err("reconstructed want/tree mismatch; no relabel or replay".into());
        }
        capacity(
            root.parent().ok_or("case prefix")?,
            plan["raw_byte_cap"].as_u64().unwrap_or(RAW_CAP),
            plan["minimum_memory_bytes"].as_u64().unwrap_or(RAW_CAP),
        )?;
        git(
            root,
            "fixture-history-ref",
            &source,
            &[
                "update-ref",
                &format!("refs/heads/history-{generation}"),
                &tip,
            ],
        )?;
    }
    git(root, "fixture-gc", &source, &["gc", "--prune=now"])?;
    if count == 32 {
        let tips = expected
            .iter()
            .map(|g| g["tip"].as_str().ok_or("tip").map(String::from))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if json!(interleaved_schedule(&tips)?) != plan["logical_fixture"]["schedule"] {
            return Err("frozen schedule mismatch".into());
        }
    }
    save(
        root,
        "fixture-verification.json",
        &json!({"verified_generations":got,"all_expected_verified":true,"verified_before_runtime_start":true,"generation_count":count,"historical_large_fixture":plan["tiny"]!=true}),
    )?;
    Ok(source)
}
fn object_hash(kind: &str, bytes: &[u8]) -> [u8; 20] {
    let mut h = sha1::Sha1::new();
    h.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    h.finalize().into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn tiny_plan() -> Result<Value> {
    let mut files = BTreeMap::new();
    for file in 0..4 {
        files.insert(format!("{file}.dat"), content(file, 4096));
    }
    files.insert("large.dat".into(), content(10001, 16384));
    let mut generations = Vec::new();
    let mut previous = None;
    for g in 1..=3 {
        if g > 1 {
            let data = files
                .get_mut(&format!("{}.dat", g % 4))
                .ok_or("tiny file")?;
            let change = format!("interleaved generation {g:02}\n");
            data[2048..2048 + change.len()].copy_from_slice(change.as_bytes());
        }
        let mut tree = Vec::new();
        for (name, bytes) in &files {
            tree.extend(format!("100644 {name}\0").as_bytes());
            tree.extend(object_hash("blob", bytes));
        }
        let mut root = b"100644 .gitattributes\0".to_vec();
        root.extend(object_hash("blob", b"data/large.dat -delta\n"));
        root.extend(b"40000 data\0");
        root.extend(object_hash("tree", &tree));
        let tree = hex(&object_hash("tree", &root));
        let mut commit = format!("tree {tree}\n");
        if let Some(ref p) = previous {
            commit.push_str(&format!("parent {p}\n"));
        }
        commit.push_str(&format!("author Benchmark <benchmark@example.invalid> 1767225600 +0000\ncommitter Benchmark <benchmark@example.invalid> 1767225600 +0000\n\nInterleaved generation {g:02}\n"));
        let tip = hex(&object_hash("commit", commit.as_bytes()));
        previous = Some(tip.clone());
        generations.push(json!({"generation":g,"tip":tip,"tree":tree}));
    }
    let mut ops = Vec::new();
    for g in 1..=3 {
        let want = &generations[g - 1]["tip"];
        let have = if g > 1 {
            generations[g - 2]["tip"].clone()
        } else {
            Value::Null
        };
        ops.push(json!({"key":format!("push_{g:02}"),"kind":"push","group":if g==1{"large_push"}else{"small_push"},"generation":g,"want":want,"have":have,"reference":"refs/heads/main","history_reference":format!("refs/heads/history-{g}")}));
        ops.push(json!({"key":format!("latest_fetch_{g:02}"),"kind":"fetch","group":"latest_fetch","generation":g,"want":want,"have":have,"reference":"refs/heads/main","client":"persistent-latest"}));
    }
    for (key, target, have) in [
        ("tiny_history_no_haves_03", 1, None),
        ("tiny_history_with_haves_03", 2, Some(1)),
        ("tiny_latest_recheck_03", 3, Some(2)),
    ] {
        ops.push(json!({"key":key,"kind":"fetch","group":if have.is_none(){"history_no_haves"}else if target==3{"latest_recheck"}else{"history_with_haves"},"generation":3,"want":generations[target-1]["tip"],"have":have.map(|h|generations[h-1]["tip"].clone()),"reference":if target==3{"refs/heads/main".into()}else{format!("refs/heads/history-{target}")},"client":"fresh-seeded-outside-timer"}));
    }
    Ok(
        json!({"schema":1,"tiny":true,"profile":"tiny-cold-v0","target_key":"tiny_latest_recheck_03","diagnostic_native_source":STAGE1,"historical_large_wants_claimed":false,
 "expected_ids_method":"independent in-memory Git blob/tree/commit SHA1 before any Git execution","logical_fixture":{"working_tree_bytes":32768,"large_file_bytes":16384,"bytes_per_file":4096,"generations":generations,"schedule":ops},"schedule_prefix_including_target":ops,"view_cache":"cold","deadline_seconds":120,"cleanup_grace_seconds":5,"raw_byte_cap":16777216,"minimum_memory_bytes":134217728}),
    )
}
fn init_client(root: &Path, client: &Path, source: &Path, have: Option<&str>) -> Result<()> {
    git(
        root,
        "client-init",
        root,
        &[
            "init",
            "--quiet",
            "--bare",
            client.to_str().ok_or("client")?,
        ],
    )?;
    git(
        root,
        "client-unpack",
        client,
        &["config", "fetch.unpackLimit", "1"],
    )?;
    git(root, "client-gc", client, &["config", "gc.auto", "0"])?;
    if let Some(have) = have {
        git(
            root,
            "client-seed",
            client,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--keep",
                "--no-write-fetch-head",
                source.to_str().ok_or("source")?,
                &format!("{have}:refs/heads/seed"),
            ],
        )?;
    }
    Ok(())
}
fn packs(client: &Path) -> Result<BTreeMap<String, (u64, String)>> {
    let mut out = BTreeMap::new();
    for e in fs::read_dir(client.join("objects/pack"))? {
        let e = e?;
        if e.path().extension().is_some_and(|s| s == "pack") {
            out.insert(
                e.file_name().to_string_lossy().into(),
                (e.metadata()?.len(), stream_hash(&e.path())?),
            );
        }
    }
    Ok(out)
}

fn http(
    root: &Path,
    name: &str,
    port: u16,
    path: &str,
    body: Option<&[u8]>,
    service: bool,
) -> Result<Value> {
    let mut headers = Vec::new();
    let mut bytes = Vec::new();
    let mut status = None;
    let result = (|| -> Result<()> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        write!(
            stream,
            "{} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n",
            if body.is_some() { "POST" } else { "GET" }
        )?;
        if service {
            write!(
                stream,
                "x-hosting-token: git-zero-local-services\r\nx-hosting-repo: {REPO}\r\nx-hosting-phase: stats\r\n"
            )?;
        }
        if let Some(b) = body {
            write!(
                stream,
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                b.len()
            )?;
        }
        stream.write_all(b"\r\n")?;
        if let Some(b) = body {
            stream.write_all(b)?;
        }
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.by_ref().take(8193).read_line(&mut line)?;
        headers.extend(line.as_bytes());
        status = Some(
            line.split_whitespace()
                .nth(1)
                .ok_or("status")?
                .parse::<u16>()?,
        );
        let mut length = None;
        let mut chunked = false;
        loop {
            line.clear();
            reader.by_ref().take(8193).read_line(&mut line)?;
            headers.extend(line.as_bytes());
            if line.is_empty() || line.len() > 8192 || headers.len() > 65536 {
                return Err("HTTP header bounds".into());
            }
            if line == "\r\n" {
                break;
            }
            let (k, v) = line.split_once(':').ok_or("header")?;
            if k.eq_ignore_ascii_case("content-length") {
                length = Some(v.trim().parse::<usize>()?);
            }
            if k.eq_ignore_ascii_case("transfer-encoding") {
                chunked = v.trim().eq_ignore_ascii_case("chunked");
            }
        }
        const CAP: usize = 8 * 1024 * 1024;
        if chunked {
            loop {
                line.clear();
                reader.by_ref().take(129).read_line(&mut line)?;
                if line.len() > 128 {
                    return Err("chunk bounds".into());
                }
                let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)?;
                if n == 0 {
                    loop {
                        line.clear();
                        reader.by_ref().take(8193).read_line(&mut line)?;
                        if line == "\r\n" {
                            break;
                        }
                        if line.is_empty() || line.len() > 8192 {
                            return Err("trailer bounds".into());
                        }
                    }
                    break;
                }
                if n > CAP - bytes.len() {
                    return Err("body cap".into());
                }
                let before = bytes.len();
                reader.by_ref().take(n as u64).read_to_end(&mut bytes)?;
                if bytes.len() - before != n {
                    return Err("body premature EOF".into());
                }
                let mut end = [0; 2];
                reader.read_exact(&mut end)?;
                if end != *b"\r\n" {
                    return Err("chunk ending".into());
                }
            }
        } else if let Some(n) = length {
            if n > CAP {
                return Err("body cap".into());
            }
            reader.by_ref().take(n as u64).read_to_end(&mut bytes)?;
            if bytes.len() != n {
                return Err("body premature EOF".into());
            }
        } else {
            reader.take((CAP + 1) as u64).read_to_end(&mut bytes)?;
            if bytes.len() > CAP {
                bytes.truncate(CAP);
                return Err("body cap".into());
            }
        }
        Ok(())
    })();
    // Exact actual response/error prefix, headers and status precede all rejection.
    fs::write(root.join(format!("{name}.headers")), headers)?;
    fs::write(root.join(format!("{name}.body")), &bytes)?;
    save(
        root,
        &format!("{name}.http.json"),
        &json!({"status":status,"body_bytes":bytes.len(),"body_sha256":hash(&bytes),"complete":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string()),"after_counters_if_failed":null}),
    )?;
    result?;
    if !status.is_some_and(|s| (200..300).contains(&s)) {
        return Err(format!("{name} HTTP failure; actual response saved").into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn usage(value: &Value) -> Result<ServiceUsage> {
    let v: ServiceUsage = serde_json::from_value(value.clone())?;
    if v.accounting_complete != Some(true) {
        return Err("actual service receipt incomplete".into());
    }
    Ok(v)
}
fn metrics(root: &Path, name: &str) -> Result<Value> {
    let value = http(root, name, 8854, "/metrics", None, false)?;
    usage(&value["cloudflare_services"])?;
    Ok(value)
}
struct Runtime {
    child: Child,
    readers: Vec<JoinHandle<io::Result<CommandOutput>>>,
    label: &'static str,
    pid: i32,
    start: u64,
    stopped: bool,
}
impl Runtime {
    fn start(root: &Path, label: &'static str, cmd: &mut Command) -> Result<Self> {
        cmd.current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let p = process(child.id() as i32)?;
        if p.group != process(std::process::id() as i32)?.group {
            return Err("runtime PGID escape".into());
        }
        let out = child.stdout.take().ok_or("stdout")?;
        let err = child.stderr.take().ok_or("stderr")?;
        let o = root.join(format!("{label}.stdout"));
        let e = root.join(format!("{label}.stderr"));
        let readers = vec![
            thread::spawn(move || pipe_capture(out, o)),
            thread::spawn(move || pipe_capture(err, e)),
        ];
        save(root, &format!("{label}-process.json"), &process_row(&p))?;
        if label == "native" {
            save(
                root,
                "native-environment.json",
                &inherited_environment_receipt(p.pid)?,
            )?;
        }
        Ok(Self {
            child,
            readers,
            label,
            pid: p.pid,
            start: p.start,
            stopped: false,
        })
    }
    fn ready(&mut self, root: &Path, needle: &str) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.child.try_wait()?.is_some() {
                return Err(format!("{} exited before readiness", self.label).into());
            }
            let out =
                fs::read_to_string(root.join(format!("{}.stdout", self.label))).unwrap_or_default();
            let err =
                fs::read_to_string(root.join(format!("{}.stderr", self.label))).unwrap_or_default();
            if out.contains(needle) || err.contains(needle) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(30) {
                return Err("runtime readiness bound".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    fn stop(&mut self, root: &Path) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        if let Ok(current) = process(self.pid) {
            if current.start != self.start {
                return Err("runtime PID generation changed".into());
            }
        }
        // Only this still-owned Child PID. Helper inherits the case PGID and is itself
        // bounded by the outside watchdog; no new unsafe positive-PID signaling API.
        command(
            root,
            "ordinary-runtime-term",
            Command::new("kill").args(["-TERM", "--", &self.pid.to_string()]),
        )?;
        let status = self.child.wait()?;
        let mut streams = Vec::new();
        for reader in self.readers.drain(..) {
            let out = reader.join().map_err(|_| "runtime drainer panic")??;
            streams.push(json!({"bytes":out.bytes,"sha256":out.sha256,"prefix_bytes":out.prefix.len(),"complete":true}));
        }
        self.stopped = true;
        save(
            root,
            &format!("{}-ordinary-cleanup.json", self.label),
            &json!({"pid":self.pid,"leader_reaped":true,"status":status.code(),"signal":status.signal(),"streams":streams}),
        )?;
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn close_scopes(final_usage: &Value) -> Result<Value> {
    let all = usage(final_usage)?;
    let workload = all.workload_counters()?;
    let mut comparisons = Vec::new();
    for field in [
        "worker_requests",
        "worker_gateway_requests",
        "worker_service_requests",
        "worker_observer_requests",
        "r2_class_a",
        "r2_class_b",
        "r2_deletes",
        "r2_read_bytes",
        "r2_written_bytes",
        "do_requests",
        "do_rows_read",
        "do_rows_written",
    ] {
        let total = final_usage[field].as_u64().ok_or("scope counter unknown")?;
        let mut sum = 0;
        for phase in final_usage["by_phase"]
            .as_object()
            .ok_or("phases")?
            .values()
        {
            sum += phase[field].as_u64().ok_or("phase counter unknown")?;
        }
        if total != sum {
            return Err(format!("phase closure failed {field}").into());
        }
        comparisons.push(json!({"counter":field,"total":total,"phase_sum":sum}));
    }
    Ok(
        json!({"complete":true,"counter_scope":all.counter_scope,"counter_epoch":all.counter_epoch,"startup":all.by_phase.get("startup"),"ordinary_workload_including_startup_excluding_stats":workload,"observer_and_cleanup_stats":all.by_phase.get("stats"),"phase_checks":comparisons,"all_actual_counters":all,"native_cpu_ms":null,"worker_cpu_ms":null,"do_gb_seconds":null,"full_bill_usd":null}),
    )
}
fn operation(root: &Path, source: &Path, latest: &Path, op: &Value, plan: &Value) -> Result<Value> {
    let key = op["key"].as_str().ok_or("key")?;
    let want = op["want"].as_str().ok_or("want")?;
    let group = op["group"].as_str().ok_or("group")?;
    let push = op["kind"] == "push";
    let client = if push {
        source.to_owned()
    } else if group == "latest_fetch" {
        latest.to_owned()
    } else {
        let p = root.join(format!("{key}.git"));
        init_client(root, &p, source, op["have"].as_str())?;
        p
    };
    let before_packs = if push {
        BTreeMap::new()
    } else {
        packs(&client)?
    };
    let before = metrics(root, &format!("{key}-before"))?;
    if key == "push_01" {
        save(root, "startup-scope.json", &before)?;
    }
    journal(
        root,
        &json!({"kind":"operation_started","operation":op,"before":before,"after":null,"complete":false}),
    )?;
    let start = Instant::now();
    let wire = if push {
        git(
            root,
            key,
            &client,
            &[
                "push",
                "--atomic",
                "--quiet",
                GATEWAY,
                &format!("{want}:refs/heads/main"),
                &format!(
                    "{want}:refs/heads/history-{}",
                    op["generation"].as_u64().ok_or("generation")?
                ),
            ],
        )
    } else {
        git(
            root,
            key,
            &client,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--keep",
                GATEWAY,
                &format!(
                    "{}:refs/heads/result",
                    op["reference"].as_str().ok_or("reference")?
                ),
            ],
        )
    };
    let wall_ms = start.elapsed().as_secs_f64() * 1000.;
    // Original measure() always obtains after-observer even when Git fails.
    let after = metrics(root, &format!("{key}-after"))?;
    if let Err(error) = wire {
        journal(
            root,
            &json!({"kind":"operation_failed","operation":op,"before":before,"after":after,"error":error.to_string(),"complete":false}),
        )?;
        return Err(error);
    }
    let earlier = usage(&before["cloudflare_services"])?;
    let later = usage(&after["cloudflare_services"])?;
    let delta: UsageCounters = later.checked_workload_delta(&earlier)?;
    let mut row = json!({"operation":op,"wall_ms":wall_ms,"before":before,"after":after,"workload_delta":delta,"complete":false,"native_cpu_ms":null,"child_cpu_ms":null});
    if push {
        let checked = metrics(root, &format!("{key}-post-verify"))?;
        if checked["cache_bytes"] != 0 || checked["cache_entries"] != 0 {
            return Err("push cache invalidation failed".into());
        }
        if checked["cloudflare_services"]["manifest_generation"].as_u64()
            != op["generation"].as_u64()
        {
            return Err("manifest generation mismatch".into());
        }
        row["post_check"] = checked;
    } else {
        let after_packs = packs(&client)?;
        let new = after_packs
            .iter()
            .filter(|(name, _)| !before_packs.contains_key(*name))
            .map(|(name, (len, sha))| json!({"name":name,"bytes":len,"sha256":sha}))
            .collect::<Vec<_>>();
        if new.is_empty() {
            return Err("fetch installed no nonempty pack".into());
        }
        if text(
            root,
            "verify-want",
            &client,
            &["rev-parse", "refs/heads/result"],
        )? != want
        {
            return Err("fetch want mismatch".into());
        }
        let tree = text(
            root,
            "verify-tree",
            &client,
            &["rev-parse", "refs/heads/result^{tree}"],
        )?;
        let expected = text(
            root,
            "source-tree",
            source,
            &["rev-parse", &format!("{want}^{{tree}}")],
        )?;
        if tree != expected {
            return Err("fetch tree mismatch".into());
        }
        git(root, "strict-fsck", &client, &["fsck", "--strict"])?;
        let archive = git(root, "archive", &client, &["archive", "refs/heads/result"])?;
        // Frozen benchmark drains archive for every fetch. Independent archive hash
        // comparison is after the timed command and adds no gateway/storage traffic.
        let expected_archive = git(root, "source-archive", source, &["archive", want])?;
        if archive.sha256 != expected_archive.sha256 || archive.bytes != expected_archive.bytes {
            return Err("archive mismatch".into());
        }
        let checked = metrics(root, &format!("{key}-post-verify"))?;
        if earlier.r2_live_bytes != later.r2_live_bytes
            || earlier.r2_live_objects != later.r2_live_objects
            || delta.r2_class_a != 0
            || delta.r2_deletes != 0
            || delta.r2_written_bytes != 0
            || delta.do_rows_written != Some(0)
        {
            return Err("fetch mutated authority".into());
        }
        if checked["cache_retained_bytes"]
            .as_u64()
            .is_none_or(|n| n > 32 * 1024 * 1024)
        {
            return Err("cache budget mismatch".into());
        }
        row["new_packs"] = json!(new);
        row["verified_want"] = json!(want);
        row["verified_tree"] = json!(tree);
        row["archive_sha256"] = json!(archive.sha256);
        row["strict_fsck"] = json!(true);
        row["post_check"] = checked;
    }
    row["complete"] = json!(true);
    journal(root, &json!({"kind":"operation_verified","row":row}))?;
    if !push && group != "latest_fetch" {
        fs::remove_dir_all(client)?;
    }
    capacity(
        root.parent().ok_or("case prefix")?,
        plan["raw_byte_cap"].as_u64().unwrap_or(RAW_CAP),
        plan["minimum_memory_bytes"].as_u64().unwrap_or(RAW_CAP),
    )?;
    Ok(row)
}
fn requests(root: &Path) -> Result<Value> {
    let bytes = fs::read(root.join("native-phase.jsonl"))?;
    let mut rows = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|b| !b.is_empty()) {
        let row: Value = serde_json::from_slice(line)?;
        if row.get("ledger_truncated").is_some() {
            return Err("Native phase ledger incomplete/truncated".into());
        }
        if matches!(row["kind"].as_str(), Some("http_request" | "native_read")) {
            rows.push(json!({"request":row["request"],"kind":row["kind"],"fields":row["fields"],"outcome":row["outcome"]}));
        }
    }
    Ok(json!(rows))
}
fn execute_case(root: &Path, plan: &Value) -> Result<Value> {
    let cap = plan["raw_byte_cap"].as_u64().unwrap_or(RAW_CAP);
    let minimum = plan["minimum_memory_bytes"].as_u64().unwrap_or(RAW_CAP);
    save(
        root,
        "resources-before.json",
        &capacity(root, cap, minimum)?,
    )?;
    save(root, "case-input.json", plan)?;
    let provenance = verify_sources()?;
    save(root, "source-runtime-before.json", &provenance)?;
    git(root, "actual-git-version", root, &["--version"])?;
    command(
        root,
        "actual-node-version",
        Command::new("node").arg("--version"),
    )?;
    command(
        root,
        "actual-wrangler-version",
        Command::new("node")
            .arg(WRANGLER)
            .arg("--version")
            .env("WRANGLER_SEND_METRICS", "false")
            .env("WRANGLER_LOG_PATH", root.join("wrangler-logs")),
    )?;
    let source = fixture(root, plan)?;
    let worker = Path::new(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let config = fs::read_to_string(worker.join("wrangler.toml"))?.replace(
        "main = \"src/index.js\"",
        &format!("main = {:?}", worker.join("src/index.js")),
    );
    fs::write(root.join("wrangler.toml"), config)?;
    let mut wrangler = Runtime::start(
        root,
        "wrangler",
        Command::new("node")
            .arg(WRANGLER)
            .args(["dev", "--config"])
            .arg(root.join("wrangler.toml"))
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                "8853",
                "--inspector-port",
                "0",
                "--local",
                "--persist-to",
            ])
            .arg(root.join("persist"))
            .arg("--show-interactive-dev-session=false")
            .env("WRANGLER_SEND_METRICS", "false")
            .env("X_LOCAL_EXPLORER", "false")
            .env("X_LOCAL_OBSERVABILITY", "false")
            .env("BROWSER", "none")
            .env("WRANGLER_LOG_PATH", root.join("wrangler-logs")),
    )?;
    wrangler.ready(root, "Ready on http://127.0.0.1:8853")?;
    let view = plan["view_cache"]
        .as_str()
        .or_else(|| plan["original_config"]["cloudflare_local"]["view_cache"].as_str())
        .ok_or("view policy")?;
    let mut native = Runtime::start(
        root,
        "native",
        Command::new(std::env::current_exe()?.with_file_name("repository-host"))
            .args(["--cloudflare", ORIGIN, "--repo", REPO, "--view-cache", view])
            .arg(root.join("native-state"))
            .arg("8854")
            .env("GIT_ZERO_PHASE_LEDGER", root.join("native-phase.jsonl")),
    )?;
    native.ready(root, "http://127.0.0.1:8854/repo.git")?;
    let env = json_from(root.join("native-environment.json"))?;
    if env["identity_environment_eligible"] != true {
        return Err("Native environment identity is unsupported".into());
    }
    audit_tree(root, "runtime-live-before.json")?;
    let latest = root.join("latest.git");
    init_client(root, &latest, &source, None)?;
    let mut rows = Vec::new();
    for op in plan["schedule_prefix_including_target"]
        .as_array()
        .ok_or("schedule prefix")?
    {
        rows.push(operation(root, &source, &latest, op, plan)?);
    }
    // No post-target object inventory is inserted into any prefix. This completion
    // inventory is the same final verification boundary used by frozen benchmark.
    let objects = http(
        root,
        "final-object-stats",
        8854,
        "/_object-stats",
        None,
        false,
    )?;
    usage(&objects["cloudflare_services"])?;
    audit_tree(root, "runtime-live-after.json")?;
    let after = verify_sources()?;
    save(root, "source-runtime-after.json", &after)?;
    if provenance != after {
        return Err("runtime/source changed during case".into());
    }
    let unregister = http(
        root,
        "ordinary-unregister",
        8853,
        "/_services/unregister",
        Some(b"{}"),
        true,
    )?;
    if unregister["registered"] != false {
        return Err("unregister failed".into());
    }
    let final_usage = http(
        root,
        "final-service-usage",
        8853,
        "/_services/metrics",
        None,
        true,
    )?;
    let closure = close_scopes(&final_usage)?;
    save(root, "scope-closure.json", &closure)?;
    native.stop(root)?;
    wrangler.stop(root)?;
    let request_hashes = requests(root)?;
    save(root, "actual-request-hashes.json", &request_hashes)?;
    save(root, "resources-after.json", &capacity(root, cap, minimum)?)?;
    Ok(
        json!({"pass":true,"tiny":plan["tiny"]==true,"fixture_reconstruction_verified":true,"operations":rows.len(),"target_key":plan["target_key"],"paid_scope_closure_complete":true,"final_native_metrics":objects,"native_environment":env,"case_pgid":process(std::process::id()as i32)?.group,"provider_source":STAGE1,"historical_provider_source_reused":false,"actual_request_hash_records":request_hashes.as_array().map_or(0,Vec::len),"performance_claim":false,"native_cpu_ms":null,"worker_cpu_ms":null,"do_gb_seconds":null,"full_bill_usd":null}),
    )
}
pub(super) fn worker(root: &Path) -> Result<()> {
    let plan = json_from(root.parent().ok_or("case parent")?.join("active-plan.json"))?;
    if plan["tiny"] != true && plan["root_runtime_go"] != true {
        return Err("large fixture generation requires separate root runtime GO".into());
    }
    let result = execute_case(root, &plan);
    match result {
        Ok(value) => {
            save(root, "case-result.json", &value)?;
            Ok(())
        }
        Err(error) => {
            save(
                root,
                "case-result.json",
                &json!({"pass":false,"error":error.to_string(),"complete":false,"after_counters":null,"counter_receipts_complete":false,"cpu_ms":null}),
            )?;
            Err(error)
        }
    }
}
pub(super) fn tiny(root: &Path) -> Result<()> {
    fs::create_dir(root)?;
    let root = root.canonicalize()?;
    resource_preflight(&root)?;
    let ports = [
        TcpListener::bind(("127.0.0.1", 8853))?,
        TcpListener::bind(("127.0.0.1", 8854))?,
    ];
    drop(ports);
    let plan = tiny_plan()?;
    save(&root, "active-plan.json", &plan)?;
    save(
        &root,
        "tiny-plan-seal.json",
        &json!({"sha256":hash(&serde_json::to_vec_pretty(&plan)?),"historical_large_wants_claimed":false,"generated_by_git_before_case":false}),
    )?;
    linux::subreaper()?;
    let supervision = supervise(
        &root.join("case"),
        "executor",
        Duration::from_secs(120),
        Duration::from_secs(5),
    )?;
    let result = json_from(root.join("case/case-result.json"))?;
    if supervision["deadline_expired"] != false
        || supervision["leader_exit_code"] != 0
        || result["pass"] != true
    {
        return Err("tiny production execution failed; preserved case artifacts".into());
    }
    let ports = [
        TcpListener::bind(("127.0.0.1", 8853))?,
        TcpListener::bind(("127.0.0.1", 8854))?,
    ];
    drop(ports);
    save(
        &root,
        "proof.json",
        &json!({"pass":true,"kind":"one tiny production proof through shared executor/supervisor","plan_sha256":hash(&serde_json::to_vec_pretty(&plan)?),"supervision":supervision,"result":result,"owned_ports":[8853,8854],"ports_free":true,"large_fixtures_generated":0,"large_replays_run":0,"performance_claim":false,"cpu_ms":null}),
    )?;
    println!("tiny same-path production proof PASS: {}", root.display());
    Ok(())
}
// This command is implemented for later review. It MUST NOT be invoked without
// separate root runtime GO and an explicitly approved owned raw prefix/config.
pub(super) fn batch(config: &Path) -> Result<()> {
    let config = json_from(config)?;
    if config["root_runtime_go"] != true || config["approved_raw_cap_bytes"] != RAW_CAP {
        return Err("separate root runtime GO and8GiB owned capacity required".into());
    }
    let own = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/fetch-phase-supervisor.rs");
    let module =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/fetch-phase-supervisor/replay.rs");
    if config["supervisor_source_sha256"] != stream_hash(&own)?
        || config["executor_source_sha256"] != stream_hash(&module)?
        || config["supervisor_elf_sha256"] != stream_hash(&std::env::current_exe()?)?
    {
        return Err("authorization does not pin reviewed source".into());
    }
    let root = PathBuf::from(
        config["approved_new_raw_prefix"]
            .as_str()
            .ok_or("owned prefix")?,
    );
    if root.exists() {
        return Err("raw prefix must be fresh".into());
    }
    let replicas = config["replicas_each"].as_u64().ok_or("replicas")?;
    if !(1..=2).contains(&replicas) {
        return Err("at most two replicas per case".into());
    }
    let inputs = config["sealed_plans"].as_array().ok_or("two plans")?;
    if inputs.len() != 2 {
        return Err("exactly two fixed plans required".into());
    }
    let allowed = [
        (
            "64mib-cold",
            "852558c0bcd8491061b762ac2a50352a89565d927fe2ae367e9e9dc22ad713a4",
        ),
        (
            "16mib-warm",
            "5f720f8368e75f4d9447871ce6809330e4f8db188d561c3457de51e21bf3072d",
        ),
    ];
    let mut plans = Vec::new();
    for (entry, (profile, expected)) in inputs.iter().zip(allowed) {
        let path = Path::new(entry.as_str().ok_or("plan path")?);
        if stream_hash(path)? != expected {
            return Err("fixed plan hash mismatch".into());
        }
        let plan = json_from(path)?;
        if plan["profile"] != profile {
            return Err("fixed plan profile mismatch".into());
        }
        geometry(&plan)?;
        let mut plan = plan;
        plan["sealed_original_input_sha256"] = json!(expected);
        plans.push(plan);
    }
    fs::create_dir(&root)?;
    save(&root, "approved-batch-config.json", &config)?;
    resource_preflight(&root)?;
    linux::subreaper()?;
    let mut results = Vec::new();
    for mut plan in plans {
        plan["root_runtime_go"] = json!(true);
        plan["raw_byte_cap"] = json!(RAW_CAP);
        plan["minimum_memory_bytes"] = json!(RAW_CAP);
        for replica in 0..replicas {
            save(&root, "active-plan.json", &plan)?;
            let case = root.join(format!(
                "{}-{replica}",
                plan["profile"].as_str().ok_or("profile")?
            ));
            let supervision = supervise(
                &case,
                "executor",
                Duration::from_secs(1200),
                Duration::from_secs(5),
            )?;
            let result = json_from(case.join("case-result.json"))?;
            if supervision["deadline_expired"] != false
                || supervision["leader_exit_code"] != 0
                || result["pass"] != true
            {
                save(
                    &root,
                    "batch-incomplete.json",
                    &json!({"case":case,"stopped_without_retry":true,"completed_cases":results,"after_counters":null}),
                )?;
                return Err("case failed/timed out; batch stopped without retry".into());
            }
            results.push(json!({"case":case,"result":result,"supervision":supervision}));
            capacity(&root, RAW_CAP, RAW_CAP)?;
        }
    }
    save(
        &root,
        "batch.json",
        &json!({"complete":true,"cases":results,"count":results.len(),"performance_claim":false,"cpu_ms":null}),
    )?;
    Ok(())
}
