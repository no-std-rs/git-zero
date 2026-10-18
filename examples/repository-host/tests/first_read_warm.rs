//! Explicit production Worker + registered Native warm-flow correctness check.
//! This driver produces receipts, not benchmark samples or performance claims.
use git_zero_host::native::command;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn configured(program: &str) -> Command {
    let mut process = Command::new(program);
    process
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z");
    process
}

fn run(program: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = configured(program);
    process.args(args);
    command(process, input)
        .unwrap_or_else(|error| panic!("{program} {args:?}: {error}"))
        .stdout
}

fn git(repo: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut all = vec!["-C", repo.to_str().unwrap()];
    all.extend(args);
    run("git", &all, input)
}

fn text(repo: &Path, args: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(repo, args, input))
        .unwrap()
        .trim()
        .into()
}

fn http(root: &Path, name: &str, url: &str, body: Option<&[u8]>) -> Vec<u8> {
    let headers = root.join(format!("{name}.headers"));
    let input = root.join(format!("{name}.request"));
    let mut args = vec![
        "--silent",
        "--show-error",
        "--fail-with-body",
        "--max-time",
        "120",
        "--http1.1",
        "--dump-header",
        headers.to_str().unwrap(),
        url,
    ];
    let data;
    if let Some(body) = body {
        fs::write(&input, body).unwrap();
        data = format!("@{}", input.display());
        args.extend([
            "-H",
            "Content-Type: application/x-git-upload-pack-request",
            "--data-binary",
            &data,
        ]);
    }
    let result = run("curl", &args, &[]);
    fs::write(root.join(format!("{name}.body")), &result).unwrap();
    result
}

fn metrics(root: &Path, name: &str, native: &str, stats: bool) -> Value {
    let route = if stats { "_object-stats" } else { "metrics" };
    serde_json::from_slice(&http(root, name, &format!("{native}/{route}"), None)).unwrap()
}

fn n(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("missing {field}: {value}"))
}

fn assert_warm_read(before: &Value, after: &Value) {
    for field in [
        "r2_class_b",
        "r2_deletes",
        "r2_read_bytes",
        "r2_written_bytes",
        "r2_live_objects",
        "r2_live_bytes",
        "r2_peak_bytes",
        "do_sqlite_bytes",
        "do_rows_written",
    ] {
        assert_eq!(
            n(&before["cloudflare_services"], field),
            n(&after["cloudflare_services"], field),
            "{field}"
        );
    }
    for value in [before, after] {
        let services = &value["cloudflare_services"];
        assert_eq!(services["accounting_complete"], true);
        assert_eq!(services["accounting_errors"], json!([]));
        // Metrics observers perform their own paid LIST, retained in the raw
        // receipt. Fetch traffic itself must perform no Class A operation.
        if !services["by_phase"]["fetch"].is_null() {
            assert_eq!(n(&services["by_phase"]["fetch"], "r2_class_a"), 0);
        }
        for field in [
            "gateway_read_plans",
            "gateway_read_consumed",
            "gateway_read_unsupported",
            "gateway_read_errors",
            "gateway_read_generation_changed",
            "gateway_read_paid_discards",
        ] {
            assert_eq!(
                n(value, field),
                0,
                "warm traffic must not negotiate private RPC: {field}"
            );
        }
        assert_eq!(value["native_view"]["scope"], "full");
        assert!(n(value, "cache_retained_bytes") <= 32 * 1024 * 1024);
    }
}

struct OwnedProcess(Child);

impl OwnedProcess {
    fn group_matches(&self) -> bool {
        Command::new("ps")
            .args(["-o", "pid=,pgid=,sid=", "-p", &self.0.id().to_string()])
            .output()
            .ok()
            .is_some_and(|output| {
                let ids: Vec<_> = String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .filter_map(|id| id.parse::<u32>().ok())
                    .collect();
                ids == [self.0.id(); 3]
            })
    }

    fn start(mut process: Command) -> Self {
        let mut owned = Self(process.spawn().unwrap());
        for _ in 0..100 {
            assert!(
                owned.0.try_wait().unwrap().is_none(),
                "owned process exited at startup"
            );
            if owned.group_matches() {
                return owned;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = owned.0.kill();
        let _ = owned.0.wait();
        panic!("setsid child must own its PID, process group, and session");
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() || !self.group_matches() {
            return;
        }
        let group = format!("-{}", self.0.id());
        let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
        for _ in 0..50 {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.group_matches() {
            let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
        }
        let _ = self.0.wait();
    }
}

fn start_worker(root: &Path, origin: &str, reservation: TcpListener) -> OwnedProcess {
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let config = fs::read_to_string(worker.join("wrangler.toml"))
        .unwrap()
        .replace(
            "main = \"src/index.js\"",
            &format!("main = {:?}", worker.join("src/index.js")),
        );
    let config_path = root.join("wrangler.toml");
    fs::write(&config_path, config).unwrap();
    let cli = std::env::var_os("GIT_ZERO_FIRST_READ_WARM_WRANGLER")
        .map(PathBuf::from)
        .unwrap_or_else(|| worker.join("node_modules/wrangler/bin/wrangler.js"));
    let log_path = root.join("wrangler.log");
    let log = File::create(&log_path).unwrap();
    let mut process = configured("setsid");
    process
        .arg("node")
        .arg(cli)
        .args(["dev", "--config"])
        .arg(config_path)
        .args([
            "--ip",
            "127.0.0.1",
            "--port",
            "8835",
            "--local",
            "--persist-to",
        ])
        .arg(root.join("persist"))
        .arg("--show-interactive-dev-session=false")
        .current_dir(root)
        .env("WRANGLER_SEND_METRICS", "false")
        .env("X_LOCAL_EXPLORER", "false")
        .env("X_LOCAL_OBSERVABILITY", "false")
        .env("BROWSER", "none")
        .env("WRANGLER_LOG_PATH", root.join("logs"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    drop(reservation);
    let mut owned = OwnedProcess::start(process);
    for _ in 0..200 {
        assert!(
            owned.0.try_wait().unwrap().is_none(),
            "Worker exited; inspect {}",
            log_path.display()
        );
        if fs::read_to_string(&log_path)
            .unwrap()
            .contains(&format!("Ready on {origin}"))
        {
            return owned;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("Worker startup timed out; inspect {}", log_path.display());
}

fn commit(source: &Path, parent: Option<&str>, blob: &str, revision: usize) -> String {
    let revision_blob = text(
        source,
        &["hash-object", "-w", "--stdin"],
        format!("revision {revision}\n").as_bytes(),
    );
    let tree = text(
        source,
        &["mktree"],
        format!("100644 blob {blob}\tpayload\n100644 blob {revision_blob}\trevision\n").as_bytes(),
    );
    let mut args = vec![
        "-c",
        "user.name=Warm contract",
        "-c",
        "user.email=warm@example.invalid",
        "commit-tree",
        &tree,
    ];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    text(source, &args, format!("revision {revision}\n").as_bytes())
}

fn fetch_checked(
    root: &Path,
    source: &Path,
    client: &Path,
    gateway: &str,
    snapshot: &str,
    wanted: &str,
    seed: Option<&str>,
) {
    if !client.exists() {
        fs::create_dir(client).unwrap();
        git(
            client,
            &["init", "--bare", "--quiet", "--initial-branch=main"],
            &[],
        );
        git(client, &["config", "fetch.unpackLimit", "1"], &[]);
        git(client, &["config", "gc.auto", "0"], &[]);
    }
    if let Some(seed) = seed {
        git(
            client,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--keep",
                source.to_str().unwrap(),
                &format!("{seed}:refs/heads/seed"),
            ],
            &[],
        );
    }
    let trace = root.join(format!(
        "{}.packet-trace",
        client.file_name().unwrap().to_str().unwrap()
    ));
    let mut process = configured("git");
    process
        .args([
            "-C",
            client.to_str().unwrap(),
            "-c",
            "protocol.version=0",
            "fetch",
            "--quiet",
            "--no-tags",
            "--keep",
            gateway,
            &format!("{snapshot}:refs/heads/result"),
        ])
        .env("GIT_TRACE_PACKET", &trace);
    command(process, &[]).unwrap();
    if let Some(seed) = seed {
        assert!(
            fs::read_to_string(&trace)
                .unwrap()
                .contains(&format!("have {seed}")),
            "seeded real Git client must negotiate its historical have"
        );
    }
    assert_eq!(
        text(client, &["rev-parse", "refs/heads/result"], &[]),
        wanted
    );
    git(client, &["fsck", "--strict", "--full"], &[]);
    assert_eq!(
        git(source, &["archive", "--format=tar", wanted], &[]),
        git(client, &["archive", "--format=tar", wanted], &[])
    );
}

#[test]
#[ignore = "owns fresh Worker 8835 and Native 8836; requires GIT_ZERO_FIRST_READ_WARM_ORIGIN and GIT_ZERO_FIRST_READ_WARM_ARTIFACTS"]
fn production_gateway_first_read_warm_contract() {
    let origin = std::env::var("GIT_ZERO_FIRST_READ_WARM_ORIGIN").expect("explicit local origin");
    assert_eq!(origin, "http://127.0.0.1:8835");
    let root = PathBuf::from(
        std::env::var("GIT_ZERO_FIRST_READ_WARM_ARTIFACTS").expect("explicit fresh artifact path"),
    );
    fs::create_dir(&root).expect("artifact directory must be fresh");
    let worker_port = TcpListener::bind("127.0.0.1:8835").expect("owned Worker port must be free");
    let native_port = TcpListener::bind("127.0.0.1:8836").expect("owned Native port must be free");
    let _worker = start_worker(&root, &origin, worker_port);
    let stdout = root.join("native.stdout");
    let mut process = configured("setsid");
    // Leave gateway-first-read at its default on: warm startup must not negotiate it.
    process
        .arg(env!("CARGO_BIN_EXE_repository-host"))
        .args([
            "--cloudflare",
            &origin,
            "--repo",
            "demo",
            "--view-cache",
            "warm",
        ])
        .arg(root.join("host"))
        .arg("8836")
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(root.join("native.stderr")).unwrap());
    drop(native_port);
    let mut native_process = OwnedProcess::start(process);
    let native = (0..200)
        .find_map(|_| {
            assert!(
                native_process.0.try_wait().unwrap().is_none(),
                "Native exited"
            );
            let found = fs::read_to_string(&stdout)
                .unwrap()
                .lines()
                .find(|line| line.starts_with("http://"))
                .map(|line| line.trim_end_matches("/repo.git").to_owned());
            if found.is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
            found
        })
        .expect("Native ready within 20 seconds");
    let source = root.join("source.git");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--initial-branch=main"],
        &[],
    );
    let mut state = 0x1234_5678_u64;
    let payload: Vec<u8> = (0..1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let blob = text(&source, &["hash-object", "-w", "--stdin"], &payload);
    let gateway = format!("{origin}/demo/repo.git");
    let mut tips = Vec::<String>::new();
    let mut reads = Vec::new();
    for revision in 1..=3 {
        let tip = commit(&source, tips.last().map(String::as_str), &blob, revision);
        git(&source, &["update-ref", "refs/heads/main", &tip], &[]);
        git(
            &source,
            &[
                "-c",
                "protocol.version=0",
                "push",
                "--atomic",
                &gateway,
                "refs/heads/main:refs/heads/main",
                &format!("refs/heads/main:refs/heads/snapshot{revision:02}"),
            ],
            &[],
        );
        let before = metrics(
            &root,
            &format!("push-{revision}-before-fetch"),
            &native,
            false,
        );
        assert_eq!(
            n(&before, "cache_bytes"),
            0,
            "push must invalidate response cache"
        );
        fetch_checked(
            &root,
            &source,
            &root.join("latest.git"),
            &gateway,
            "refs/heads/main",
            &tip,
            None,
        );
        let after = metrics(
            &root,
            &format!("push-{revision}-after-fetch"),
            &native,
            false,
        );
        assert_warm_read(&before, &after);
        reads.push(json!({"name":format!("push-{revision}-latest"), "before":before, "after":after, "want":tip}));
        tips.push(tip);
    }
    let before_history = metrics(&root, "before-history", &native, false);
    for (name, snapshot, wanted, seed) in [
        (
            "history-fresh.git",
            "refs/heads/snapshot01",
            tips[0].as_str(),
            None,
        ),
        (
            "history-seeded.git",
            "refs/heads/snapshot02",
            tips[1].as_str(),
            Some(tips[0].as_str()),
        ),
    ] {
        let before = metrics(&root, &format!("{name}-before"), &native, false);
        fetch_checked(
            &root,
            &source,
            &root.join(name),
            &gateway,
            snapshot,
            wanted,
            seed,
        );
        let after = metrics(&root, &format!("{name}-after"), &native, false);
        assert_warm_read(&before, &after);
        reads.push(json!({"name":name,"want":wanted,"have":seed,"before":before,"after":after}));
    }
    let line = format!("want {} ofs-delta no-progress\n", tips[2]);
    let mut request = format!("{:04x}{line}", line.len() + 4).into_bytes();
    request.extend_from_slice(b"00000009done\n");
    let response = http(
        &root,
        "cache-fill",
        &format!("{gateway}/git-upload-pack"),
        Some(&request),
    );
    assert!(response.windows(4).any(|window| window == b"PACK"));
    let cached = metrics(&root, "cache-filled", &native, false);
    assert_eq!(
        response,
        http(
            &root,
            "cache-recheck",
            &format!("{gateway}/git-upload-pack"),
            Some(&request)
        )
    );
    let recheck = metrics(&root, "cache-rechecked", &native, false);
    assert_eq!(n(&recheck, "cache_hits"), n(&cached, "cache_hits") + 1);
    assert_eq!(n(&recheck, "cache_misses"), n(&cached, "cache_misses"));
    assert_warm_read(&cached, &recheck);
    let final_stats = metrics(&root, "final-stats", &native, true);
    assert_warm_read(&before_history, &final_stats);
    for (name, tip) in [
        ("refs/heads/main", &tips[2]),
        ("refs/heads/snapshot01", &tips[0]),
        ("refs/heads/snapshot02", &tips[1]),
        ("refs/heads/snapshot03", &tips[2]),
    ] {
        assert_eq!(final_stats["manifest"]["refs"][name], *tip);
    }
    git(
        &root.join("host/view/repo.git"),
        &["fsck", "--strict", "--full"],
        &[],
    );
    fs::write(root.join("contract.json"), serde_json::to_vec_pretty(&json!({
        "kind":"production_worker_warm_correctness_not_benchmark", "worker_origin":origin,
        "view_cache":"warm", "gateway_first_read_default":"on_but_not_negotiated",
        "reads":reads, "cache_filled":cached, "cache_recheck":recheck, "final_stats":final_stats,
        "fsck_strict":true, "archive_identical":true, "private_rpc_plans":0,
        "full_bill_cpu":null, "speed_claim":null,
    })).unwrap()).unwrap();
}
