//! Rust owns this registered Native/real R2/SQLite contract. The ignored test
//! starts its own Wrangler on GIT_ZERO_INLINE_NATIVE_PORT (default 8833).
use git_zero_host::{
    accounting::{OperationUsage, ServiceUsage},
    hash,
    native::command,
    storage::Manifest,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TOKEN: &str = "git-zero-local-services";
const PROOF: &str = "x-git-zero-retirement-proof";

struct HttpMessage {
    first: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    wire: Vec<u8>,
}

fn read_http(reader: &mut impl BufRead) -> io::Result<HttpMessage> {
    let mut message = HttpMessage {
        first: String::new(),
        headers: BTreeMap::new(),
        body: Vec::new(),
        wire: Vec::new(),
    };
    let mut line = String::new();
    reader.read_line(&mut line)?;
    message.first = line.trim().into();
    message.wire.extend_from_slice(line.as_bytes());
    loop {
        line.clear();
        assert!(reader.read_line(&mut line)? > 0, "truncated HTTP headers");
        message.wire.extend_from_slice(line.as_bytes());
        assert!(message.wire.len() <= 64 * 1024, "bounded fixture headers");
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        assert!(
            message
                .headers
                .insert(name.to_ascii_lowercase(), value.trim().into())
                .is_none()
        );
    }
    if message.first.starts_with("HTTP/")
        && matches!(message.first.split_whitespace().nth(1), Some("204" | "304"))
    {
        return Ok(message);
    }
    if message
        .headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        loop {
            line.clear();
            assert!(reader.read_line(&mut line)? > 0, "truncated HTTP chunk");
            message.wire.extend_from_slice(line.as_bytes());
            let length = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
            assert!(
                message.body.len() + length <= 2 * 1024 * 1024,
                "bounded fixture body"
            );
            if length == 0 {
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line)? > 0);
                    message.wire.extend_from_slice(line.as_bytes());
                    if line == "\r\n" {
                        break;
                    }
                }
                break;
            }
            let offset = message.body.len();
            message.body.resize(offset + length, 0);
            reader.read_exact(&mut message.body[offset..])?;
            message.wire.extend_from_slice(&message.body[offset..]);
            let mut ending = [0; 2];
            reader.read_exact(&mut ending)?;
            assert_eq!(ending, *b"\r\n");
            message.wire.extend_from_slice(&ending);
        }
    } else if let Some(length) = message.headers.get("content-length") {
        let length = length.parse::<usize>().unwrap();
        assert!(length <= 2 * 1024 * 1024, "bounded fixture body");
        message.body.resize(length, 0);
        reader.read_exact(&mut message.body)?;
        message.wire.extend_from_slice(&message.body);
    } else if message.first.starts_with("HTTP/") {
        reader
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut message.body)?;
        assert!(message.body.len() <= 2 * 1024 * 1024);
        message.wire.extend_from_slice(&message.body);
    }
    Ok(message)
}

// This test-only Rust bridge records the actual Native service exchange. It
// preserves bytes and responses, including errors and optional proof headers.
struct Bridge {
    origin: String,
    root: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Bridge {
    fn start(worker: &str, root: &Path) -> Self {
        let root = root.join("bridge");
        fs::create_dir(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let destination = worker.strip_prefix("http://").unwrap().to_owned();
        let output = root.clone();
        let thread = std::thread::spawn(move || {
            let mut sequence = 0;
            while !stopping.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("recording bridge: {error}"),
                };
                assert!(sequence < 256, "bounded Native fixture exchange count");
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let request = read_http(&mut BufReader::new(stream.try_clone().unwrap())).unwrap();
                fs::write(
                    output.join(format!("{sequence:03}.request.http")),
                    &request.wire,
                )
                .unwrap();
                let mut upstream = TcpStream::connect(&destination).unwrap();
                upstream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                upstream
                    .set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                upstream.write_all(&request.wire).unwrap();
                let response = read_http(&mut BufReader::new(upstream)).unwrap();
                fs::write(
                    output.join(format!("{sequence:03}.response.http")),
                    &response.wire,
                )
                .unwrap();
                stream.write_all(&response.wire).unwrap();
                sequence += 1;
            }
        });
        Self {
            origin,
            root,
            stop,
            thread: Some(thread),
        }
    }

    fn exchanges(&self) -> Vec<(HttpMessage, HttpMessage)> {
        let mut paths = fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with(".request.http"))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let response = path.with_file_name(
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .replace(".request.", ".response."),
                );
                (
                    read_http(&mut BufReader::new(fs::File::open(path).unwrap())).unwrap(),
                    read_http(&mut BufReader::new(fs::File::open(response).unwrap())).unwrap(),
                )
            })
            .collect()
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if !std::thread::panicking() {
                thread.join().unwrap();
            }
        }
    }
}

struct Runtime {
    child: Child,
    origin: String,
}
impl Runtime {
    fn start(root: &Path) -> Self {
        let port = std::env::var("GIT_ZERO_INLINE_NATIVE_PORT")
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/test/fused-metadata.toml");
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
    fn start(worker: &Worker<'_>, root: &Path, bridge: &str) -> Self {
        let log = fs::File::create(root.join("native.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_repository-host"))
            .args([
                "--cloudflare",
                bridge,
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
        self.json("GET", "/_services/test/inline-state", Value::Null)
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
    let data = format!("inline Native retirement generation {generation}\n")
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

fn save(root: &Path, name: &str, value: &impl serde::Serialize) {
    fs::write(root.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn recorded_push(root: &Path, generation: u64, source: &Path, url: &str, tip: &str) -> Output {
    let output = push(source, url, tip);
    fs::write(
        root.join(format!("push-{generation}.stdout")),
        &output.stdout,
    )
    .unwrap();
    fs::write(
        root.join(format!("push-{generation}.stderr")),
        &output.stderr,
    )
    .unwrap();
    fs::write(
        root.join(format!("push-{generation}.status")),
        output.status.to_string(),
    )
    .unwrap();
    output
}

fn fingerprints() -> BTreeMap<&'static str, String> {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        "cloudflare/src/index.js",
        "cloudflare/src/catalog-pages.js",
        "cloudflare/src/retirement-proof.js",
        "cloudflare/src/inline-catalog.js",
        "cloudflare/test/peak-catalog-worker.js",
        "cloudflare/test/catalog-pages-worker.js",
        "cloudflare/test/deferred-catalog-worker.js",
        "cloudflare/test/fused-metadata-worker.js",
        "cloudflare/test/fused-metadata.toml",
        "src/cloudflare.rs",
        "src/native.rs",
        "src/lib.rs",
        "tests/inline_native.rs",
    ]
    .into_iter()
    .map(|path| (path, hash(&fs::read(package.join(path)).unwrap())))
    .collect()
}

fn trace_position(trace: &[Value], prefix: &str) -> Option<usize> {
    trace
        .iter()
        .position(|event| event.as_str().unwrap().starts_with(prefix))
}

#[test]
#[ignore = "owns default 8833; same registered Native must recover real fused SQL/sync/response faults before its next PUT"]
fn actual_registered_native_inline_faults_recover_before_next_mutation() {
    let root = std::env::var("GIT_ZERO_INLINE_NATIVE_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "git-zero-inline-native-{}-{}",
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
    let frozen = fingerprints();
    save(&root, "source.json", &frozen);
    let runner = std::env::current_exe().unwrap();
    let native_binary = Path::new(env!("CARGO_BIN_EXE_repository-host"));
    save(
        &root,
        "runner.json",
        &json!({
            "executable":runner,"sha256":hash(&fs::read(&runner).unwrap()),
            "args":std::env::args().collect::<Vec<_>>(),
            "native_executable":native_binary,"native_sha256":hash(&fs::read(native_binary).unwrap()),
        }),
    );
    eprintln!("Native inline contract artifacts: {}", root.display());
    let runtime = Runtime::start(&root);
    let mut reports = Vec::new();
    for mode in [
        "future-normalized-ack",
        "fused-sql-before",
        "fused-transaction",
        "fused-sync-after",
        "lost-retirement-reply",
        "delete-second",
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
            &["config", "user.name", "Inline Native contract"],
            &[],
        );
        git(
            &source,
            &["config", "user.email", "contract@example.invalid"],
            &[],
        );
        let worker = Worker {
            origin: &runtime.origin,
            repo: format!("inline-native-{mode}-{}", std::process::id()),
        };
        let bridge = Bridge::start(worker.origin, &case_root);
        let native = NativeProcess::start(&worker, &case_root, &bridge.origin);
        let native_pid = native.child.id();
        fs::write(case_root.join("native-pid"), native_pid.to_string()).unwrap();
        let url = format!("{}/{}/repo.git", runtime.origin, worker.repo);
        let mut parent = None;
        for generation in 1..=3 {
            let (tip, _, _) = commit(&source, parent.as_deref(), generation);
            let output = recorded_push(&case_root, generation, &source, &url, &tip);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            parent = Some(tip);
        }
        let initial = worker.state();
        save(&case_root, "initial-state.json", &initial);
        let manifest: Manifest = serde_json::from_value(initial["manifest"].clone()).unwrap();
        assert_eq!(manifest.generation, 3);
        assert_eq!(manifest.packs.len(), 3);
        assert_eq!(initial["root"]["entries"].as_array().unwrap().len(), 3);
        assert!(initial["catalog"].as_array().unwrap().is_empty());
        let future_ack = mode == "future-normalized-ack";
        if future_ack {
            worker.json(
                "POST",
                "/_services/test/inline-normalized-future",
                json!({}),
            );
            save(&case_root, "future-normalized-state.json", &worker.state());
        }
        let before = worker.usage();
        save(&case_root, "before-usage.json", &before);
        worker.page_fault(if mode == "delete-second" { mode } else { "" });
        worker.fault(if mode == "delete-second" { "" } else { mode });
        let before_fault_exchange = bridge.exchanges().len();
        let (fourth, _, _) = commit(&source, parent.as_deref(), 4);
        let pushed = recorded_push(&case_root, 4, &source, &url, &fourth);
        // Persist each actual observation before assertions so failed gates
        // retain useful evidence instead of only an end-of-test report.
        let failed_state = worker.state();
        save(&case_root, "failed-state.json", &failed_state);
        let after = worker.usage();
        save(&case_root, "after-usage.json", &after);
        let rollback = matches!(
            mode,
            "fused-sql-before" | "fused-transaction" | "future-normalized-ack"
        );
        let partial = mode == "delete-second";
        let unknown = mode == "cursor-unknown";
        let retired = matches!(
            mode,
            "lost-retirement-reply" | "delete-second" | "cursor-unknown"
        );
        assert_eq!(
            pushed.status.success(),
            partial || unknown,
            "{mode}: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        let exchanges = bridge.exchanges();
        let fault_calls = &exchanges[before_fault_exchange..];
        let uploads: Vec<_> = fault_calls
            .iter()
            .filter(|(request, _)| {
                request
                    .first
                    .starts_with("POST /_services/manifest/upload-retire ")
            })
            .collect();
        assert_eq!(
            uploads.len(),
            1,
            "ambiguous upload never retries its PUT/CAS"
        );
        assert!(
            !fault_calls.iter().any(|(request, _)| request
                .first
                .starts_with("PUT /_services/blob/")
                || request.first.starts_with("POST /_services/manifest ")
                || request
                    .first
                    .starts_with("POST /_services/manifest/retire ")),
            "no scalar fallback after sending the fused frame"
        );
        let (upload_request, upload_response) = uploads[0];
        assert!(
            !upload_response.headers.contains_key(PROOF),
            "failed/partial/unknown SQL responses never certify all-success retirement"
        );
        assert_eq!(&upload_request.body[..4], b"GZUP");
        let length = u32::from_le_bytes(upload_request.body[8..12].try_into().unwrap()) as usize;
        let frame: Value = serde_json::from_slice(&upload_request.body[12..12 + length]).unwrap();
        assert_eq!(frame["expected"], 3);
        assert_eq!(frame["next"]["generation"], 4);
        assert_eq!(frame["next"]["garbage"].as_array().unwrap().len(), 3);
        assert_eq!(
            frame["next"]["packs"].as_array().unwrap().len(),
            1,
            "fourth real Git push really compacts"
        );
        if partial || unknown {
            let response: Value = serde_json::from_slice(&upload_response.body).unwrap();
            let outcomes = response["retirement"]["outcomes"].as_array().unwrap();
            assert_eq!(
                outcomes
                    .iter()
                    .map(|outcome| outcome["deleted"].as_bool().unwrap())
                    .collect::<Vec<_>>(),
                if partial {
                    vec![true, false, true]
                } else {
                    vec![true, true, true]
                }
            );
        }
        let published: Manifest = serde_json::from_value(failed_state["manifest"].clone()).unwrap();
        assert_eq!(published.generation, if rollback { 3 } else { 4 });
        assert_eq!(
            published.refs["refs/heads/main"],
            if rollback {
                parent.clone().unwrap()
            } else {
                fourth.clone()
            }
        );
        assert_eq!(published.packs.len(), if rollback { 3 } else { 1 });
        assert_eq!(
            failed_state["certain"],
            mode == "lost-retirement-reply" || unknown
        );
        assert_eq!(
            failed_state["catalog"].as_array().unwrap().len(),
            usize::from(future_ack)
        );
        assert_eq!(
            operation(&after, "r2_put").attempts - operation(&before, "r2_put").attempts,
            u64::from(!future_ack)
        );
        assert_eq!(
            operation(&after, "r2_put").successes - operation(&before, "r2_put").successes,
            u64::from(!future_ack)
        );
        assert_eq!(
            operation(&after, "manifest_cas").attempts
                - operation(&before, "manifest_cas").attempts,
            u64::from(!future_ack)
        );
        assert_eq!(
            operation(&after, "manifest_cas").successes
                - operation(&before, "manifest_cas").successes,
            u64::from(retired)
        );
        assert_eq!(
            operation(&after, "r2_delete").attempts - operation(&before, "r2_delete").attempts,
            if retired { 3 } else { 0 }
        );
        assert_eq!(
            operation(&after, "r2_delete").successes - operation(&before, "r2_delete").successes,
            if partial {
                2
            } else if retired {
                3
            } else {
                0
            }
        );
        assert_eq!(
            after.r2_live_objects,
            if retired {
                if partial { 2 } else { 1 }
            } else if future_ack {
                3
            } else {
                4
            }
        );
        let peak = if future_ack {
            before.r2_peak_bytes
        } else {
            before
                .r2_peak_bytes
                .max(before.r2_live_bytes + frame["chunk"]["len"].as_u64().unwrap())
        };
        if future_ack {
            assert_eq!(upload_response.first.split_whitespace().nth(1), Some("409"));
            let delta = after.checked_workload_delta(&before).unwrap();
            assert_eq!(delta.r2_class_a + delta.r2_class_b + delta.r2_deletes, 0);
            assert_eq!(delta.do_rows_written, Some(0));
        }
        assert_eq!(
            after.r2_peak_bytes, peak,
            "full actual post-PUT/pre-retirement physical peak"
        );
        assert_eq!(
            after.calls.do_rows_read.is_none(),
            mode == "fused-sql-before" || unknown
        );
        assert_eq!(
            after.calls.do_rows_written.is_none(),
            mode == "fused-sql-before" || unknown
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
        // Worker SQL/observer reads above cannot recover the Native view. Its
        // next real Git request performs recovery, with the same process.
        worker.page_fault("");
        let before_recovery = worker.usage();
        save(&case_root, "before-recovery-usage.json", &before_recovery);
        worker.fault("");
        let before_recovery_exchange = bridge.exchanges().len();
        let (fifth, fifth_blob, fifth_data) = commit(&source, Some(&fourth), 5);
        let pushed = recorded_push(&case_root, 5, &source, &url, &fifth);
        let recovered_state = worker.state();
        save(&case_root, "recovered-state.json", &recovered_state);
        let after_recovery = worker.usage();
        save(&case_root, "after-recovery-usage.json", &after_recovery);
        assert!(
            pushed.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        assert_eq!(native.child.id(), native_pid);
        let delta = after_recovery
            .checked_workload_delta(&before_recovery)
            .unwrap();
        let trace = recovered_state["trace"].as_array().unwrap();
        let put = trace_position(trace, "r2:put:").unwrap();
        let before_put = &trace[..put];
        let recovery_calls = bridge.exchanges();
        let recovery_calls = &recovery_calls[before_recovery_exchange..];
        let recovery_gc = recovery_calls
            .iter()
            .position(|(request, _)| request.first.starts_with("POST /_services/gc "));
        let recovery_upload = recovery_calls
            .iter()
            .position(|(request, _)| {
                request
                    .first
                    .starts_with("POST /_services/manifest/upload-retire ")
            })
            .unwrap();
        if unknown {
            assert_eq!(
                delta.r2_class_a, 1,
                "known successful publication needs only the next PUT"
            );
            assert!(trace_position(before_put, "r2:list:").is_none());
            assert!(recovery_gc.is_none());
            assert!(
                recovery_calls
                    .iter()
                    .all(|(request, _)| !request.headers.contains_key(PROOF)),
                "SDK retains no proof from unknown SQL"
            );
        } else {
            assert_eq!(
                delta.r2_class_a, 2,
                "ambiguous/partial publication requires the Native's generation-checked physical GC LIST"
            );
            let checked = before_put
                .iter()
                .position(|event| {
                    event == "SELECT generation, manifest FROM repository_state WHERE id = 1"
                })
                .unwrap();
            let listed = trace_position(before_put, "r2:list:")
                .expect("actual recovery LIST precedes next PUT");
            assert!(checked < listed);
            assert!(
                before_put[listed + 1..]
                    .iter()
                    .any(|event| event == "sync:durable"),
                "actual GC sync precedes mutation"
            );
            assert!(recovery_gc.unwrap() < recovery_upload);
        }
        let recovered: Manifest =
            serde_json::from_value(recovered_state["manifest"].clone()).unwrap();
        assert_eq!(recovered.generation, if rollback { 4 } else { 5 });
        assert_eq!(recovered.refs["refs/heads/main"], fifth);
        assert_eq!(recovered_state["certain"], true);
        assert!(after_recovery.r2_peak_bytes >= peak);
        assert_eq!(
            text(git(&view, &["rev-parse", "refs/heads/main"], &[])),
            fifth
        );
        git(&view, &["fsck", "--strict"], &[]);
        assert_eq!(
            git(&view, &["archive", &fifth], &[]),
            git(&source, &["archive", &fifth], &[])
        );
        // A rollback makes push5 another genuine compaction. One additional
        // paid push then folds only that newly completed retirement debt.
        let mut final_tip = fifth;
        let mut final_blob = fifth_blob;
        let mut final_data = fifth_data;
        if rollback {
            assert_eq!(recovered.garbage.len(), 3);
            let (sixth, blob, data) = commit(&source, Some(&final_tip), 6);
            let pushed = recorded_push(&case_root, 6, &source, &url, &sixth);
            assert!(
                pushed.status.success(),
                "{mode}: {}",
                String::from_utf8_lossy(&pushed.stderr)
            );
            final_tip = sixth;
            final_blob = blob;
            final_data = data;
        }
        let final_state = worker.state();
        let final_usage = worker.usage();
        save(&case_root, "final-state.json", &final_state);
        save(&case_root, "final-usage.json", &final_usage);
        let final_manifest: Manifest =
            serde_json::from_value(final_state["manifest"].clone()).unwrap();
        assert_eq!(final_manifest.refs["refs/heads/main"], final_tip);
        assert!(final_manifest.garbage.is_empty());
        assert!(final_state["debt"].is_null());
        assert_eq!(final_state["certain"], true);
        assert!(final_state["catalog"].as_array().unwrap().is_empty());
        assert_eq!(final_state["live_objects"], final_usage.r2_live_objects);
        assert_eq!(final_state["live_bytes"], final_usage.r2_live_bytes);
        assert_eq!(
            final_state["root"]["entries"].as_array().unwrap().len() as u64,
            final_usage.r2_live_objects
        );
        assert!(final_state["sqlite_bytes"].as_u64().unwrap() <= 24_821);
        assert!(final_state["root"]["peak"].as_u64().unwrap() >= peak);
        let clone = case_root.join("client.git");
        git(
            &case_root,
            &["clone", "--bare", "--quiet", &url, clone.to_str().unwrap()],
            &[],
        );
        git(&clone, &["fsck", "--strict"], &[]);
        assert_eq!(
            git(&clone, &["cat-file", "blob", &final_blob], &[]),
            final_data
        );
        assert_eq!(
            git(&clone, &["archive", &final_tip], &[]),
            git(&source, &["archive", &final_tip], &[])
        );
        let (status, metrics) = request(&native.origin, "GET", "/metrics", None, &[]);
        fs::write(case_root.join("native-metrics.json"), &metrics).unwrap();
        assert_eq!(status, 200);
        let metrics: Value = serde_json::from_slice(&metrics).unwrap();
        assert_eq!(metrics["compactions"], if rollback { 2 } else { 1 });
        assert!(metrics["cache_retained_bytes"].as_u64().unwrap() <= 32 * 1024 * 1024);
        assert_eq!(metrics["cache_budget_bytes"], 32 * 1024 * 1024);
        assert!(metrics["cache_entries"].as_u64().unwrap() <= 256);
        assert!(metrics["history_lineage_bytes"].as_u64().unwrap() <= 32 * 1024);
        reports.push(json!({"mode":mode,"native_pid":native_pid,"rollback":rollback,
            "push4_success":partial || unknown,"before":before,"after":after,"failed_state":failed_state,
            "recovery_delta":delta,"recovered":recovered_state,"final_state":final_state,
            "final_usage":final_usage,"native_metrics":metrics}));
        save(&root, "report.json", &reports);
        worker.json("POST", "/_services/unregister", json!({}));
    }
    drop(runtime);
    let after = fingerprints();
    save(&root, "source-after.json", &after);
    assert_eq!(
        frozen, after,
        "frozen production, wrappers and driver must not change while runtime watches them"
    );
}
