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
    let mut args = vec![
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        "120",
        "--http1.1",
        "--dump-header",
    ];
    let header = root.join(format!("{name}.headers"));
    args.push(header.to_str().unwrap());
    args.push(url);
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
    let bytes = run("curl", &args, &[]);
    fs::write(root.join(format!("{name}.body")), &bytes).unwrap();
    bytes
}
fn metrics(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&request(root, name, "http://127.0.0.1:8842/metrics", None)).unwrap()
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
        "tests/protected_chunk_cloudflare.rs",
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
    out
}

#[test]
#[ignore = "explicit Rust-owned local production-workerd gate on ports 8841/8842"]
fn protected_chunk_production_gate() {
    let root =
        PathBuf::from(std::env::var("GIT_ZERO_PROTECTED_ARTIFACTS").expect("fresh artifact root"));
    assert!(
        !root.exists(),
        "retain previous attempts; use a new artifact root"
    );
    fs::create_dir_all(&root).unwrap();
    let before = sources();
    fs::write(
        root.join("source-before.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    let view = std::env::var("GIT_ZERO_PROTECTED_VIEW").unwrap_or_else(|_| "cold".into());
    let first_read = std::env::var("GIT_ZERO_PROTECTED_FIRST_READ").unwrap_or_else(|_| "on".into());
    let socket = std::env::var("GIT_ZERO_PROTECTED_SOCKET").unwrap_or_else(|_| "on".into());
    let mode = std::env::var("GIT_ZERO_PROTECTED_MODE").unwrap_or_else(|_| "on".into());
    assert!(mode == "on" || mode == "off");
    let initial = std::env::var("GIT_ZERO_PROTECTED_INITIAL_MIB")
        .unwrap_or_else(|_| "16".into())
        .parse::<usize>()
        .unwrap();
    assert!(initial == 16 || initial == 64);
    for port in [8841, 8842] {
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).expect("owned port free"));
    }
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let faults = std::env::var_os("GIT_ZERO_PROTECTED_FAULTS").is_some();
    let pressure = std::env::var_os("GIT_ZERO_PROTECTED_PLAN_PRESSURE").is_some();
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
    let cli = std::env::var("GIT_ZERO_PROTECTED_WRANGLER").unwrap_or_else(|_| {
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
            "8841",
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
        "Ready on http://127.0.0.1:8841",
    );
    let native_root = root.join("native");
    let mut native = spawn(
        &root,
        env!("CARGO_BIN_EXE_repository-host"),
        &[
            "--cloudflare",
            "http://127.0.0.1:8841",
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
            "8842",
        ],
        "host.log",
    );
    wait(&root, &mut native, "host.log", "Git gateway:");
    let gateway = "http://127.0.0.1:8841/protected-gate/repo.git";
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
    if mode == "on" && view == "cold" {
        assert_eq!(n(&admitted, "protected_chunk_admissions"), 1);
        assert!(n(&admitted, "protected_chunk_bytes") > 8 * 1024 * 1024);
        if initial == 64 {
            assert_eq!(n(&admitted, "protected_chunk_bytes"), 16 * 1024 * 1024);
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
    if mode == "on" && initial == 16 && view == "cold" {
        assert_eq!(
            rows.last().unwrap()["gets"],
            0,
            "all-cache ordinary fallback"
        );
    }
    let restart_before_receive = std::env::var_os("GIT_ZERO_PROTECTED_RESTART_RECEIVE").is_some();
    if restart_before_receive {
        drop(native);
        native = spawn(
            &root,
            env!("CARGO_BIN_EXE_repository-host"),
            &[
                "--cloudflare",
                "http://127.0.0.1:8841",
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
                "8842",
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
    for generation in 2..=8 {
        if generation == 5 {
            fs::write(
                source.join("replacement"),
                fixture(8 * 1024 * 1024, 0x77700),
            )
            .unwrap();
        }
        fs::write(
            source.join("change"),
            format!("generation {generation}\n").repeat(100),
        )
        .unwrap();
        let want = commit(&source, generation);
        let before = metrics(&root, &format!("push-{generation}-before"));
        push(&source, gateway, generation);
        let after = metrics(&root, &format!("push-{generation}-after"));
        check_budget(&after);
        if restart_before_receive && generation == 2 && mode == "on" {
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
        if generation == 4 || generation == 8 {
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
    if mode == "on" && !faults && !pressure && view == "cold" {
        assert!(n(&final_metrics, "protected_chunk_hits") > 10);
        assert_eq!(n(&final_metrics, "protected_chunk_admissions"), 1);
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
            &json!({"owned_ports":[8841,8842],"owned_process_groups_stopped":true}),
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
        if item.path().is_dir() {
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
#[ignore = "explicit local production-workerd allocation/pressure gate; owns 8841"]
fn protected_chunk_allocation_pressure_gate() {
    use std::sync::atomic::Ordering::SeqCst;
    let root =
        PathBuf::from(std::env::var("GIT_ZERO_PROTECTED_ARTIFACTS").expect("new artifact root"));
    assert!(!root.exists());
    fs::create_dir_all(&root).unwrap();
    let base = PathBuf::from(
        std::env::var("GIT_ZERO_PROTECTED_BASE").expect("completed immutable 64 MiB gate fixture"),
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
    drop(std::net::TcpListener::bind("127.0.0.1:8841").unwrap());
    let cli = std::env::var("GIT_ZERO_PROTECTED_WRANGLER").unwrap();
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
            "8841",
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
        "Ready on http://127.0.0.1:8841",
    );
    let mut host = git_zero_host::native::NativeHost::open_cloudflare(
        root.join("codec"),
        "http://127.0.0.1:8841",
        "protected-gate",
        "git-zero-local-services",
        true,
    )
    .unwrap();
    let services = host.cloudflare_services().unwrap();
    services.enable_socket_rpc(true);
    services.register("http://127.0.0.1:8842").unwrap();
    drop(services);
    let initial = direct_metrics(&mut host);
    assert_eq!(
        n(&initial, "protected_chunk_admissions"),
        0,
        "actual startup GETs cannot admit"
    );
    let source = root.join("source");
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
    assert_eq!(n(&admitted, "protected_chunk_bytes"), 16 * 1024 * 1024);
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
    assert_eq!(n(&post_push, "protected_chunk_bytes"), 16 * 1024 * 1024);
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
        "http://127.0.0.1:8841",
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
        "{\"owned_process_groups_stopped\":true,\"owned_port\":8841}\n",
    )
    .unwrap();
}
fn service(root: &Path, name: &str, route: &str, body: Option<&Value>) -> Value {
    let url = format!("http://127.0.0.1:8841/_services/{route}");
    let mut args = vec![
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        "120",
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
    let bytes = run("curl", &args, &[]);
    fs::write(root.join(format!("{name}.body")), &bytes).unwrap();
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
fn protected_faults(root: &Path, source: &Path, client: &Path, gateway: &str) {
    let mut rows = Vec::new();
    let mut serial = 100;
    // A true external-base thin pack, completed using the cached+remote Full
    // snapshot after a historical projection. No benchmark fixture is changed.
    let manifest = service(root, "thin-manifest", "manifest", None);
    let parent = manifest["refs"]["refs/heads/main"].as_str().unwrap();
    let mut base = fs::read(source.join("base")).unwrap();
    base[1] ^= 1;
    fs::write(source.join("base"), base).unwrap();
    git(source, &["add", "base"], &[]);
    let tip = commit_on(source, parent, serial);
    serial += 1;
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
    assert!(n(&after, "gateway_receive_consumed") > n(&before, "gateway_receive_consumed"));
    assert!(n(&after, "protected_chunk_hits") > n(&before, "protected_chunk_hits"));
    rows.push(json!({"kind":"actual external-base thin prepared Full receive","before":before,"after":after,"request_bytes":thin.len(),"response_sha256":hash(&body)}));
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
            &fetch_body(&tip, None, &format!("fault-{mode}")),
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
            &fetch_body(&tip, None, &format!("healthy-{mode}")),
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
    assert_eq!(refill["after"]["protected_chunk_bytes"], 16 * 1024 * 1024);
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
        &old,
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
