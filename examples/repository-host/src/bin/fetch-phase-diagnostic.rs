//! Stage-one, tiny real-Git production-gateway proof. No benchmark/scorer edits.
use git_zero_host::{diagnostics, hash};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const CAP: usize = 2 * 1024 * 1024;
static START: OnceLock<Instant> = OnceLock::new();
fn budget_expired() -> bool {
    START.get_or_init(Instant::now).elapsed() > Duration::from_secs(180)
}

const GATEWAY: &str = "http://127.0.0.1:8853/fetchdiag/repo.git";

fn save(root: &Path, name: &str, value: &Value) -> io::Result<()> {
    fs::write(root.join(name), serde_json::to_vec_pretty(value)?)
}
fn file_hash(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
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
fn drain(mut pipe: impl Read) -> io::Result<(Vec<u8>, usize)> {
    let mut prefix = Vec::new();
    let mut total = 0;
    let mut buf = [0; 8192];
    loop {
        let n = pipe.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let keep = n.min(CAP - prefix.len());
        prefix.extend_from_slice(&buf[..keep]);
        total += n;
    }
    Ok((prefix, total))
}
fn run(root: &Path, name: &str, command: &mut Command, input: &[u8]) -> Result<Vec<u8>> {
    if budget_expired() {
        return Err("stage-one 180 s budget exhausted".into());
    }
    let mut phase = diagnostics::Span::new("diagnostic_client_child");
    phase.field("operation", name);
    phase.mark("client_spawn_begin");
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let id = child.id();
    phase.field("child_pid", id);
    phase.mark("client_spawned");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let started = Instant::now();
    let captured = std::thread::scope(|scope| -> io::Result<_> {
        let writer = scope.spawn(move || {
            let mut stdin = stdin;
            stdin.write_all(input)
        });
        let out = scope.spawn(|| drain(stdout));
        let err = scope.spawn(|| drain(stderr));
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > Duration::from_secs(90) || budget_expired() {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{id}")])
                    .status();
                timed_out = true;
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        phase.mark("client_exit_observed");
        let output = out
            .join()
            .map_err(|_| io::Error::other("stdout drainer panic"))??;
        let errors = err
            .join()
            .map_err(|_| io::Error::other("stderr drainer panic"))??;
        let written = writer
            .join()
            .map_err(|_| io::Error::other("stdin writer panic"))?;
        phase.mark("client_full_output_drained");
        Ok((status, output, errors, written, timed_out))
    });
    phase.finish(&captured);
    let (status, (out, out_len), (err, err_len), written, timed_out) = captured?;
    // Persist both streams and status BEFORE interpreting a failed child.
    fs::write(root.join(format!("{name}.stdout")), &out)?;
    fs::write(root.join(format!("{name}.stderr")), &err)?;
    save(
        root,
        &format!("{name}.child.json"),
        &json!({"pid":id,"status":status.code(),
        "success":status.success(),"timed_out":timed_out,"stdout_bytes":out_len,"stderr_bytes":err_len,
        "stdout_truncated":out_len>CAP,"stderr_truncated":err_len>CAP,"stdout_prefix_sha256":hash(&out),
        "stdin_error_kind":written.as_ref().err().map(|e|format!("{:?}", e.kind())),"cpu_ms":null}),
    )?;
    if timed_out {
        return Err(
            format!("{name} exceeded bounded diagnostic budget; output/status retained").into(),
        );
    }
    if !status.success() {
        return Err(format!("{name} failed: {status}; retained stdout/stderr/status").into());
    }
    written?;
    if out_len > CAP || err_len > CAP {
        return Err(format!("{name} capture exceeded cap").into());
    }
    Ok(out)
}
fn git(root: &Path, name: &str, repo: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    run(
        root,
        name,
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Diagnostic")
            .env("GIT_AUTHOR_EMAIL", "diagnostic@invalid")
            .env("GIT_COMMITTER_NAME", "Diagnostic")
            .env("GIT_COMMITTER_EMAIL", "diagnostic@invalid")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z"),
        input,
    )
}
fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().into())
}

fn live_group(id: u32) -> io::Result<bool> {
    for entry in fs::read_dir("/proc")?.flatten() {
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(") ").map(|p| p.1) else {
            continue;
        };
        let values: Vec<_> = rest.split_whitespace().take(3).collect();
        if values.len() == 3 && values[0] != "Z" && values[2].parse::<u32>().ok() == Some(id) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn log_drain(
    mut pipe: impl Read + Send + 'static,
    log: Arc<Mutex<(File, usize)>>,
) -> JoinHandle<io::Result<usize>> {
    std::thread::spawn(move || {
        let mut buf = [0; 8192];
        let mut total = 0;
        loop {
            let n = pipe.read(&mut buf)?;
            if n == 0 {
                return Ok(total);
            }
            total += n;
            let mut log = log.lock().map_err(|_| io::Error::other("log lock"))?;
            let keep = n.min(CAP - log.1);
            log.0.write_all(&buf[..keep])?;
            log.1 += keep;
        }
    })
}
struct Owned {
    child: Child,
    root: PathBuf,
    label: &'static str,
    stopped: bool,
    readers: Vec<JoinHandle<io::Result<usize>>>,
}
impl Owned {
    fn start(
        root: &Path,
        label: &'static str,
        command: &mut Command,
        ledger: Option<PathBuf>,
    ) -> Result<Self> {
        let log = Arc::new(Mutex::new((
            File::create(root.join(format!("{label}.log")))?,
            0,
        )));
        command
            .current_dir(root)
            .env_remove("GIT_ZERO_PHASE_LEDGER")
            .env("WRANGLER_SEND_METRICS", "false")
            .env("X_LOCAL_EXPLORER", "false")
            .env("X_LOCAL_OBSERVABILITY", "false")
            .env("BROWSER", "none")
            .env("WRANGLER_LOG_PATH", root.join("wrangler-logs"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(path) = ledger {
            command.env("GIT_ZERO_PHASE_LEDGER", path);
        }
        let mut child = command.spawn()?;
        let readers = vec![
            log_drain(child.stdout.take().unwrap(), log.clone()),
            log_drain(child.stderr.take().unwrap(), log),
        ];
        Ok(Self {
            child,
            root: root.into(),
            label,
            stopped: false,
            readers,
        })
    }
    fn ready(&mut self, needle: &str) -> Result<()> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) && !budget_expired() {
            if self.child.try_wait()?.is_some() {
                return Err(
                    format!("{} exited before ready; inspect retained log", self.label).into(),
                );
            }
            if fs::read_to_string(self.root.join(format!("{}.log", self.label)))?.contains(needle) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("{} readiness deadline", self.label).into())
    }
    fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let id = self.child.id();
        let term = Command::new("kill")
            .args(["-TERM", "--", &format!("-{id}")])
            .output()?;
        let start = Instant::now();
        let mut forced = false;
        loop {
            let reaped = self.child.try_wait()?.is_some();
            if reaped && !live_group(id)? {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                forced = true;
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{id}")])
                    .status();
                self.child.wait()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut log_bytes = 0;
        for reader in self.readers.drain(..) {
            log_bytes += reader
                .join()
                .map_err(|_| io::Error::other("log drainer panic"))??;
        }
        self.stopped = true;
        let stopped = !live_group(id)?;
        save(
            &self.root,
            &format!("{}-cleanup.json", self.label),
            &json!({"owned_pgid":id,
            "term_status":term.status.code(),"forced_kill":forced,"leader_reaped":true,
            "owned_group_stopped":stopped,"log_bytes":log_bytes,"log_truncated":log_bytes>CAP}),
        )?;
        if !stopped {
            return Err(io::Error::other("owned process group remains active"));
        }
        Ok(())
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn runtime_processes(groups: &[u32]) -> Value {
    let mut rows = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(rest) = stat.rsplit_once(") ").map(|p| p.1) else {
                continue;
            };
            let pgid = rest
                .split_whitespace()
                .nth(2)
                .and_then(|n| n.parse::<u32>().ok());
            if !pgid.is_some_and(|p| groups.contains(&p)) {
                continue;
            }
            let exe = fs::read_link(entry.path().join("exe")).ok();
            let seal = exe.as_ref().and_then(|p| file_hash(p).ok());
            rows.push(json!({"pid":pid,"pgid":pgid,"exe":exe,"exe_sha256":seal}));
        }
    }
    json!(rows)
}
fn sources(worker: &Path, native: &Path, wrangler: &Path) -> Result<Value> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut rows = Vec::new();
    for name in [
        "src/diagnostics.rs",
        "src/http.rs",
        "src/native.rs",
        "src/cloudflare.rs",
        "src/socket_rpc.rs",
        "src/bin/fetch-phase-diagnostic.rs",
        "Cargo.toml",
        "../../Cargo.lock",
    ] {
        let p = source.join(name).canonicalize()?;
        rows.push(json!({"path":p,"sha256":file_hash(&p)?}));
    }
    for name in [
        "index.js",
        "catalog-pages.js",
        "first-read.js",
        "inline-catalog.js",
        "retirement-proof.js",
        "service-socket.js",
    ] {
        let p = worker.join("src").join(name);
        rows.push(json!({"path":p,"sha256":file_hash(&p)?}));
    }
    let node = executable("node")?;
    let git = executable("git")?;
    let workerd = wrangler
        .ancestors()
        .nth(3)
        .ok_or("wrangler location")?
        .join("@cloudflare/workerd-linux-64/bin/workerd")
        .canonicalize()?;
    Ok(
        json!({"sources":rows,"native":{"path":native,"sha256":file_hash(native)?},
        "runner":{"path":std::env::current_exe()?,"sha256":file_hash(&std::env::current_exe()?)?},
        "node":{"path":node,"sha256":file_hash(&node)?},"git":{"path":git,"sha256":file_hash(&git)?},
        "wrangler":{"path":wrangler,"sha256":file_hash(wrangler)?},
        "workerd":{"path":workerd,"sha256":file_hash(&workerd)?},
        "server_cpu_ms":null,"worker_cpu_ms":null,"do_gb_seconds":null,"full_bill_usd":null}),
    )
}

// Separate exact saved POST control: same gateway, full framed body drain.
// This is proof instrumentation and is never a timed benchmark replacement.
fn http(
    root: &Path,
    name: &str,
    port: u16,
    path: &str,
    body: Option<&[u8]>,
    chunked: bool,
) -> Result<(u16, Vec<u8>)> {
    let mut span = diagnostics::Span::new("diagnostic_http_client");
    span.field("path", path);
    span.mark("client_connect_begin");
    let headers_path = root.join(format!("{name}.headers"));
    let body_path = root.join(format!("{name}.body"));
    let mut retained_headers = Vec::new();
    let mut retained_body = Vec::new();
    let mut status = None;
    let result = (|| -> io::Result<()> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let counts = diagnostics::Counts::default();
        span.counts(&counts);
        let mut io = BufReader::new(diagnostics::Observed::new(stream, counts));
        span.mark("client_connected");
        let mut header = format!(
            "{} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n",
            if body.is_some() { "POST" } else { "GET" }
        );
        if let Some(body) = body {
            span.body("request_body", body);
            fs::write(root.join(format!("{name}.request")), body)?;
            header.push_str("Content-Type: application/x-git-upload-pack-request\r\n");
            if chunked {
                header.push_str("Transfer-Encoding: chunked\r\n");
            } else {
                header.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
        }
        header.push_str("\r\n");
        span.mark("client_request_write_begin");
        io.get_mut().write_all(header.as_bytes())?;
        if let Some(body) = body {
            if chunked {
                for part in body.chunks(23) {
                    write!(io.get_mut(), "{:x}\r\n", part.len())?;
                    io.get_mut().write_all(part)?;
                    io.get_mut().write_all(b"\r\n")?;
                }
                io.get_mut().write_all(b"0\r\n\r\n")?;
            } else {
                io.get_mut().write_all(body)?;
            }
        }
        span.mark("client_request_written");
        loop {
            span.mark("client_response_header_read_begin");
            let mut line = String::new();
            io.by_ref().take(8193).read_line(&mut line)?;
            retained_headers.extend_from_slice(line.as_bytes());
            if !line.starts_with("HTTP/1.") || line.len() > 8192 {
                return Err(io::Error::other("bad status line"));
            }
            let code: u16 = line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| io::Error::other("missing status"))?
                .parse()
                .map_err(io::Error::other)?;
            let mut length = None;
            let mut chunks = false;
            loop {
                line.clear();
                io.by_ref().take(8193).read_line(&mut line)?;
                retained_headers.extend_from_slice(line.as_bytes());
                if line.is_empty() || line.len() > 8192 || retained_headers.len() > 65536 {
                    return Err(io::Error::other("bad headers"));
                }
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line
                    .split_once(':')
                    .ok_or_else(|| io::Error::other("bad header"))?;
                if key.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
                }
                if key.eq_ignore_ascii_case("transfer-encoding") {
                    chunks = value.trim().eq_ignore_ascii_case("chunked");
                }
            }
            if code == 100 {
                span.mark("client_continue_received");
                continue;
            }
            status = Some(code);
            span.field("status", code);
            span.mark("client_response_headers_complete");
            if chunks {
                loop {
                    line.clear();
                    io.by_ref().take(129).read_line(&mut line)?;
                    if line.len() > 128 {
                        return Err(io::Error::other("oversized chunk line"));
                    }
                    let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                        .map_err(io::Error::other)?;
                    if n == 0 {
                        loop {
                            line.clear();
                            io.by_ref().take(8193).read_line(&mut line)?;
                            if line == "\r\n" {
                                break;
                            }
                            if line.is_empty() || line.len() > 8192 {
                                return Err(io::Error::other("bad trailer"));
                            }
                        }
                        break;
                    }
                    if n > CAP - retained_body.len() {
                        return Err(io::Error::other("body cap"));
                    }
                    let before = retained_body.len();
                    io.by_ref().take(n as u64).read_to_end(&mut retained_body)?;
                    if retained_body.len() - before != n {
                        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                    }
                    let mut ending = [0; 2];
                    io.read_exact(&mut ending)?;
                    if ending != *b"\r\n" {
                        return Err(io::Error::other("bad chunk ending"));
                    }
                }
            } else if let Some(n) = length {
                if n > CAP {
                    return Err(io::Error::other("body cap"));
                }
                // Keep the actual prefix even if EOF arrives early.
                let mut take = io.by_ref().take(n as u64);
                take.read_to_end(&mut retained_body)?;
                if retained_body.len() != n {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
            } else {
                io.take((CAP + 1) as u64).read_to_end(&mut retained_body)?;
                if retained_body.len() > CAP {
                    retained_body.truncate(CAP);
                    return Err(io::Error::other("body cap"));
                }
            }
            span.mark("client_full_body_complete");
            span.body("response_body", &retained_body);
            break;
        }
        Ok(())
    })();
    fs::write(headers_path, &retained_headers)?;
    fs::write(body_path, &retained_body)?;
    save(
        root,
        &format!("{name}.http.json"),
        &json!({"status":status,"body_bytes_retained":retained_body.len(),
        "body_sha256":hash(&retained_body),"complete":result.is_ok(),"error_kind":result.as_ref().err().map(|e|format!("{:?}",e.kind())),
        "raw_os_error":result.as_ref().err().and_then(|e|e.raw_os_error())}),
    )?;
    span.finish(&result);
    result?;
    Ok((status.ok_or("missing final status")?, retained_body))
}
fn packet(value: &str) -> Vec<u8> {
    format!("{:04x}{value}", value.len() + 4).into_bytes()
}
fn fetch_body(want: &str) -> Vec<u8> {
    let mut body = packet(&format!(
        "want {want} no-progress ofs-delta agent=fetch-phase-proof\n"
    ));
    body.extend(b"0000");
    body.extend(packet("done\n"));
    body
}
fn ledger_check(root: &Path) -> Result<Value> {
    let bytes = fs::read(root.join("native-phase.jsonl"))?;
    let rows: Vec<Value> = bytes
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
        .map(serde_json::from_slice)
        .collect::<std::result::Result<_, _>>()?;
    let mut kinds = std::collections::BTreeMap::new();
    let mut complete_fetch = 0;
    let mut metric_error = 0;
    let mut socket_terminal = 0;
    for row in &rows {
        let kind = row["kind"].as_str().ok_or("ledger kind")?;
        *kinds.entry(kind.to_string()).or_insert(0usize) += 1;
        if !row["server_cpu_ms"].is_null() || !row["child_cpu_ms"].is_null() {
            return Err("CPU must remain null".into());
        }
        if row["boot_id"].as_str().is_none() || row["process_boot_anchor_ns"].as_u64().is_none() {
            return Err("missing boot clock receipt".into());
        }
        let stages: Vec<&str> = row["stages"]
            .as_array()
            .ok_or("stages")?
            .iter()
            .filter_map(|s| s["stage"].as_str())
            .collect();
        if kind == "http_request"
            && row["fields"]["path"] == "/repo.git/git-upload-pack"
            && row["fields"]["response_status"] == 200
        {
            let expected = [
                "accepted",
                "request_headers_complete",
                "request_body_complete",
                "handler_begin",
                "handler_end",
                "response_headers_written",
                "response_body_written",
            ];
            let positions: Option<Vec<usize>> = expected
                .iter()
                .map(|e| stages.iter().position(|s| s == e))
                .collect();
            let positions = positions.ok_or("fetch missing phase boundary")?;
            if !positions.windows(2).all(|p| p[0] < p[1]) {
                return Err("fetch phase order".into());
            }
            if row["fields"]["request_body_sha256"].as_str().is_none() {
                return Err("fetch missing body hash".into());
            }
            complete_fetch += 1;
        }
        if kind == "service_callback"
            && row["fields"]["route"] == "metrics"
            && row["outcome"] == "error"
        {
            if row["fields"]["error_stage"] != "connect_begin"
                || row["fields"]["raw_os_error"].as_i64().is_none()
            {
                return Err("failed metrics callback missing exact stage/OS error".into());
            }
            metric_error += 1;
        }
        if kind == "service_socket"
            && stages.contains(&"socket_terminal_received")
            && stages.contains(&"socket_ack_written")
        {
            socket_terminal += 1;
        }
        let mut previous = 0;
        for s in row["stages"].as_array().unwrap() {
            let n = s["process_ns"].as_u64().ok_or("phase clock")?;
            if n < previous {
                return Err("phase clock regression".into());
            }
            previous = n;
        }
    }
    for k in [
        "http_request",
        "service_callback",
        "view_materialize",
        "git_child",
        "native_read",
    ] {
        if !kinds.contains_key(k) {
            return Err(format!("missing {k} proof").into());
        }
    }
    if complete_fetch == 0 || metric_error == 0 || socket_terminal == 0 {
        return Err("missing completed fetch, failed callback, or WS terminal/ACK proof".into());
    }
    Ok(
        json!({"kinds":kinds,"complete_fetches":complete_fetch,"metrics_connect_errors":metric_error,"socket_terminal_and_ack":socket_terminal,"phase_order_pass":true,"cpu_ms":null}),
    )
}
fn cohort(
    root: &Path,
    native: &Path,
    wrangler: &Path,
    worker: &Path,
    traced: bool,
) -> Result<Value> {
    fs::create_dir(root)?;
    let config = fs::read_to_string(worker.join("wrangler.toml"))?.replace(
        "main = \"src/index.js\"",
        &format!("main = {:?}", worker.join("src/index.js")),
    );
    fs::write(root.join("wrangler.toml"), config)?;
    let mut runtime = Owned::start(
        root,
        "wrangler",
        Command::new("node")
            .arg(wrangler)
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
            .arg("--show-interactive-dev-session=false"),
        None,
    )?;
    runtime.ready("Ready on http://127.0.0.1:8853")?;
    // Native alone opts in; the common Worker and its request headers are unchanged.
    let mut host = Owned::start(
        root,
        "native",
        Command::new(native)
            .args([
                "--cloudflare",
                "http://127.0.0.1:8853",
                "--repo",
                "fetchdiag",
                "--view-cache",
                "cold",
            ])
            .arg(root.join("native-state"))
            .arg("8854"),
        traced.then(|| root.join("native-phase.jsonl")),
    )?;
    host.ready("http://127.0.0.1:8854/repo.git")?;
    save(
        root,
        "runtime-active-before.json",
        &runtime_processes(&[runtime.child.id(), host.child.id()]),
    )?;
    let source = root.join("source");
    fs::create_dir(&source)?;
    git(
        root,
        "source-init",
        &source,
        &["init", "-q", "-b", "main"],
        &[],
    )?;
    let mut wants = Vec::new();
    let mut trees = Vec::new();
    for generation in 1..=3 {
        let mut state = 0x12345678u64 + generation as u64;
        let bytes: Vec<u8> = (0..65536)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        fs::write(source.join(format!("generation-{generation}.bin")), bytes)?;
        git(
            root,
            &format!("add-{generation}"),
            &source,
            &["add", "."],
            &[],
        )?;
        git(
            root,
            &format!("commit-{generation}"),
            &source,
            &["commit", "-q", "-m", &format!("generation {generation}")],
            &[],
        )?;
        wants.push(text(git(
            root,
            &format!("want-{generation}"),
            &source,
            &["rev-parse", "HEAD"],
            &[],
        )?)?);
        trees.push(text(git(
            root,
            &format!("tree-{generation}"),
            &source,
            &["rev-parse", "HEAD^{tree}"],
            &[],
        )?)?);
        git(
            root,
            &format!("push-{generation}"),
            &source,
            &[
                "-c",
                "http.version=HTTP/1.1",
                "-c",
                "pack.window=0",
                "push",
                "--quiet",
                GATEWAY,
                "HEAD:refs/heads/main",
                &format!("HEAD:refs/heads/history-{generation:02}"),
            ],
            &[],
        )?;
    }
    let want = &wants[0];
    let tree = &trees[0];
    let body = fetch_body(want);
    save(
        root,
        "fixture.json",
        &json!({"kind":"tiny stage-one proof; not retained tail replay","generations":3,
        "file_bytes_per_generation":65536,"want":want,"tree":tree,"sealed_request_bytes":body.len(),"sealed_request_sha256":hash(&body)}),
    )?;
    let client = root.join("client");
    fs::create_dir(&client)?;
    git(root, "client-init", &client, &["init", "-q"], &[])?;
    let mut fetch = Command::new("git");
    fetch
        .arg("-C")
        .arg(&client)
        .args([
            "-c",
            "http.version=HTTP/1.1",
            "fetch",
            "--quiet",
            GATEWAY,
            "refs/heads/history-01:refs/heads/verified",
        ])
        .env("GIT_TRACE2_EVENT", root.join("client-trace2.jsonl"))
        .env("GIT_TRACE_CURL", root.join("client-curl.log"))
        .env("GIT_TRACE_CURL_NO_DATA", "1");
    run(root, "real-git-fetch", &mut fetch, &[])?;
    let verified = text(git(
        root,
        "verified-want",
        &client,
        &["rev-parse", "refs/heads/verified"],
        &[],
    )?)?;
    let verified_tree = text(git(
        root,
        "verified-tree",
        &client,
        &["rev-parse", "refs/heads/verified^{tree}"],
        &[],
    )?)?;
    if &verified != want || &verified_tree != tree {
        return Err("real Git want/tree mismatch".into());
    }
    git(
        root,
        "client-fsck",
        &client,
        &["fsck", "--strict", "--no-reflogs"],
        &[],
    )?;
    let archive = git(root, "source-archive", &source, &["archive", want], &[])?;
    if git(root, "client-archive", &client, &["archive", want], &[])? != archive {
        return Err("Git archive mismatch".into());
    }
    let mut framing = Vec::new();
    for (name, chunked) in [("content-length", false), ("chunked", true)] {
        let (status, response) = http(
            root,
            name,
            8853,
            "/fetchdiag/repo.git/git-upload-pack",
            Some(&body),
            chunked,
        )?;
        if status != 200 {
            return Err(format!("{name} returned {status}; actual headers/body saved").into());
        }
        let at = response
            .windows(4)
            .position(|p| p == b"PACK")
            .ok_or("missing pack")?;
        let verify = root.join(format!("verify-{name}"));
        fs::create_dir(&verify)?;
        git(root, &format!("{name}-init"), &verify, &["init", "-q"], &[])?;
        git(
            root,
            &format!("{name}-index-pack"),
            &verify,
            &["index-pack", "--stdin", "--strict", "--fix-thin"],
            &response[at..],
        )?;
        git(
            root,
            &format!("{name}-update-ref"),
            &verify,
            &["update-ref", "refs/heads/verified", want],
            &[],
        )?;
        git(
            root,
            &format!("{name}-fsck"),
            &verify,
            &["fsck", "--strict", "--no-reflogs"],
            &[],
        )?;
        if git(
            root,
            &format!("{name}-archive"),
            &verify,
            &["archive", want],
            &[],
        )? != archive
        {
            return Err("framing control archive mismatch".into());
        }
        framing.push(json!({"framing":name,"status":status,"body_bytes":response.len(),"body_sha256":hash(&response),"want":want,"tree":tree,"archive_sha256":hash(&archive)}));
    }
    let (before_status, before) = http(
        root,
        "metrics-before-failure",
        8854,
        "/metrics",
        None,
        false,
    )?;
    if before_status != 200 {
        return Err("pre-failure observer failed".into());
    }
    let before: Value = serde_json::from_slice(&before)?;
    if before["cloudflare_services"]["accounting_complete"] != true {
        return Err("incomplete pre-failure receipt".into());
    }
    save(
        root,
        "runtime-active-after.json",
        &runtime_processes(&[runtime.child.id(), host.child.id()]),
    )?;
    // One bounded real callback failure validates retention and exact error stage.
    runtime.stop()?;
    let (failure_status, failure_body) = http(
        root,
        "metrics-expected-failure",
        8854,
        "/metrics",
        None,
        false,
    )?;
    if failure_status != 503 || failure_body.is_empty() {
        return Err("failure capture proof expected actual 503 body".into());
    }
    host.stop()?;
    let ledger = if traced {
        ledger_check(root)?
    } else {
        Value::Null
    };
    if !traced && root.join("native-phase.jsonl").exists() {
        return Err("disabled Native wrote ledger".into());
    }
    Ok(
        json!({"traced":traced,"want":want,"tree":tree,"archive_sha256":hash(&archive),"framing":framing,
        "failure_status":failure_status,"failure_body_sha256":hash(&failure_body),"failure_body_bytes":failure_body.len(),
        "pre_failure_observer_complete":true,"after_failure_counters":null,"after_failure_counters_complete":false,
        "phase_ledger":ledger,"server_cpu_ms":null,"worker_cpu_ms":null,"full_bill_usd":null}),
    )
}
fn main() -> Result<()> {
    START.get_or_init(Instant::now);
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--cheap-proof")) {
        return Err("use --cheap-proof NEW_OUTPUT_DIR --wrangler PATH; expensive replays require independent approval".into());
    }
    let root = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--wrangler")) {
        return Err("missing --wrangler".into());
    }
    let wrangler = PathBuf::from(args.next().ok_or("missing Wrangler path")?).canonicalize()?;
    if args.next().is_some() || root.exists() {
        return Err("output must be new; unexpected arguments rejected".into());
    }
    fs::create_dir(&root)?;
    let root = root.canonicalize()?;
    if std::env::var_os("GIT_ZERO_PHASE_LEDGER").map(PathBuf::from)
        != Some(root.join("client-phase.jsonl"))
    {
        return Err("set GIT_ZERO_PHASE_LEDGER to NEW_OUTPUT_DIR/client-phase.jsonl so client boundaries are retained".into());
    }
    // Sequential cohorts own only these fixed application ports.
    let reservations = [
        TcpListener::bind(("127.0.0.1", 8853))?,
        TcpListener::bind(("127.0.0.1", 8854))?,
    ];
    drop(reservations);
    let native = std::env::current_exe()?
        .with_file_name("repository-host")
        .canonicalize()?;
    let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
    let before = sources(&worker, &native, &wrangler)?;
    save(&root, "source-runtime-before.json", &before)?;
    for (name, args) in [
        ("git-version", vec!["--version"]),
        ("node-version", vec!["--version"]),
    ] {
        run(
            &root,
            name,
            Command::new(if name == "git-version" { "git" } else { "node" }).args(args),
            &[],
        )?;
    }
    run(
        &root,
        "wrangler-version",
        Command::new("node")
            .arg(&wrangler)
            .arg("--version")
            .env("WRANGLER_SEND_METRICS", "false")
            .env("WRANGLER_LOG_PATH", root.join("version-logs")),
        &[],
    )?;
    let result = (|| -> Result<Value> {
        let baseline = cohort(&root.join("disabled"), &native, &wrangler, &worker, false)?;
        let traced = cohort(&root.join("traced"), &native, &wrangler, &worker, true)?;
        for key in [
            "want",
            "tree",
            "archive_sha256",
            "framing",
            "failure_status",
            "failure_body_sha256",
        ] {
            if baseline[key] != traced[key] {
                return Err(format!("observational equivalence failed for {key}").into());
            }
        }
        let after = sources(&worker, &native, &wrangler)?;
        save(&root, "source-runtime-after.json", &after)?;
        if before != after {
            return Err("source/runtime changed during proof".into());
        }
        let free = [
            TcpListener::bind(("127.0.0.1", 8853))?,
            TcpListener::bind(("127.0.0.1", 8854))?,
        ];
        drop(free);
        save(
            &root,
            "cleanup.json",
            &json!({"owned_application_ports":[8853,8854],"ports_free":true,"both_cohort_process_leaders_reaped":true}),
        )?;
        Ok(
            json!({"schema":1,"stage":"cheap production-gateway proof only","approved_base":"f124d3b71ce1acc44ab20d6850b2b619b4bd8ec5",
            "pass":true,"observational_equivalence":true,"disabled":baseline,"traced":traced,
            "tail_replays_run":0,"performance_claim":false,"server_cpu_ms":null,"worker_cpu_ms":null,"do_gb_seconds":null,"full_bill_usd":null}),
        )
    })();
    match result {
        Ok(value) => {
            save(&root, "proof.json", &value)?;
            println!("cheap proof PASS: {}", root.display());
            Ok(())
        }
        Err(error) => {
            save(
                &root,
                "proof.json",
                &json!({"pass":false,"error":error.to_string(),"complete":false,"counters_after_failure":null,"performance_claim":false}),
            )?;
            Err(error)
        }
    }
}
