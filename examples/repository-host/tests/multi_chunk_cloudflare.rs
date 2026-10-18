//! Explicit local correctness gate. Rust owns workerd and Native; no benchmark
//! runner, scorer, warmup or pricing output is produced.
use git_zero_host::{hash, native::command};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

fn run(program: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
        .env("GIT_TERMINAL_PROMPT", "0");
    command(cmd, input)
        .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"))
        .stdout
}
fn git(repo: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut all = vec!["-C", repo.to_str().unwrap()];
    all.extend(args);
    run("git", &all, input)
}
fn text(repo: &Path, args: &[&str]) -> String {
    String::from_utf8(git(repo, args, &[]))
        .unwrap()
        .trim()
        .into()
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.wait();
    }
}
fn spawn(root: &Path, program: &str, args: &[&str], log: &str) -> Owned {
    let file = File::create(root.join(log)).unwrap();
    Owned(
        Command::new("setsid")
            .arg(program)
            .args(args)
            .current_dir(root)
            .env("WRANGLER_SEND_METRICS", "false")
            .env("X_LOCAL_EXPLORER", "false")
            .env("X_LOCAL_OBSERVABILITY", "false")
            .env("BROWSER", "none")
            .env("WRANGLER_LOG_PATH", root.join("logs"))
            .stdin(Stdio::null())
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .spawn()
            .unwrap(),
    )
}
fn wait(root: &Path, process: &mut Owned, log: &str, needle: &str) {
    for _ in 0..300 {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "exited: {}",
            root.join(log).display()
        );
        if fs::read_to_string(root.join(log)).unwrap().contains(needle) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("not ready: {}", root.join(log).display());
}
fn request(root: &Path, name: &str, url: &str, body: Option<&[u8]>) -> Vec<u8> {
    let header = root.join(format!("{name}.headers"));
    let output = root.join(format!("{name}.body"));
    let mut args = vec![
        "--silent",
        "--show-error",
        "--max-time",
        "120",
        "--http1.1",
        "--dump-header",
        header.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--write-out",
        "%{http_code}",
        url,
    ];
    let input = root.join(format!("{name}.request"));
    let input_arg;
    if let Some(bytes) = body {
        fs::write(&input, bytes).unwrap();
        input_arg = format!("@{}", input.display());
        args.extend([
            "-H",
            "Content-Type: application/x-git-upload-pack-request",
            "--data-binary",
            &input_arg,
        ]);
    }
    let status = run("curl", &args, &[]);
    fs::write(root.join(format!("{name}.status")), &status).unwrap();
    let bytes = fs::read(output).unwrap();
    assert_eq!(
        String::from_utf8(status).unwrap(),
        "200",
        "HTTP failure for {name}, actual body retained ({} bytes)",
        bytes.len()
    );
    bytes
}

fn record_gate_context(root: &Path) {
    let environment: BTreeMap<_, _> = [
        "GIT_TRACE2_EVENT",
        "GIT_ZERO_MULTI_ARTIFACTS",
        "GIT_ZERO_MULTI_BASE",
        "GIT_ZERO_MULTI_MODE",
        "GIT_ZERO_MULTI_WRANGLER",
        "GIT_ZERO_MULTI_COHORT",
        "GIT_ZERO_MULTI_VIEW",
        "GIT_ZERO_MULTI_FIRST_READ",
        "GIT_ZERO_MULTI_SOCKET",
    ]
    .into_iter()
    .map(|key| (key, std::env::var(key).ok()))
    .collect();
    fs::write(
        root.join("runner-environment.json"),
        serde_json::to_vec_pretty(&environment).unwrap(),
    )
    .unwrap();
}

fn metrics(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&request(root, name, "http://127.0.0.1:8846/metrics", None)).unwrap()
}
fn n(value: &Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("missing {key}: {value}"))
}
fn usage(value: &Value, key: &str) -> u64 {
    n(&value["cloudflare_services"], key)
}
fn packet(value: &str) -> Vec<u8> {
    format!("{:04x}{value}", value.len() + 4).into_bytes()
}
fn fetch_body(want: &str, have: Option<&str>, agent: &str) -> Vec<u8> {
    let mut bytes = packet(&format!(
        "want {want} no-progress ofs-delta agent={agent}\n"
    ));
    bytes.extend(b"0000");
    if let Some(have) = have {
        bytes.extend(packet(&format!("have {have}\n")));
    }
    bytes.extend(packet("done\n"));
    bytes
}
fn fixture(size: usize, mut state: u64) -> Vec<u8> {
    let mut bytes = vec![0; size];
    for (index, byte) in bytes.iter_mut().enumerate() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if index % 8192 >= 4096 {
            *byte = state as u8;
        }
    }
    bytes
}
fn commit(source: &Path, number: usize) -> String {
    git(source, &["add", "."], &[]);
    git(
        source,
        &["commit", "-q", "-m", &format!("generation {number}")],
        &[],
    );
    text(source, &["rev-parse", "HEAD"])
}
fn push(source: &Path, gateway: &str, generation: usize) {
    git(
        source,
        &[
            "-c",
            "http.version=HTTP/1.1",
            "-c",
            "http.postBuffer=268435456",
            "-c",
            "pack.window=0",
            "push",
            "--quiet",
            gateway,
            "HEAD:refs/heads/main",
            &format!("HEAD:refs/heads/history-{generation:02}"),
        ],
        &[],
    );
}
fn check_budget(value: &Value) {
    assert!(
        n(value, "cache_retained_bytes") <= 32 * 1024 * 1024,
        "{value}"
    );
    assert!(
        n(value, "joint_cache_retained_peak_bytes") <= 32 * 1024 * 1024,
        "{value}"
    );
    assert!(n(value, "joint_cache_entries") <= 256, "{value}");
    assert_eq!(
        value["cloudflare_services"]["accounting_complete"], true,
        "{value}"
    );
}
fn read(
    root: &Path,
    source: &Path,
    client: &Path,
    gateway: &str,
    name: &str,
    want: &str,
    have: Option<&str>,
) -> Value {
    let before = metrics(root, &format!("{name}-before"));
    let bytes = request(
        root,
        name,
        &format!("{gateway}/git-upload-pack"),
        Some(&fetch_body(want, have, name)),
    );
    let after = metrics(root, &format!("{name}-after"));
    check_budget(&after);
    let at = bytes
        .windows(4)
        .position(|w| w == b"PACK")
        .expect("nonempty Git pack");
    git(
        client,
        &["index-pack", "--stdin", "--strict", "--fix-thin"],
        &bytes[at..],
    );
    git(client, &["update-ref", "refs/heads/verified", want], &[]);
    git(client, &["fsck", "--strict", "--no-reflogs"], &[]);
    assert_eq!(
        git(source, &["archive", want], &[]),
        git(client, &["archive", want], &[])
    );
    for key in [
        "r2_written_bytes",
        "r2_deletes",
        "r2_live_objects",
        "r2_live_bytes",
        "do_rows_written",
        "do_sqlite_bytes",
    ] {
        assert_eq!(usage(&before, key), usage(&after, key), "read grew {key}");
    }
    assert_eq!(
        before["cloudflare_services"]["operations"]["r2_put"],
        after["cloudflare_services"]["operations"]["r2_put"],
        "reads perform no PUT; observer LISTs remain separately physical"
    );
    json!({"name":name,"before":before,"after":after,"response_bytes":bytes.len(),"response_sha256":hash(&bytes),
        "gets":usage(&after,"r2_class_b")-usage(&before,"r2_class_b"),
        "read_bytes":usage(&after,"r2_read_bytes")-usage(&before,"r2_read_bytes")})
}
fn sources() -> BTreeMap<String, Value> {
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<String, Value>) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(base, &entry.unwrap().path(), out);
            }
        } else {
            let bytes = fs::read(path).unwrap();
            out.insert(
                path.strip_prefix(base).unwrap().to_string_lossy().into(),
                json!({"bytes":bytes.len(),"sha256":hash(&bytes)}),
            );
        }
    }
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut out = BTreeMap::new();
    for rel in [
        "src",
        "cloudflare/src",
        "Cargo.toml",
        "cloudflare/wrangler.toml",
        "tests/multi_chunk_cloudflare.rs",
        "cloudflare/test/first-read-worker.js",
        "cloudflare/test/small-receive-worker.js",
        "cloudflare/test/socket-rpc-worker.js",
    ] {
        visit(&base, &base.join(rel), &mut out);
    }
    for (name, path) in [
        ("workspace_lock", base.join("../../Cargo.lock")),
        (
            "native_binary",
            PathBuf::from(env!("CARGO_BIN_EXE_repository-host")),
        ),
        ("runner_binary", std::env::current_exe().unwrap()),
    ] {
        let bytes = fs::read(&path).unwrap();
        out.insert(
            name.into(),
            json!({"path":path,"bytes":bytes.len(),"sha256":hash(&bytes)}),
        );
    }
    if let Ok(cli) = std::env::var("GIT_ZERO_MULTI_WRANGLER") {
        let cli = PathBuf::from(cli);
        let modules = cli.parent().unwrap().parent().unwrap().parent().unwrap();
        for (name, path) in [
            ("runtime_node", PathBuf::from("/home/pawel/.local/bin/node")),
            ("runtime_wrangler_entry", cli.clone()),
            (
                "runtime_wrangler_cli",
                cli.parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("wrangler-dist/cli.js"),
            ),
            ("runtime_workerd_entry", modules.join("workerd/bin/workerd")),
            (
                "runtime_workerd_binary",
                modules.join("@cloudflare/workerd-linux-64/bin/workerd"),
            ),
        ] {
            let bytes = fs::read(&path).unwrap();
            out.insert(name.into(), json!({"path":path,"resolved_path":path.canonicalize().unwrap(),"bytes":bytes.len(),"sha256":hash(&bytes)}));
        }
    }
    out
}

#[test]
#[ignore = "explicit Rust-owned local production-workerd gate on ports 8845/8846"]
fn multi_chunk_production_gate() {
    let root =
        PathBuf::from(std::env::var("GIT_ZERO_MULTI_ARTIFACTS").expect("fresh artifact root"));
    assert!(
        !root.exists(),
        "retain previous attempts; use a new artifact root"
    );
    fs::create_dir_all(&root).unwrap();
    record_gate_context(&root);
    let before = sources();
    fs::write(
        root.join("source-before.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    let view = std::env::var("GIT_ZERO_MULTI_VIEW").unwrap_or_else(|_| "cold".into());
    let first_read = std::env::var("GIT_ZERO_MULTI_FIRST_READ").unwrap_or_else(|_| "on".into());
    let socket = std::env::var("GIT_ZERO_MULTI_SOCKET").unwrap_or_else(|_| "on".into());
    let mode = std::env::var("GIT_ZERO_MULTI_MODE").unwrap_or_else(|_| "multi".into());
    assert!(matches!(mode.as_str(), "one" | "multi" | "joint" | "off"));
    let initial = std::env::var("GIT_ZERO_MULTI_INITIAL_MIB")
        .unwrap_or_else(|_| "16".into())
        .parse::<usize>()
        .unwrap();
    assert!(initial == 16 || initial == 64);
    for port in [8845, 8846] {
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).expect("owned port free"));
    }
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let faults = std::env::var_os("GIT_ZERO_MULTI_FAULTS").is_some();
    let pressure = std::env::var_os("GIT_ZERO_MULTI_PLAN_PRESSURE").is_some();
    if pressure {
        assert_eq!(initial, 64);
        assert!(!faults);
    }
    let config = if faults || pressure {
        fs::read_to_string(worker.join("test/socket-rpc.toml"))
            .unwrap()
            .replace(
                r#"main = "socket-rpc-worker.js""#,
                &format!("main = {:?}", worker.join("test/socket-rpc-worker.js")),
            )
    } else {
        fs::read_to_string(worker.join("wrangler.toml"))
            .unwrap()
            .replace(
                r#"main = "src/index.js""#,
                &format!("main = {:?}", worker.join("src/index.js")),
            )
    };
    fs::write(root.join("wrangler.toml"), config).unwrap();
    let cli = std::env::var("GIT_ZERO_MULTI_WRANGLER").unwrap_or_else(|_| {
        worker
            .join("node_modules/wrangler/bin/wrangler.js")
            .to_string_lossy()
            .into()
    });
    let config_path = root.join("wrangler.toml");
    let persist = root.join("persist");
    let mut runtime = spawn(
        &root,
        "node",
        &[
            &cli,
            "dev",
            "--config",
            config_path.to_str().unwrap(),
            "--ip",
            "127.0.0.1",
            "--port",
            "8845",
            "--local",
            "--persist-to",
            persist.to_str().unwrap(),
            "--show-interactive-dev-session=false",
        ],
        "wrangler.log",
    );
    wait(
        &root,
        &mut runtime,
        "wrangler.log",
        "Ready on http://127.0.0.1:8845",
    );
    let native_root = root.join("native");
    let mut native = spawn(
        &root,
        env!("CARGO_BIN_EXE_repository-host"),
        &[
            "--cloudflare",
            "http://127.0.0.1:8845",
            "--repo",
            "protected-gate",
            "--view-cache",
            &view,
            "--gateway-first-read",
            &first_read,
            "--socket-rpc",
            &socket,
            "--protected-chunk",
            &mode,
            native_root.to_str().unwrap(),
            "8846",
        ],
        "host.log",
    );
    wait(&root, &mut native, "host.log", "Git gateway:");
    let gateway = "http://127.0.0.1:8845/protected-gate/repo.git";
    let source = root.join("source");
    let client = root.join("client.git");
    run(
        "git",
        &["init", "-q", "-b", "main", source.to_str().unwrap()],
        &[],
    );
    run(
        "git",
        &["init", "-q", "--bare", client.to_str().unwrap()],
        &[],
    );
    git(&source, &["config", "user.name", "Protected gate"], &[]);
    git(
        &source,
        &["config", "user.email", "protected@example.invalid"],
        &[],
    );
    git(&source, &["config", "core.compression", "1"], &[]);
    git(&source, &["config", "pack.window", "0"], &[]);
    fs::write(
        source.join("base"),
        fixture(initial * 1024 * 1024, 0x84aa29b17),
    )
    .unwrap();
    let first = commit(&source, 1);
    let mut rows = Vec::new();
    push(&source, gateway, 1);
    let initial_metrics = metrics(&root, "initial-push-after");
    check_budget(&initial_metrics);
    assert_eq!(
        n(&initial_metrics, "protected_chunk_admissions"),
        0,
        "PUT/setup cannot admit"
    );
    assert_eq!(
        n(&initial_metrics, "cache_entries"),
        0,
        "push response cache unchanged"
    );
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "initial-fetch",
        &first,
        None,
    ));
    let admitted = metrics(&root, "admitted");
    if mode != "off" && view == "cold" {
        assert!(n(&admitted, "protected_chunk_admissions") >= 1);
        assert!(n(&admitted, "protected_chunk_bytes") > 8 * 1024 * 1024);
        if initial == 64 {
            assert!(n(&admitted, "protected_chunk_bytes") >= 16 * 1024 * 1024);
        }
    } else {
        assert_eq!(n(&admitted, "protected_chunk_admissions"), 0);
    }
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "same-history-fresh-authority",
        &first,
        None,
    ));
    if mode != "off" && initial == 16 && view == "cold" {
        assert_eq!(
            rows.last().unwrap()["gets"],
            0,
            "all-cache ordinary fallback"
        );
    }
    let restart_before_receive = std::env::var_os("GIT_ZERO_MULTI_RESTART_RECEIVE").is_some();
    if restart_before_receive {
        drop(native);
        native = spawn(
            &root,
            env!("CARGO_BIN_EXE_repository-host"),
            &[
                "--cloudflare",
                "http://127.0.0.1:8845",
                "--repo",
                "protected-gate",
                "--view-cache",
                &view,
                "--gateway-first-read",
                &first_read,
                "--socket-rpc",
                &socket,
                "--protected-chunk",
                &mode,
                native_root.to_str().unwrap(),
                "8846",
            ],
            "host-restarted.log",
        );
        wait(&root, &mut native, "host-restarted.log", "Git gateway:");
        let restarted = metrics(&root, "restarted-before-receive");
        assert_eq!(
            n(&restarted, "protected_chunk_admissions"),
            0,
            "restart recovery cannot warm cache"
        );
        assert_eq!(n(&restarted, "protected_chunk_bytes"), 0);
    }
    let mut history = vec![first.clone()];
    for generation in 2..=16 {
        if generation % 4 == 1 {
            fs::write(
                source.join("replacement"),
                fixture(8 * 1024 * 1024, 0x77700 + generation as u64),
            )
            .unwrap();
        }
        fs::write(
            source.join("change"),
            format!("generation {generation}\n").repeat(100),
        )
        .unwrap();
        let want = commit(&source, generation);
        history.push(want.clone());
        let before = metrics(&root, &format!("push-{generation}-before"));
        push(&source, gateway, generation);
        let after = metrics(&root, &format!("push-{generation}-after"));
        check_budget(&after);
        if restart_before_receive && generation == 2 && mode != "off" {
            assert_eq!(n(&after, "gateway_receive_consumed"), 1);
            assert_eq!(
                n(&after, "protected_chunk_admissions"),
                1,
                "prepared Full receive admits and must not invalidate its own stable token"
            );
        }
        assert_eq!(n(&after, "cache_entries"), 0);
        assert_eq!(n(&after, "cache_bytes"), 0);
        rows.push(json!({"name":format!("push-{generation}"),"before":before,"after":after}));
        let row = read(
            &root,
            &source,
            &client,
            gateway,
            &format!("latest-{generation}"),
            &want,
            Some(&first),
        );
        assert!(
            if view == "cold" {
                row["gets"].as_u64().unwrap() > 0
            } else {
                row["gets"] == 0
            },
            "new output naturally needs actual GET"
        );
        rows.push(row);
        if generation % 4 == 0 {
            let middle = &history[generation / 2 - 1];
            rows.push(read(
                &root,
                &source,
                &client,
                gateway,
                &format!("seeded-history-{generation}"),
                middle,
                Some(&first),
            ));
            rows.push(read(
                &root,
                &source,
                &client,
                gateway,
                &format!("latest-recheck-{generation}"),
                &want,
                Some(middle),
            ));
            rows.push(read(
                &root,
                &source,
                &client,
                gateway,
                &format!("history-{generation}"),
                &first,
                None,
            ));
        }
    }
    if !faults && !pressure {
        let (tip, thin) = external_thin(
            &root,
            &source,
            gateway,
            view == "cold" && first_read == "on",
        );
        rows.push(thin);
        let row = read(
            &root,
            &source,
            &client,
            gateway,
            "thin-latest",
            &tip,
            Some(&first),
        );
        assert!(if view == "cold" {
            row["gets"].as_u64().unwrap() > 0
        } else {
            row["gets"] == 0
        });
        rows.push(row);
    }
    if pressure {
        protected_plan_pressure(&root, &source, &client, gateway, &first);
    }
    if faults {
        protected_faults(&root, &source, &client, gateway);
    }
    let final_metrics = metrics(&root, "final");
    if faults {
        fault_bounds(&final_metrics);
    } else {
        check_budget(&final_metrics);
    }
    if mode != "off" && !faults && !pressure && view == "cold" {
        assert!(n(&final_metrics, "protected_chunk_hits") > 10);
        if mode == "one" {
            assert_eq!(n(&final_metrics, "protected_chunk_entries"), 1);
        } else {
            assert!(n(&final_metrics, "protected_chunk_admissions") > 1);
            assert!(n(&final_metrics, "protected_chunk_prunes") > 0);
            assert!(n(&final_metrics, "protected_chunk_rejections") > 0);
        }
    }
    let after = sources();
    assert_eq!(before, after, "source/binary changed during gate");
    fs::write(
        root.join("source-after.json"),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    fs::write(root.join("contract.json"),serde_json::to_vec_pretty(&json!({"kind":"actual production workerd correctness/accounting; no benchmark","mode":mode,"initial_mib":initial,"rows":rows,"final":final_metrics,"full_bill_usd":null})).unwrap()).unwrap();
    drop(native);
    drop(runtime);
    fs::write(
        root.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_ports":[8845,8846],"owned_process_groups_stopped":true}),
        )
        .unwrap(),
    )
    .unwrap();
}
// Appended only after the first unchanged-source production pair finishes.
struct Counted;
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn allocated(bytes: usize) {
    use std::sync::atomic::Ordering::SeqCst;
    let n = LIVE.fetch_add(bytes, SeqCst) + bytes;
    PEAK.fetch_max(n, SeqCst);
}
unsafe impl std::alloc::GlobalAlloc for Counted {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc_zeroed(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), std::sync::atomic::Ordering::SeqCst);
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        let q = unsafe { std::alloc::System.realloc(p, l, n) };
        if !q.is_null() {
            LIVE.fetch_sub(l.size(), std::sync::atomic::Ordering::SeqCst);
            allocated(n);
        }
        q
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let target = to.join(item.file_name());
        if item.file_type().unwrap().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(item.path()).unwrap(), target).unwrap();
        } else if item.path().is_dir() {
            copy_tree(&item.path(), &target);
        } else {
            fs::copy(item.path(), target).unwrap();
        }
    }
}
fn direct_metrics(host: &mut git_zero_host::native::NativeHost) -> Value {
    let response = host.handle(git_zero_host::http::Request {
        method: "GET".into(),
        target: "/metrics".into(),
        headers: BTreeMap::new(),
        body: vec![],
    });
    assert_eq!(response.status, 200);
    serde_json::from_slice(&response.body).unwrap()
}
fn direct_request(
    host: &mut git_zero_host::native::NativeHost,
    target: &str,
    body: Vec<u8>,
) -> git_zero_host::http::Response {
    host.handle(git_zero_host::http::Request {
        method: "POST".into(),
        target: format!("/repo.git/git-{target}-pack"),
        headers: BTreeMap::from([
            (
                "content-type".into(),
                format!("application/x-git-{target}-pack-request"),
            ),
            ("content-length".into(), body.len().to_string()),
        ]),
        body,
    })
}
fn services_manifest(host: &mut git_zero_host::native::NativeHost) -> Value {
    let services = host.cloudflare_services().unwrap();
    let manifest =
        git_zero_host::storage::CloudflareBindings::manifest_load(services.as_ref()).unwrap();
    serde_json::to_value(manifest).unwrap()
}

fn direct_push(host: &mut git_zero_host::native::NativeHost, source: &Path, old: &str, new: &str) {
    let packed = git(
        source,
        &[
            "pack-objects",
            "--stdout",
            "--revs",
            "--delta-base-offset",
            "--window=0",
        ],
        format!("{new}\n^{old}\n").as_bytes(),
    );
    let mut body = packet(&format!(
        "{old} {new} refs/heads/main\0 report-status ofs-delta\n"
    ));
    body.extend(b"0000");
    body.extend(packed);
    let response = direct_request(host, "receive", body);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert!(String::from_utf8_lossy(&response.body).contains("ok refs/heads/main"));
}

#[test]
#[ignore = "explicit local production-workerd allocation/pressure gate; owns 8845"]
fn protected_chunk_allocation_pressure_gate() {
    use std::sync::atomic::Ordering::SeqCst;
    let root = PathBuf::from(std::env::var("GIT_ZERO_MULTI_ARTIFACTS").expect("new artifact root"));
    assert!(!root.exists());
    fs::create_dir_all(&root).unwrap();
    record_gate_context(&root);
    let base = PathBuf::from(
        std::env::var("GIT_ZERO_MULTI_BASE").expect("completed immutable 64 MiB gate fixture"),
    );
    let before = sources();
    fs::write(
        root.join("source-before.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    copy_tree(&base.join("persist"), &root.join("persist"));
    copy_tree(&base.join("source"), &root.join("source"));
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let cfg = fs::read_to_string(worker.join("wrangler.toml"))
        .unwrap()
        .replace(
            "main = \"src/index.js\"",
            &format!("main = {:?}", worker.join("src/index.js")),
        );
    fs::write(root.join("wrangler.toml"), cfg).unwrap();
    drop(std::net::TcpListener::bind("127.0.0.1:8845").unwrap());
    let cli = std::env::var("GIT_ZERO_MULTI_WRANGLER").unwrap();
    let cfg = root.join("wrangler.toml");
    let persist = root.join("persist");
    let mut runtime = spawn(
        &root,
        "node",
        &[
            &cli,
            "dev",
            "--config",
            cfg.to_str().unwrap(),
            "--ip",
            "127.0.0.1",
            "--port",
            "8845",
            "--local",
            "--persist-to",
            persist.to_str().unwrap(),
            "--show-interactive-dev-session=false",
        ],
        "wrangler.log",
    );
    wait(
        &root,
        &mut runtime,
        "wrangler.log",
        "Ready on http://127.0.0.1:8845",
    );
    let mut host = git_zero_host::native::NativeHost::open_cloudflare(
        root.join("codec"),
        "http://127.0.0.1:8845",
        "protected-gate",
        "git-zero-local-services",
        true,
    )
    .unwrap();
    let services = host.cloudflare_services().unwrap();
    services.enable_socket_rpc(true);
    services.register("http://127.0.0.1:8846").unwrap();
    drop(services);
    let initial = direct_metrics(&mut host);
    assert_eq!(
        n(&initial, "protected_chunk_admissions"),
        0,
        "actual startup GETs cannot admit"
    );
    let source = root.join("source");
    let manifest = services_manifest(&mut host);
    let main = manifest["refs"]["refs/heads/main"].as_str().unwrap();
    git(&source, &["reset", "--hard", main], &[]);
    let old = text(&source, &["rev-parse", "HEAD"]);
    let base_live = LIVE.load(SeqCst);
    PEAK.store(base_live, SeqCst);
    let response = direct_request(
        &mut host,
        "upload",
        fetch_body(&old, None, "allocation-first"),
    );
    assert_eq!(response.status, 200);
    drop(response);
    let retained_live = LIVE.load(SeqCst);
    let transient_peak = PEAK.load(SeqCst);
    let admitted = direct_metrics(&mut host);
    check_budget(&admitted);
    assert!(n(&admitted, "protected_chunk_bytes") >= 16 * 1024 * 1024);
    assert!(
        retained_live.saturating_sub(base_live) <= n(&admitted, "cache_retained_bytes") as usize,
        "actual owned heap delta must fit conservative retained charge"
    );
    let first_record = json!({"baseline_live":base_live,"retained_live":retained_live,"delta":retained_live.saturating_sub(base_live),"request_peak_live":transient_peak,"metrics":admitted});
    fs::write(
        root.join("first-allocation.json"),
        serde_json::to_vec_pretty(&first_record).unwrap(),
    )
    .unwrap();
    fs::write(source.join("pressure"), fixture(63 * 512 * 1024, 0x2819aa)).unwrap();
    let new = commit(&source, 9);
    direct_push(&mut host, &source, &old, &new);
    let post_push = direct_metrics(&mut host);
    assert!(n(&post_push, "protected_chunk_bytes") >= 16 * 1024 * 1024);
    assert_eq!(n(&post_push, "cache_entries"), 0);
    let mut rows = Vec::new();
    for index in 0..12 {
        let before = direct_metrics(&mut host);
        let live = LIVE.load(SeqCst);
        PEAK.store(live, SeqCst);
        let response = direct_request(
            &mut host,
            "upload",
            fetch_body(&new, Some(&old), &format!("pressure-{index}")),
        );
        assert_eq!(response.status, 200);
        let bytes = response.body.len();
        assert!(
            bytes <= 16 * 1024 * 1024 && bytes > 16 * 1024 * 1024 - 262144,
            "productive near-max response: {bytes}"
        );
        drop(response);
        let retained = LIVE.load(SeqCst);
        let peak = PEAK.load(SeqCst);
        let after = direct_metrics(&mut host);
        fs::write(
            root.join(format!("pressure-{index}-metrics.json")),
            serde_json::to_vec_pretty(&after).unwrap(),
        )
        .unwrap();
        check_budget(&after);
        assert_eq!(
            n(&after, "protected_chunk_bytes"),
            0,
            "productive response evicts protected slot before retention"
        );
        assert_eq!(n(&after, "cache_entries"), 1);
        assert!(n(&after, "protected_chunk_evictions") > n(&before, "protected_chunk_evictions"));
        assert!(
            usage(&after, "r2_class_b") > usage(&before, "r2_class_b"),
            "pressure/refill uses actual GETs"
        );
        rows.push(json!({"index":index,"response_bytes":bytes,"live_before":live,"retained_live":retained,"request_peak_live":peak,"before":before,"after":after}));
    }
    drop(host);
    let mut reopened = git_zero_host::native::NativeHost::open_cloudflare(
        root.join("reopen"),
        "http://127.0.0.1:8845",
        "protected-gate",
        "git-zero-local-services",
        true,
    )
    .unwrap();
    let restart = direct_metrics(&mut reopened);
    assert_eq!(n(&restart, "protected_chunk_admissions"), 0);
    assert_eq!(n(&restart, "protected_chunk_bytes"), 0);
    let after = sources();
    assert_eq!(before, after);
    fs::write(
        root.join("source-after.json"),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    fs::write(root.join("allocation-contract.json"),serde_json::to_vec_pretty(&json!({"kind":"actual production workerd, in-process Native counted allocator; correctness only","copied_fixture_from":base,"initial":initial,"admitted":first_record,"pressure":rows,"restart":restart,"full_bill_usd":null})).unwrap()).unwrap();
    drop(reopened);
    drop(runtime);
    fs::write(
        root.join("cleanup.json"),
        "{\"owned_process_groups_stopped\":true,\"owned_port\":8845}\n",
    )
    .unwrap();
}
fn service(root: &Path, name: &str, route: &str, body: Option<&Value>) -> Value {
    let url = format!("http://127.0.0.1:8845/_services/{route}");
    let header = root.join(format!("{name}.headers"));
    let output = root.join(format!("{name}.body"));
    let mut args = vec![
        "--silent",
        "--show-error",
        "--max-time",
        "120",
        "--dump-header",
        header.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--write-out",
        "%{http_code}",
        "-H",
        "x-hosting-token: git-zero-local-services",
        "-H",
        "x-hosting-repo: protected-gate",
    ];
    let file = root.join(format!("{name}.request"));
    let file_arg;
    if let Some(body) = body {
        fs::write(&file, serde_json::to_vec(body).unwrap()).unwrap();
        file_arg = format!("@{}", file.display());
        args.extend([
            "-H",
            "content-type: application/json",
            "--data-binary",
            &file_arg,
        ]);
    }
    args.push(&url);
    let status = run("curl", &args, &[]);
    fs::write(root.join(format!("{name}.status")), &status).unwrap();
    let bytes = fs::read(output).unwrap();
    assert_eq!(
        String::from_utf8(status).unwrap(),
        "200",
        "service failure for {name}, actual body retained ({} bytes)",
        bytes.len()
    );
    serde_json::from_slice(&bytes).unwrap()
}

fn fault_http(root: &Path, name: &str, url: &str, body: &[u8], receive: bool) -> (u16, Vec<u8>) {
    let input = root.join(format!("{name}.request"));
    let output = root.join(format!("{name}.body"));
    let header = root.join(format!("{name}.headers"));
    fs::write(&input, body).unwrap();
    let status = run(
        "curl",
        &[
            "--silent",
            "--show-error",
            "--max-time",
            "120",
            "--http1.1",
            "--dump-header",
            header.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--write-out",
            "%{http_code}",
            "-H",
            if receive {
                "content-type: application/x-git-receive-pack-request"
            } else {
                "content-type: application/x-git-upload-pack-request"
            },
            "--data-binary",
            &format!("@{}", input.display()),
            url,
        ],
        &[],
    );
    (
        String::from_utf8(status).unwrap().parse().unwrap(),
        fs::read(output).unwrap(),
    )
}
fn commit_on(source: &Path, parent: &str, serial: usize) -> String {
    let tree = text(source, &["write-tree"]);
    String::from_utf8(git(
        source,
        &["commit-tree", &tree, "-p", parent],
        format!("fault generation {serial}\n").as_bytes(),
    ))
    .unwrap()
    .trim()
    .into()
}
fn receive_body(source: &Path, parent: &str, tip: &str, thin: bool) -> Vec<u8> {
    let mut args = vec!["pack-objects", "--stdout", "--revs", "--delta-base-offset"];
    args.push(if thin { "--window=10" } else { "--window=0" });
    if thin {
        args.push("--thin");
    }
    let packed = git(source, &args, format!("{tip}\n^{parent}\n").as_bytes());
    let mut body = packet(&format!(
        "{parent} {tip} refs/heads/main\0 report-status ofs-delta\n"
    ));
    body.extend(b"0000");
    body.extend(packed);
    body
}
fn fault_bounds(value: &Value) {
    assert!(n(value, "cache_retained_bytes") <= 32 * 1024 * 1024);
    assert!(n(value, "joint_cache_retained_peak_bytes") <= 32 * 1024 * 1024);
    assert!(n(value, "joint_cache_entries") <= 256);
}
fn external_thin(root: &Path, source: &Path, gateway: &str, prepared: bool) -> (String, Value) {
    // A true external-base thin pack, completed using the cached+remote Full
    // snapshot after a historical projection. No benchmark fixture is changed.
    let manifest = service(root, "thin-manifest", "manifest", None);
    let parent = manifest["refs"]["refs/heads/main"].as_str().unwrap();
    let mut base = fs::read(source.join("base")).unwrap();
    base[1] ^= 1;
    fs::write(source.join("base"), base).unwrap();
    git(source, &["add", "base"], &[]);
    let tip = commit_on(source, parent, 100);
    let thin = receive_body(source, parent, &tip, true);
    let offset = thin.windows(4).position(|w| w == b"PACK").unwrap();
    let empty = root.join("thin-proof.git");
    run(
        "git",
        &["init", "-q", "--bare", empty.to_str().unwrap()],
        &[],
    );
    let mut probe = Command::new("git");
    probe.args([
        "-C",
        empty.to_str().unwrap(),
        "index-pack",
        "--stdin",
        "--strict",
    ]);
    let unresolved = command(probe, &thin[offset..])
        .expect_err("actual thin pack must require an external base");
    fs::write(
        root.join("thin-unresolved-proof.txt"),
        unresolved.to_string(),
    )
    .unwrap();
    let before = metrics(root, "thin-before");
    let (status, body) = fault_http(
        root,
        "thin-receive",
        &format!("{gateway}/git-receive-pack"),
        &thin,
        true,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("ok refs/heads/main"));
    let after = metrics(root, "thin-after");
    fault_bounds(&after);
    if prepared {
        assert!(n(&after, "gateway_receive_consumed") > n(&before, "gateway_receive_consumed"));
    }
    (
        tip,
        json!({"kind":"actual external-base thin Full receive","before":before,"after":after,"request_bytes":thin.len(),"response_sha256":hash(&body)}),
    )
}

fn protected_faults(root: &Path, source: &Path, client: &Path, gateway: &str) {
    let mut rows = Vec::new();
    let mut serial = 101;
    let have = text(source, &["rev-parse", "HEAD^"]);
    let published = service(root, "fault-published-have", "manifest", None);
    assert!(
        published["refs"]
            .as_object()
            .unwrap()
            .values()
            .any(|id| id.as_str() == Some(have.as_str())),
        "private planning requires a genuinely published have"
    );
    let (tip, thin) = external_thin(root, source, gateway, true);
    rows.push(thin);
    // All controls wrap the actual production Worker and actual binding calls.
    for mode in [
        "continue-cache-fill",
        "get-failure",
        "get-corrupt",
        "continue-auth",
        "continue-corrupt-signed",
        "continue-drop",
        "unknown-sql",
        "missing-receipt",
        "get-body-failure",
        "generation-change",
    ] {
        service(
            root,
            &format!("fetch-{mode}-arm"),
            "test/first-read-arm",
            Some(&json!({"mode":mode})),
        );
        let before = metrics(root, &format!("fetch-{mode}-before"));
        let (status, body) = fault_http(
            root,
            &format!("fetch-{mode}"),
            &format!("{gateway}/git-upload-pack"),
            &fetch_body(&tip, Some(&have), &format!("fault-{mode}")),
            false,
        );
        let after = metrics(root, &format!("fetch-{mode}-after"));
        fault_bounds(&after);
        let trace = service(
            root,
            &format!("fetch-{mode}-trace"),
            "test/first-read-trace",
            None,
        );
        assert!(
            matches!(mode, "unknown-sql" | "missing-receipt")
                || trace["gateway_hook_fired"] == true
                || trace["fired"] == true,
            "actual fault must fire {mode}: {trace}"
        );
        assert_eq!(
            status == 200,
            matches!(
                mode,
                "continue-cache-fill" | "unknown-sql" | "generation-change"
            ),
            "{mode}: {status}: {}",
            String::from_utf8_lossy(&body)
        );
        if mode == "unknown-sql" {
            assert!(
                after["cloudflare_services"]["do_rows_read"].is_null(),
                "unknown cursor stays NULL"
            );
        }
        if mode == "continue-cache-fill" {
            assert!(
                n(&after, "gateway_read_paid_discards") > n(&before, "gateway_read_paid_discards")
            );
        }
        service(
            root,
            &format!("fetch-{mode}-disarm"),
            "test/first-read-arm",
            Some(&json!({"mode":""})),
        );
        let (healthy, pack) = fault_http(
            root,
            &format!("fetch-{mode}-healthy"),
            &format!("{gateway}/git-upload-pack"),
            &fetch_body(&tip, Some(&have), &format!("healthy-{mode}")),
            false,
        );
        assert_eq!(healthy, 200);
        let at = pack.windows(4).position(|w| w == b"PACK").unwrap();
        git(
            client,
            &["index-pack", "--stdin", "--strict", "--fix-thin"],
            &pack[at..],
        );
        assert_eq!(
            git(source, &["archive", &tip], &[]),
            git(client, &["archive", &tip], &[])
        );
        rows.push(json!({"kind":"read fault","mode":mode,"status":status,"before":before,"after":after,"trace":trace,"response_sha256":hash(&body)}));
        fs::write(
            root.join("fault-contract.json"),
            serde_json::to_vec_pretty(&json!({"rows":rows,"full_bill_usd":null})).unwrap(),
        )
        .unwrap();
    }
    for mode in [
        "put-after",
        "sql-before",
        "transaction",
        "sync-after",
        "lost-reply",
        "partial-delete",
        "unknown-sql",
        "gateway-lost-reply",
        "socket-lost-reply",
        "socket-bad-id",
        "socket-duplicate-segment",
        "socket-truncated",
        "socket-bad-terminal",
    ] {
        let mut fired = false;
        for attempt in 0..5 {
            let manifest = service(
                root,
                &format!("push-{mode}-{attempt}-manifest"),
                "manifest",
                None,
            );
            let parent = manifest["refs"]["refs/heads/main"].as_str().unwrap();
            let next = commit_on(source, parent, serial);
            serial += 1;
            service(
                root,
                &format!("push-{mode}-{attempt}-arm"),
                "test/small-receive-arm",
                Some(&json!({"mode":mode})),
            );
            let before = metrics(root, &format!("push-{mode}-{attempt}-before"));
            let (status, body) = fault_http(
                root,
                &format!("push-{mode}-{attempt}"),
                &format!("{gateway}/git-receive-pack"),
                &receive_body(source, parent, &next, false),
                true,
            );
            let after = metrics(root, &format!("push-{mode}-{attempt}-after"));
            fault_bounds(&after);
            let failed = service(
                root,
                &format!("push-{mode}-{attempt}-trace"),
                "test/small-receive-trace",
                None,
            );
            fired = failed["fired"] == true
                || failed["socket_fault"]["fired"] == true
                || failed["gateway_fired"] == true;
            rows.push(json!({"kind":"publication attempt","mode":mode,"attempt":attempt,"fired":fired,"status":status,"before":before,"after":after,"trace":failed,"response_sha256":hash(&body)}));
            if !fired {
                assert_eq!(status, 200);
                continue;
            }
            assert_eq!(
                status,
                if matches!(mode, "partial-delete" | "unknown-sql") {
                    200
                } else if mode == "gateway-lost-reply" {
                    500
                } else {
                    503
                },
                "{mode}: {}",
                String::from_utf8_lossy(&body)
            );
            let cas = failed["trace"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["name"] == "manifest_cas")
                .count();
            assert!(cas <= 1, "attempted mutation is not replayed {mode}");
            let current = service(
                root,
                &format!("push-{mode}-{attempt}-authoritative"),
                "manifest",
                None,
            );
            let parent = current["refs"]["refs/heads/main"].as_str().unwrap();
            service(
                root,
                &format!("push-{mode}-{attempt}-disarm"),
                "test/small-receive-arm",
                Some(&json!({"mode":""})),
            );
            let next = commit_on(source, parent, serial);
            serial += 1;
            let (next_status, next_body) = fault_http(
                root,
                &format!("push-{mode}-{attempt}-next"),
                &format!("{gateway}/git-receive-pack"),
                &receive_body(source, parent, &next, false),
                true,
            );
            assert_eq!(
                next_status,
                200,
                "{mode}: {}",
                String::from_utf8_lossy(&next_body)
            );
            assert!(String::from_utf8_lossy(&next_body).contains("ok refs/heads/main"));
            let follow = metrics(root, &format!("push-{mode}-{attempt}-follow"));
            fault_bounds(&follow);
            let trace = service(
                root,
                &format!("push-{mode}-{attempt}-follow-trace"),
                "test/small-receive-trace",
                None,
            );
            let ops = trace["trace"].as_array().unwrap();
            let put = ops.iter().position(|v| v["name"] == "r2_put").unwrap();
            if !matches!(mode, "unknown-sql" | "gateway-lost-reply") {
                assert!(
                    ops[..put]
                        .iter()
                        .any(|v| v["name"] == "r2_list" && v["route"] == "/_services/gc"),
                    "SAME Native physical GC before next PUT {mode}: {trace}"
                );
            }
            let view = root.join("native/view/repo.git");
            git(&view, &["fsck", "--strict", "--full"], &[]);
            assert_eq!(
                git(source, &["archive", &next], &[]),
                git(&view, &["archive", &next], &[])
            );
            rows.push(json!({"kind":"same Native distinct next push","mode":mode,"follow":follow,"trace":trace}));
            break;
        }
        assert!(
            fired,
            "bounded actual publication fault never fired: {mode}"
        );
        fs::write(
            root.join("fault-contract.json"),
            serde_json::to_vec_pretty(&json!({"rows":rows,"full_bill_usd":null})).unwrap(),
        )
        .unwrap();
    }
}
fn protected_plan_pressure(root: &Path, source: &Path, client: &Path, gateway: &str, first: &str) {
    let old = text(source, &["rev-parse", "HEAD"]);
    let planned_want = text(source, &["rev-parse", "HEAD^"]);
    let published = service(root, "pressure-published-want", "manifest", None);
    assert!(
        published["refs"]
            .as_object()
            .unwrap()
            .values()
            .any(|id| id.as_str() == Some(planned_want.as_str())),
        "outer private want must remain a published ref"
    );
    fs::write(source.join("pressure"), fixture(63 * 512 * 1024, 0x2819aa)).unwrap();
    let tip = commit(source, 9);
    push(source, gateway, 9);
    let big = read(
        root,
        source,
        client,
        gateway,
        "pressure-big-response",
        &tip,
        Some(&old),
    );
    assert!(big["response_bytes"].as_u64().unwrap() > 16 * 1024 * 1024 - 262144);
    assert_eq!(
        big["after"]["protected_chunk_bytes"], 0,
        "actual big response evicts"
    );
    let refill = read(
        root,
        source,
        client,
        gateway,
        "pressure-initial-history-refill",
        first,
        None,
    );
    assert!(
        n(&refill["after"], "protected_chunk_entries") > 0,
        "actual fitting ordinary GET must populate a bounded cached mask without evicting the live large response"
    );
    service(
        root,
        "pressure-plan-arm",
        "test/first-read-arm",
        Some(
            &json!({"mode":"continue-cache-pressure","pressure":fetch_body(&tip,Some(&old),"different-cache-pressure-request")}),
        ),
    );
    let before = metrics(root, "pressure-plan-before");
    let response = read(
        root,
        source,
        client,
        gateway,
        "pressure-plan-discard",
        &planned_want,
        Some(first),
    );
    let after = metrics(root, "pressure-plan-after");
    let trace = service(root, "pressure-plan-trace", "test/first-read-trace", None);
    assert_eq!(trace["gateway_hook_fired"], true);
    assert!(n(&after, "gateway_read_paid_discards") > n(&before, "gateway_read_paid_discards"));
    assert!(n(&after, "protected_chunk_evictions") > n(&before, "protected_chunk_evictions"));
    assert!(n(&after, "protected_chunk_admissions") > n(&before, "protected_chunk_admissions"));
    check_budget(&after);
    service(
        root,
        "pressure-plan-disarm",
        "test/first-read-arm",
        Some(&json!({"mode":""})),
    );
    fs::write(root.join("plan-pressure-contract.json"),serde_json::to_vec_pretty(&json!({"kind":"actual cache eviction between authenticated first-missing plan and continuation, paid discard then ordinary safe reconstruction","big":big,"refill":refill,"before":before,"response":response,"after":after,"trace":trace,"full_bill_usd":null})).unwrap()).unwrap();
}

#[test]
#[ignore = "bounded fresh Native controls from an explicitly copied completed fixture; owns 8845/8846"]
fn multi_chunk_copied_controls_gate() {
    let root = PathBuf::from(std::env::var("GIT_ZERO_MULTI_ARTIFACTS").unwrap());
    assert!(!root.exists());
    fs::create_dir_all(&root).unwrap();
    record_gate_context(&root);
    let base = PathBuf::from(std::env::var("GIT_ZERO_MULTI_BASE").unwrap());
    let cohort = std::env::var("GIT_ZERO_MULTI_COHORT").unwrap();
    assert!(matches!(
        cohort.as_str(),
        "faults" | "plan" | "warm" | "http" | "restart"
    ));
    let before = sources();
    fs::write(
        root.join("source-before.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    copy_tree(&base.join("persist"), &root.join("persist"));
    copy_tree(&base.join("source"), &root.join("source"));
    for port in [8845, 8846] {
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).expect("owned port free"));
    }
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let wrapped = matches!(cohort.as_str(), "faults" | "plan");
    let cfg = if wrapped {
        fs::read_to_string(worker.join("test/socket-rpc.toml"))
            .unwrap()
            .replace(
                r#"main = "socket-rpc-worker.js""#,
                &format!("main = {:?}", worker.join("test/socket-rpc-worker.js")),
            )
    } else {
        fs::read_to_string(worker.join("wrangler.toml"))
            .unwrap()
            .replace(
                r#"main = "src/index.js""#,
                &format!("main = {:?}", worker.join("src/index.js")),
            )
    };
    fs::write(root.join("wrangler.toml"), cfg).unwrap();
    let cli = std::env::var("GIT_ZERO_MULTI_WRANGLER").unwrap();
    let cfg = root.join("wrangler.toml");
    let persist = root.join("persist");
    let mut runtime = spawn(
        &root,
        "node",
        &[
            &cli,
            "dev",
            "--config",
            cfg.to_str().unwrap(),
            "--ip",
            "127.0.0.1",
            "--port",
            "8845",
            "--local",
            "--persist-to",
            persist.to_str().unwrap(),
            "--show-interactive-dev-session=false",
        ],
        "wrangler.log",
    );
    wait(
        &root,
        &mut runtime,
        "wrangler.log",
        "Ready on http://127.0.0.1:8845",
    );
    let view = if cohort == "warm" { "warm" } else { "cold" };
    let feature = if cohort == "http" { "off" } else { "on" };
    let native_root = root.join("native");
    let mut native = spawn(
        &root,
        env!("CARGO_BIN_EXE_repository-host"),
        &[
            "--cloudflare",
            "http://127.0.0.1:8845",
            "--repo",
            "protected-gate",
            "--view-cache",
            view,
            "--gateway-first-read",
            feature,
            "--socket-rpc",
            feature,
            "--protected-chunk",
            &std::env::var("GIT_ZERO_MULTI_MODE").unwrap_or_else(|_| "joint".into()),
            native_root.to_str().unwrap(),
            "8846",
        ],
        "host.log",
    );
    wait(&root, &mut native, "host.log", "Git gateway:");
    let gateway = "http://127.0.0.1:8845/protected-gate/repo.git";
    let source = root.join("source");
    let client = root.join("client.git");
    run(
        "git",
        &["init", "-q", "--bare", client.to_str().unwrap()],
        &[],
    );
    let initial = metrics(&root, "copied-startup");
    assert_eq!(n(&initial, "protected_chunk_admissions"), 0);
    assert_eq!(n(&initial, "protected_chunk_entries"), 0);
    let manifest = service(&root, "copied-current-manifest", "manifest", None);
    let current = manifest["refs"]["refs/heads/main"]
        .as_str()
        .unwrap()
        .to_string();
    git(&source, &["reset", "--hard", &current], &[]);
    let first = text(&source, &["rev-list", "--max-parents=0", "HEAD"]);
    let mut rows = Vec::new();
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "copied-ordinary-bootstrap",
        &current,
        None,
    ));
    let observer_before = metrics(&root, "observer-before");
    let _ = request(
        &root,
        "observer-object-stats",
        "http://127.0.0.1:8846/_object-stats",
        None,
    );
    let observer_after = metrics(&root, "observer-after");
    for key in ["protected_chunk_admissions", "protected_chunk_prunes"] {
        assert_eq!(
            n(&observer_before, key),
            n(&observer_after, key),
            "observer cannot change pool {key}"
        );
    }
    if cohort == "faults" {
        protected_faults(&root, &source, &client, gateway);
    } else if cohort == "plan" {
        protected_plan_pressure(&root, &source, &client, gateway, &first);
    } else if cohort == "restart" {
        assert!(n(&observer_after, "protected_chunk_admissions") > 1);
        drop(native);
        native = spawn(
            &root,
            env!("CARGO_BIN_EXE_repository-host"),
            &[
                "--cloudflare",
                "http://127.0.0.1:8845",
                "--repo",
                "protected-gate",
                "--view-cache",
                "cold",
                "--gateway-first-read",
                "on",
                "--socket-rpc",
                "on",
                "--protected-chunk",
                &std::env::var("GIT_ZERO_MULTI_MODE").unwrap_or_else(|_| "joint".into()),
                native_root.to_str().unwrap(),
                "8846",
            ],
            "host-restarted.log",
        );
        wait(&root, &mut native, "host-restarted.log", "Git gateway:");
        let restarted = metrics(&root, "same-root-restarted");
        assert_eq!(n(&restarted, "protected_chunk_admissions"), 0);
        assert_eq!(n(&restarted, "protected_chunk_entries"), 0);
        fs::write(
            source.join("restart-receive"),
            b"real new receive after same-root Native restart\n",
        )
        .unwrap();
        git(&source, &["add", "."], &[]);
        let tip = commit_on(&source, &current, 300);
        let payload = receive_body(&source, &current, &tip, false);
        let (status, body) = fault_http(
            &root,
            "restarted-prepared-receive",
            &format!("{gateway}/git-receive-pack"),
            &payload,
            true,
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        assert!(String::from_utf8_lossy(&body).contains("ok refs/heads/main"));
        let after = metrics(&root, "restarted-receive-after");
        check_budget(&after);
        assert!(n(&after, "gateway_receive_consumed") > n(&restarted, "gateway_receive_consumed"));
        assert!(n(&after, "protected_chunk_admissions") > 0);
        assert!(usage(&after, "r2_class_b") > usage(&restarted, "r2_class_b"));
        rows.push(json!({"name":"same-root-restarted-prepared-receive","before":restarted,"after":after,"request_bytes":payload.len(),"response_bytes":body.len(),"response_sha256":hash(&body)}));
        rows.push(read(
            &root,
            &source,
            &client,
            gateway,
            "restarted-latest",
            &tip,
            Some(&current),
        ));
        rows.push(read(
            &root,
            &source,
            &client,
            gateway,
            "restarted-history",
            &first,
            None,
        ));
    } else {
        let mut parent = current.clone();
        for serial in 0..3 {
            fs::write(source.join("compatibility"), format!("ordinary {serial}\n")).unwrap();
            git(&source, &["add", "."], &[]);
            let tip = commit_on(&source, &parent, 200 + serial);
            let payload = receive_body(&source, &parent, &tip, false);
            let before = metrics(&root, &format!("compat-push-{serial}-before"));
            let (status, body) = fault_http(
                &root,
                &format!("compat-push-{serial}"),
                &format!("{gateway}/git-receive-pack"),
                &payload,
                true,
            );
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            assert!(String::from_utf8_lossy(&body).contains("ok refs/heads/main"));
            let after = metrics(&root, &format!("compat-push-{serial}-after"));
            check_budget(&after);
            rows.push(
                json!({"name":format!("compat-push-{serial}"),"before":before,"after":after}),
            );
            rows.push(read(
                &root,
                &source,
                &client,
                gateway,
                &format!("compat-latest-{serial}"),
                &tip,
                Some(&parent),
            ));
            rows.push(read(
                &root,
                &source,
                &client,
                gateway,
                &format!("compat-history-{serial}"),
                &first,
                None,
            ));
            parent = tip;
        }
    }
    let final_metrics = metrics(&root, "copied-final");
    if cohort == "faults" {
        fault_bounds(&final_metrics);
    } else {
        check_budget(&final_metrics);
    }
    if cohort == "warm" {
        assert_eq!(n(&final_metrics, "protected_chunk_admissions"), 0);
        assert_eq!(
            usage(&final_metrics, "r2_class_b"),
            usage(&initial, "r2_class_b")
        );
        assert_eq!(
            usage(&final_metrics, "r2_read_bytes"),
            usage(&initial, "r2_read_bytes")
        );
    }
    if cohort == "http" {
        assert_eq!(n(&final_metrics, "gateway_read_consumed"), 0);
        assert_eq!(n(&final_metrics, "gateway_receive_consumed"), 0);
        assert!(n(&final_metrics, "protected_chunk_admissions") > 1);
        assert!(n(&final_metrics, "protected_chunk_hits") > 0);
    }
    let after = sources();
    assert_eq!(before, after);
    fs::write(
        root.join("source-after.json"),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    fs::write(root.join("contract.json"),serde_json::to_vec_pretty(&json!({"kind":"bounded actual production local correctness controls, copied fixture, fresh Native","cohort":cohort,"copied_fixture":base,"initial":initial,"rows":rows,"observer_before":observer_before,"observer_after":observer_after,"final":final_metrics,"full_bill_usd":null})).unwrap()).unwrap();
    drop(native);
    drop(runtime);
    fs::write(
        root.join("cleanup.json"),
        "{\"owned_ports\":[8845,8846],\"owned_process_groups_stopped\":true}\n",
    )
    .unwrap();
}

fn seal_fixture(root: &Path) -> BTreeMap<String, Value> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<String, Value>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let key = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if metadata.is_symlink() {
            out.insert(key, json!({"symlink":fs::read_link(path).unwrap()}));
        } else if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), out);
            }
        } else {
            assert!(metadata.is_file(), "unsupported fixture entry {path:?}");
            let bytes = fs::read(path).unwrap();
            out.insert(key, json!({"bytes":bytes.len(),"sha256":hash(&bytes)}));
        }
    }
    let mut out = BTreeMap::new();
    for name in ["source", "persist"] {
        visit(root, &root.join(name), &mut out);
    }
    out
}

#[test]
#[ignore = "explicit same-source Multi/Joint production pair; owns 8845/8846"]
fn joint_chunk_copied_production_gate() {
    let root = PathBuf::from(std::env::var("GIT_ZERO_MULTI_ARTIFACTS").unwrap());
    assert!(!root.exists(), "preserve every previous attempt");
    fs::create_dir_all(&root).unwrap();
    record_gate_context(&root);
    let base = PathBuf::from(std::env::var("GIT_ZERO_MULTI_BASE").unwrap());
    let mode = std::env::var("GIT_ZERO_MULTI_MODE").unwrap();
    assert!(matches!(mode.as_str(), "multi" | "joint"));
    let before = sources();
    fs::write(
        root.join("source-before.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    let seed = seal_fixture(&base);
    fs::write(
        root.join("fixture-input-seal.json"),
        serde_json::to_vec_pretty(&seed).unwrap(),
    )
    .unwrap();
    copy_tree(&base.join("persist"), &root.join("persist"));
    copy_tree(&base.join("source"), &root.join("source"));
    let copied = seal_fixture(&root);
    assert_eq!(seed, copied, "durable R2/SQL and source copy must be exact");
    assert_eq!(
        seed,
        seal_fixture(&base),
        "input must remain stopped and unchanged while copying"
    );
    fs::write(
        root.join("fixture-copy-seal.json"),
        serde_json::to_vec_pretty(&copied).unwrap(),
    )
    .unwrap();
    for port in [8845, 8846] {
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).expect("owned port free"));
    }
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let config = fs::read_to_string(worker.join("wrangler.toml"))
        .unwrap()
        .replace(
            "main = \"src/index.js\"",
            &format!("main = {:?}", worker.join("src/index.js")),
        );
    fs::write(root.join("wrangler.toml"), config).unwrap();
    let cli = std::env::var("GIT_ZERO_MULTI_WRANGLER").unwrap();
    let mut runtime = spawn(
        &root,
        "node",
        &[
            &cli,
            "dev",
            "--config",
            root.join("wrangler.toml").to_str().unwrap(),
            "--ip",
            "127.0.0.1",
            "--port",
            "8845",
            "--local",
            "--persist-to",
            root.join("persist").to_str().unwrap(),
            "--show-interactive-dev-session=false",
        ],
        "wrangler.log",
    );
    wait(
        &root,
        &mut runtime,
        "wrangler.log",
        "Ready on http://127.0.0.1:8845",
    );
    let mut native = spawn(
        &root,
        env!("CARGO_BIN_EXE_repository-host"),
        &[
            "--cloudflare",
            "http://127.0.0.1:8845",
            "--repo",
            "protected-gate",
            "--view-cache",
            "cold",
            "--gateway-first-read",
            "on",
            "--socket-rpc",
            "on",
            "--protected-chunk",
            &mode,
            root.join("native").to_str().unwrap(),
            "8846",
        ],
        "host.log",
    );
    wait(&root, &mut native, "host.log", "Git gateway:");
    let startup = metrics(&root, "startup-paid-recovery");
    check_budget(&startup);
    assert_eq!(n(&startup, "protected_chunk_admissions"), 0);
    assert_eq!(n(&startup, "protected_chunk_entries"), 0);
    assert!(
        usage(&startup, "r2_class_b") > 0,
        "copied cold recovery is paid, never prewarm"
    );
    let input_manifest = service(&root, "input-manifest", "manifest", None);
    let input_stats: Value = serde_json::from_slice(&request(
        &root,
        "input-object-stats",
        "http://127.0.0.1:8846/_object-stats",
        None,
    ))
    .unwrap();
    fs::write(
        root.join("input-logical.json"),
        serde_json::to_vec_pretty(&json!({"manifest":input_manifest,"stats":input_stats})).unwrap(),
    )
    .unwrap();
    let source = root.join("source");
    let client = root.join("client.git");
    run(
        "git",
        &["init", "-q", "--bare", client.to_str().unwrap()],
        &[],
    );
    let current = input_manifest["refs"]["refs/heads/main"]
        .as_str()
        .unwrap()
        .to_owned();
    git(&source, &["reset", "--hard", &current], &[]);
    let first = text(&source, &["rev-list", "--max-parents=0", "HEAD"]);
    let history: Vec<String> = input_manifest["refs"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| name.starts_with("refs/heads/history-"))
        .map(|(_, id)| id.as_str().unwrap().to_owned())
        .collect();
    assert!(history.len() >= 8);
    let chosen: Vec<_> = (0..9)
        .map(|round| history[(round * 7 + 3) % history.len()].clone())
        .collect();
    fs::write(root.join("closed-history-sequence.json"), serde_json::to_vec_pretty(&json!({"input_current":current,"input_first":first,"published_history":history,"chosen":chosen,"selection_scope":"saved request sequence; source-derived manifest descriptor order, not an observed ordered GET timeline"})).unwrap()).unwrap();
    let gateway = "http://127.0.0.1:8845/protected-gate/repo.git";
    let mut rows = Vec::new();
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "copied-first-full",
        &current,
        None,
    ));
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "copied-full-recheck",
        &current,
        None,
    ));
    for (round, historical) in chosen.iter().enumerate() {
        let generation = 17 + round;
        if round % 4 == 0 {
            fs::write(
                source.join("replacement"),
                fixture(8 * 1024 * 1024, 0x88800 + generation as u64),
            )
            .unwrap();
        }
        fs::write(
            source.join("joint-change"),
            format!("ordinary generation {generation}\n").repeat(100),
        )
        .unwrap();
        let want = commit(&source, generation);
        let before = metrics(&root, &format!("push-{generation}-before"));
        push(&source, gateway, generation);
        let after = metrics(&root, &format!("push-{generation}-after"));
        check_budget(&after);
        rows.push(json!({"name":format!("push-{generation}"),"before":before,"after":after}));
        let latest = read(
            &root,
            &source,
            &client,
            gateway,
            &format!("latest-{generation}"),
            &want,
            Some(&first),
        );
        assert!(
            latest["gets"].as_u64().unwrap() > 0 && latest["read_bytes"].as_u64().unwrap() > 0,
            "new ordinary output naturally requires actual GETs"
        );
        rows.push(latest);
        rows.push(read(
            &root,
            &source,
            &client,
            gateway,
            &format!("history-nohave-{generation}"),
            historical,
            None,
        ));
        git(
            &client,
            &["cat-file", "-e", &format!("{first}^{{commit}}")],
            &[],
        );
        rows.push(read(
            &root,
            &source,
            &client,
            gateway,
            &format!("history-seeded-{generation}"),
            historical,
            Some(&first),
        ));
        git(
            &client,
            &["cat-file", "-e", &format!("{historical}^{{commit}}")],
            &[],
        );
        rows.push(read(
            &root,
            &source,
            &client,
            gateway,
            &format!("latest-recheck-{generation}"),
            &want,
            Some(historical),
        ));
        let published = service(&root, &format!("published-{generation}"), "manifest", None);
        assert_eq!(published["refs"]["refs/heads/main"], want);
    }
    let (tip, thin) = external_thin(&root, &source, gateway, true);
    rows.push(thin);
    rows.push(read(
        &root,
        &source,
        &client,
        gateway,
        "thin-latest",
        &tip,
        Some(&first),
    ));
    let final_manifest = service(&root, "final-manifest", "manifest", None);
    let final_metrics = metrics(&root, "final-after-manifest-observer");
    check_budget(&final_metrics);
    let after = sources();
    assert_eq!(before, after, "one frozen source/binary for both policies");
    fs::write(
        root.join("source-after.json"),
        serde_json::to_vec_pretty(&after).unwrap(),
    )
    .unwrap();
    fs::write(root.join("contract.json"), serde_json::to_vec_pretty(&json!({"kind":"same-source Multi/Joint actual production correctness/accounting, not benchmark","mode":mode,"copied_fixture":base,"startup_paid_recovery":startup,"input_manifest":input_manifest,"rows":rows,"final":final_metrics,"final_manifest":final_manifest,"full_bill_usd":null})).unwrap()).unwrap();
    drop(native);
    drop(runtime);
    fs::write(
        root.join("cleanup.json"),
        "{\"owned_ports\":[8845,8846],\"owned_process_groups_stopped\":true}\n",
    )
    .unwrap();
}
