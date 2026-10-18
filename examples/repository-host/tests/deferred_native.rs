//! Rust owns this registered Native/real R2/SQLite contract. The ignored test
//! starts its own Wrangler on GIT_ZERO_DEFERRED_NATIVE_PORT (default 8833).
use git_zero_host::{
    accounting::{OperationUsage, ServiceUsage},
    hash,
    native::command,
    storage::{Manifest, StoredPack},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TOKEN: &str = "git-zero-local-services";

struct Runtime {
    child: Child,
    origin: String,
}
impl Runtime {
    fn start(root: &Path) -> Self {
        let port = std::env::var("GIT_ZERO_DEFERRED_NATIVE_PORT")
            .unwrap_or_else(|_| "8833".into())
            .parse::<u16>()
            .unwrap();
        assert!(port > 0);
        let reservation = std::net::TcpListener::bind(("127.0.0.1", port))
            .expect("owned Native contract port must be free; never stop another runtime");
        let cli = std::env::var("GIT_ZERO_WRANGLER_CLI").unwrap_or_else(|_| {
            "/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js".into()
        });
        assert!(Path::new(&cli).is_file(), "reuse the installed Wrangler");
        let config =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/test/deferred-catalog.toml");
        let log_path = root.join("wrangler.log");
        let log = fs::File::create(&log_path).unwrap();
        drop(reservation);
        let child = Command::new("setsid")
            .args(["node", &cli, "dev", "--config"])
            .arg(config)
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--local",
                "--persist-to",
            ])
            .arg(root.join("persist"))
            .arg("--show-interactive-dev-session=false")
            .env("WRANGLER_SEND_METRICS", "false")
            .env("X_LOCAL_EXPLORER", "false")
            .env("X_LOCAL_OBSERVABILITY", "false")
            .env("BROWSER", "none")
            .env("WRANGLER_LOG_PATH", root.join("logs"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        fs::write(root.join("owned-runtime-pid"), child.id().to_string()).unwrap();
        let mut runtime = Self {
            child,
            origin: format!("http://127.0.0.1:{port}"),
        };
        for _ in 0..300 {
            assert!(
                runtime.child.try_wait().unwrap().is_none(),
                "Wrangler exited: {}",
                log_path.display()
            );
            if fs::read_to_string(&log_path)
                .unwrap()
                .contains(&format!("Ready on {}", runtime.origin))
            {
                return runtime;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Wrangler not ready: {}", log_path.display());
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = Command::new("kill")
                .args(["-TERM", "--", &format!("-{}", self.child.id())])
                .status();
            let _ = self.child.wait();
        }
    }
}

struct NativeProcess {
    child: Child,
    origin: String,
}
impl NativeProcess {
    fn start(worker: &Worker<'_>, root: &Path) -> Self {
        let log = fs::File::create(root.join("native.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_repository-host"))
            .args([
                "--cloudflare",
                worker.origin,
                "--repo",
                &worker.repo,
                "--token",
                TOKEN,
                "--view-cache",
                "warm",
            ])
            .arg(root.join("native"))
            .arg("0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut native = Self {
            child,
            origin: String::new(),
        };
        let mut ready = String::new();
        BufReader::new(native.child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        native.origin = ready
            .trim()
            .strip_suffix("/repo.git")
            .expect("Native failed before actual registration; inspect native.log")
            .into();
        native
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(
    origin: &str,
    method: &str,
    route: &str,
    repo: Option<&str>,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut process = Command::new("curl");
    process.args([
        "--silent",
        "--show-error",
        "--max-time",
        "120",
        "--request",
        method,
        "--write-out",
        "\n%{http_code}",
    ]);
    if let Some(repo) = repo {
        for (name, value) in [
            ("x-hosting-token", TOKEN),
            ("x-hosting-repo", repo),
            ("x-hosting-phase", "push"),
        ] {
            process.arg("--header").arg(format!("{name}: {value}"));
        }
    }
    if !body.is_empty() {
        process.args(["--data-binary", "@-"]);
    }
    let mut child = process
        .arg(format!("{origin}{route}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(body).unwrap();
    let mut output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "curl: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let boundary = output.stdout.len().checked_sub(4).unwrap();
    assert_eq!(output.stdout[boundary], b'\n');
    let status = std::str::from_utf8(&output.stdout[boundary + 1..])
        .unwrap()
        .parse()
        .unwrap();
    output.stdout.truncate(boundary);
    (status, output.stdout)
}

struct Worker<'a> {
    origin: &'a str,
    repo: String,
}
impl Worker<'_> {
    fn json(&self, method: &str, route: &str, value: Value) -> Value {
        let bytes = if method == "GET" {
            Vec::new()
        } else {
            serde_json::to_vec(&value).unwrap()
        };
        let (status, body) = request(self.origin, method, route, Some(&self.repo), &bytes);
        assert_eq!(status, 200, "{route}: {}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }
    fn state(&self) -> Value {
        self.json("GET", "/_services/test/deferred", Value::Null)
    }
    fn usage(&self) -> ServiceUsage {
        serde_json::from_value(self.json("GET", "/_services/metrics", Value::Null)).unwrap()
    }
    fn page_fault(&self, mode: &str) {
        self.json("POST", "/_services/test/page-fault", json!({"mode":mode}));
    }
    fn fault(&self, mode: &str) {
        self.json(
            "POST",
            "/_services/test/deferred-fault",
            json!({"mode":mode}),
        );
    }
    fn seed_singletons(&self, manifest: &Manifest, peak: u64) {
        let rows: Vec<_> = manifest.packs.iter().flat_map(StoredPack::chunks).map(|chunk| {
            let digest = chunk.key.strip_prefix("chunks/").unwrap();
            let mut entries = vec![0x47, 0x5a, 0x43, 0x50, 2, 1];
            entries.extend(manifest.generation.to_le_bytes());
            for offset in (0..64).step_by(2) {
                entries.push(u8::from_str_radix(&digest[offset..offset + 2], 16).unwrap());
            }
            entries.extend(u32::try_from(chunk.len).unwrap().to_le_bytes());
            json!({"key":format!("pages/{digest}"),"size":chunk.len,"peak_bytes":peak,"entries":entries})
        }).collect();
        assert_eq!(rows.len(), 3);
        self.json(
            "POST",
            "/_services/test/seed",
            json!({"reset":true,"rows":rows}),
        );
    }
}

fn git(source: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(source).args(args);
    command(process, input).unwrap().stdout
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().to_owned()
}

fn commit(source: &Path, parent: Option<&str>, generation: u64) -> (String, String, Vec<u8>) {
    let data = format!("deferred Native retirement generation {generation}\n")
        .repeat(512)
        .into_bytes();
    let blob = text(git(source, &["hash-object", "-w", "--stdin"], &data));
    let tree = text(git(
        source,
        &["mktree"],
        format!("100644 blob {blob}\tdata.txt\n").as_bytes(),
    ));
    let mut args = vec!["commit-tree", &tree];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    let tip = text(git(
        source,
        &args,
        format!("generation {generation}\n").as_bytes(),
    ));
    (tip, blob, data)
}

fn push(source: &Path, url: &str, tip: &str) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["push", "--quiet", url, &format!("{tip}:refs/heads/main")])
        .output()
        .unwrap()
}

fn operation(usage: &ServiceUsage, name: &str) -> OperationUsage {
    usage.operations.get(name).cloned().unwrap_or_default()
}

#[test]
#[ignore = "owns configurable default 8833; real registered Native compaction, partial DELETE and ambiguous SQL/sync recovery"]
fn actual_registered_native_deferred_retirement_recovers_before_next_mutation() {
    let root = std::env::var("GIT_ZERO_DEFERRED_NATIVE_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "git-zero-deferred-native-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
        });
    fs::create_dir_all(&root).unwrap();
    assert!(
        !root.join("persist").exists(),
        "requires fresh owned persistence"
    );
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fingerprints: BTreeMap<_, _> = [
        "cloudflare/src/index.js",
        "cloudflare/src/catalog-pages.js",
        "cloudflare/test/deferred-catalog-worker.js",
        "src/cloudflare.rs",
        "src/native.rs",
        "tests/deferred_native.rs",
    ]
    .into_iter()
    .map(|file| (file, hash(&fs::read(package.join(file)).unwrap())))
    .collect();
    fs::write(
        root.join("source.json"),
        serde_json::to_vec_pretty(&fingerprints).unwrap(),
    )
    .unwrap();
    let runtime = Runtime::start(&root);
    let mut reports = Vec::new();
    for mode in [
        "delete-second",
        "retire-catalog",
        "retire-sync",
        "cursor-unknown",
    ] {
        let case_root = root.join(mode);
        let source = case_root.join("source.git");
        fs::create_dir_all(&source).unwrap();
        git(
            &source,
            &["init", "--bare", "--quiet", "--object-format=sha1"],
            &[],
        );
        git(
            &source,
            &["config", "user.name", "Deferred Native contract"],
            &[],
        );
        git(
            &source,
            &["config", "user.email", "contract@example.invalid"],
            &[],
        );
        let worker = Worker {
            origin: &runtime.origin,
            repo: format!("native-{mode}-{}", std::process::id()),
        };
        let native = NativeProcess::start(&worker, &case_root);
        let url = format!("{}/{}/repo.git", runtime.origin, worker.repo);
        let mut parent = None;
        for generation in 1..=3 {
            let (tip, _, _) = commit(&source, parent.as_deref(), generation);
            let output = push(&source, &url, &tip);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            parent = Some(tip);
        }
        let state = worker.state();
        let manifest: Manifest = serde_json::from_value(state["manifest"].clone()).unwrap();
        assert_eq!(manifest.generation, 3);
        assert_eq!(manifest.packs.len(), 3);
        if mode == "retire-catalog" {
            // Legitimate singleton pages force the charged eager path, so the
            // existing real SQL fault executes even with deferred capability.
            worker.seed_singletons(&manifest, state["peak"].as_u64().unwrap());
        }
        let before = worker.usage();
        worker.page_fault(if mode == "cursor-unknown" { "" } else { mode });
        worker.fault(if mode == "cursor-unknown" { mode } else { "" });
        let (fourth, _, _) = commit(&source, parent.as_deref(), 4);
        let pushed = push(&source, &url, &fourth);
        let partial = mode == "delete-second";
        let unknown = mode == "cursor-unknown";
        assert_eq!(
            pushed.status.success(),
            partial || unknown,
            "{mode}: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        let failed_state = worker.state();
        let published: Manifest = serde_json::from_value(failed_state["manifest"].clone()).unwrap();
        assert_eq!(
            published.generation, 4,
            "publication remains authoritative despite retirement error"
        );
        assert_eq!(published.refs["refs/heads/main"], fourth);
        assert_eq!(published.garbage.len(), 3);
        assert_eq!(
            published.packs.len(),
            1,
            "the fourth append really compacted"
        );
        assert_eq!(failed_state["certain"], unknown);
        let after = worker.usage();
        assert_eq!(
            operation(&after, "manifest_cas").successes
                - operation(&before, "manifest_cas").successes,
            1
        );
        assert_eq!(
            operation(&after, "r2_put").attempts - operation(&before, "r2_put").attempts,
            1
        );
        assert_eq!(
            operation(&after, "r2_delete").attempts - operation(&before, "r2_delete").attempts,
            3
        );
        assert_eq!(
            operation(&after, "r2_delete").successes - operation(&before, "r2_delete").successes,
            if partial { 2 } else { 3 }
        );
        assert_eq!(
            operation(&after, "r2_delete").failures - operation(&before, "r2_delete").failures,
            u64::from(partial)
        );
        assert_eq!(
            after.calls.do_rows_read.is_none(),
            mode == "retire-catalog" || unknown
        );
        assert_eq!(
            after.calls.do_rows_written.is_none(),
            mode == "retire-catalog" || unknown
        );
        assert_eq!(after.accounting_complete, Some(true));
        let view = case_root.join("native/view/repo.git");
        assert_eq!(
            text(git(&view, &["rev-parse", "refs/heads/main"], &[])),
            if partial || unknown {
                fourth.clone()
            } else {
                parent.clone().unwrap()
            }
        );
        // Inspection used only SQL/observer metrics. No restart, Native stats,
        // explicit GC, or test-side retry can recover its dirty native view.
        worker.page_fault("");
        let before_recovery = worker.usage();
        worker.fault("");
        let (fifth, blob, data) = commit(&source, Some(&fourth), 5);
        let pushed = push(&source, &url, &fifth);
        assert!(
            pushed.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        let recovered_state = worker.state();
        let after_recovery = worker.usage();
        let delta = after_recovery
            .checked_workload_delta(&before_recovery)
            .unwrap();
        let trace = recovered_state["trace"].as_array().unwrap();
        let put = trace
            .iter()
            .position(|event| event.as_str().unwrap().starts_with("r2:put:"))
            .unwrap();
        let before_put = &trace[..put];
        if unknown {
            assert_eq!(
                delta.r2_class_a, 1,
                "known successful publication needs only its next real PUT"
            );
            assert!(
                !before_put
                    .iter()
                    .any(|event| event.as_str().unwrap().starts_with("r2:list:"))
            );
            assert_eq!(
                before_put
                    .iter()
                    .filter(|event| event.as_str().unwrap().starts_with("r2:head:"))
                    .count(),
                0,
                "the live DO already observed successful DELETEs despite unknown SQL counts"
            );
        } else {
            assert_eq!(
                delta.r2_class_a, 2,
                "dirty view requires one generation-checked GC LIST before the next PUT"
            );
            let checked = before_put
                .iter()
                .position(|event| {
                    event == "SELECT generation, manifest FROM repository_state WHERE id = 1"
                })
                .expect("recovery GC checks authoritative generation");
            let listed = before_put
                .iter()
                .position(|event| event.as_str().unwrap().starts_with("r2:list:"))
                .expect("actual recovery inventory LIST");
            assert!(checked < listed);
            assert!(
                before_put[listed + 1..]
                    .iter()
                    .any(|event| event == "sync:durable"),
                "GC becomes durable before another mutation"
            );
        }
        assert_eq!(
            operation(&after_recovery, "r2_delete").attempts
                - operation(&before_recovery, "r2_delete").attempts,
            if unknown { 0 } else { 3 + u64::from(partial) },
            "GC removes any remaining object, then Native retries all three durable debt keys idempotently"
        );
        let recovered: Manifest =
            serde_json::from_value(recovered_state["manifest"].clone()).unwrap();
        assert_eq!(recovered.generation, 5);
        assert_eq!(recovered.refs["refs/heads/main"], fifth);
        assert!(recovered.garbage.is_empty());
        assert_eq!(recovered_state["certain"], true);
        assert!(recovered_state["sqlite_bytes"].as_u64().unwrap() <= 24_821);
        assert_eq!(
            text(git(&view, &["rev-parse", "refs/heads/main"], &[])),
            fifth
        );
        git(&view, &["fsck", "--strict"], &[]);
        let clone = case_root.join("client.git");
        git(
            &case_root,
            &["clone", "--bare", "--quiet", &url, clone.to_str().unwrap()],
            &[],
        );
        git(&clone, &["fsck", "--strict"], &[]);
        assert_eq!(git(&clone, &["cat-file", "blob", &blob], &[]), data);
        let (status, metrics) = request(&native.origin, "GET", "/metrics", None, &[]);
        assert_eq!(status, 200);
        let metrics: Value = serde_json::from_slice(&metrics).unwrap();
        assert_eq!(metrics["compactions"], 1);
        assert!(metrics["cache_retained_bytes"].as_u64().unwrap() <= 32 * 1024 * 1024);
        reports.push(json!({"mode":mode,"published_push_succeeded":partial || unknown,"before":before,"after":after,"failed_state":failed_state,"recovery_delta":delta,"recovered":recovered_state,"native_metrics":metrics}));
        worker.json("POST", "/_services/unregister", json!({}));
    }
    drop(runtime);
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    eprintln!("Native deferred contract artifacts: {}", root.display());
}
