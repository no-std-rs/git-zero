//! Rust owns the real production-workerd socket proof. No benchmark or scorer
//! input is generated. This allocator observes the runner's live requested
//! heap bytes, including opaque Tungstenite/BytesMut ownership after calls.
use git_zero_host::{
    Repository,
    accounting::ServiceUsage,
    cloudflare::HttpServices,
    hash,
    storage::{
        Chunk, CloudflareBindings, DurableStateStore, Manifest, R2BlobStore, UploadRetiringCas,
    },
};
use serde_json::{Value, json};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[path = "support/socket_protocol.rs"]
mod protocol;

struct Counted;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
fn allocated(size: usize) {
    let total = LIVE.fetch_add(size, Ordering::SeqCst) + size;
    PEAK.fetch_max(total, Ordering::SeqCst);
}
// Test-only transparent allocator: the System layout is unchanged, and each
// successful allocation/reallocation is paired with its actual layout size.
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(p, layout, size) };
        if !out.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
            allocated(size);
        }
        out
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

struct Runtime(Child);
impl Runtime {
    fn start(root: &Path) -> Self {
        let reservation =
            std::net::TcpListener::bind("127.0.0.1:8839").expect("owned port 8839 free");
        let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare");
        let config = fs::read_to_string(worker.join("wrangler.toml"))
            .unwrap()
            .replace(
                "main = \"src/index.js\"",
                &format!("main = {:?}", worker.join("src/index.js")),
            );
        fs::write(root.join("wrangler.toml"), config).unwrap();
        let cli = std::env::var_os("GIT_ZERO_SOCKET_WRANGLER")
            .map(PathBuf::from)
            .unwrap_or_else(|| worker.join("node_modules/wrangler/bin/wrangler.js"));
        let log = File::create(root.join("wrangler.log")).unwrap();
        drop(reservation);
        let child = Command::new("setsid")
            .arg("node")
            .arg(cli)
            .args(["dev", "--config"])
            .arg(root.join("wrangler.toml"))
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                "8839",
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
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut result = Self(child);
        for _ in 0..200 {
            assert!(
                result.0.try_wait().unwrap().is_none(),
                "Worker exited: {}",
                root.display()
            );
            if fs::read_to_string(root.join("wrangler.log"))
                .unwrap()
                .contains("Ready on http://127.0.0.1:8839")
            {
                return result;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Worker did not become ready: {}", root.display());
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.wait();
    }
}

fn sources() -> Value {
    fn visit(base: &Path, path: &Path, out: &mut serde_json::Map<String, Value>) {
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
    let mut out = serde_json::Map::new();
    for rel in [
        "src",
        "cloudflare/src",
        "cloudflare/wrangler.toml",
        "cloudflare/package.json",
        "cloudflare/package-lock.json",
        "Cargo.toml",
        "tests/socket_rpc_cloudflare.rs",
        "tests/support/socket_protocol.rs",
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
    Value::Object(out)
}
fn write(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn usage(services: &HttpServices) -> (Value, ServiceUsage) {
    let v = services.usage().unwrap();
    let s = serde_json::from_value(v.clone()).unwrap();
    (v, s)
}

fn boundary_publications(root: &Path, cases: &mut Vec<Value>) {
    fn wire_metadata(next: &Manifest, pending: &git_zero_host::storage::PendingChunk) -> usize {
        serde_json::to_vec(&json!({"version":1,"expected":0,"next":next,"chunk":pending.chunk()}))
            .unwrap()
            .len()
    }
    for (name, metadata_bound, payload) in [
        ("metadata", 1_500_000, 8usize),
        ("whole-upload", 2048, 16 * 1024 * 1024),
    ] {
        let services = Arc::new(
            HttpServices::new(
                "http://127.0.0.1:8839",
                &format!("socket-bound-{name}"),
                "git-zero-local-services",
            )
            .unwrap(),
        );
        services.enable_socket_rpc(true);
        services.register("http://127.0.0.1:8840").unwrap();
        let mut repository = Repository::new(
            R2BlobStore::new(services.clone()),
            DurableStateStore::new(services.clone()),
        );
        repository.chunk_bytes = 16 * 1024 * 1024;
        let data = vec![b'B'; payload - 3];
        let (stored, pending) = repository
            .stage_bundled_pack("boundary".into(), &data, b"idx", 0)
            .unwrap()
            .into_parts();
        let mut next = Manifest {
            generation: 1,
            packs: vec![stored],
            ..Manifest::default()
        };
        if name == "metadata" {
            for i in 0..10000 {
                next.refs.insert(
                    format!("refs/heads/m{i:05}-{}", "x".repeat(80)),
                    "1".repeat(40),
                );
            }
        }
        let base_name = "refs/heads/padding".to_owned();
        next.refs.insert(base_name.clone(), "1".repeat(40));
        let before_length = wire_metadata(&next, &pending);
        assert!(before_length < metadata_bound);
        next.refs.remove(&base_name);
        let padded = format!("{base_name}{}", "x".repeat(metadata_bound - before_length));
        assert!(padded.len() < 65520 - 46);
        next.refs.insert(padded.clone(), "1".repeat(40));
        assert_eq!(wire_metadata(&next, &pending), metadata_bound);
        let (raw_before, before) = usage(&services);
        let result = services
            .try_compare_and_swap_upload_retiring(0, &next, &pending)
            .unwrap();
        assert!(matches!(result, UploadRetiringCas::Published { .. }));
        let (raw_after, after) = usage(&services);
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.do_requests, 1);
        assert_eq!(delta.worker_requests, 1);
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_written_bytes, payload as u64);
        assert_eq!(after.accounting_complete, Some(true));
        next.refs.remove(&padded);
        next.refs.insert(format!("{padded}x"), "1".repeat(40));
        assert_eq!(wire_metadata(&next, &pending), metadata_bound + 1);
        let diagnostic_before = services.socket_diagnostics().unwrap();
        assert!(matches!(
            services
                .try_compare_and_swap_upload_retiring(0, &next, &pending)
                .unwrap(),
            UploadRetiringCas::Unsupported
        ));
        assert_eq!(services.socket_diagnostics().unwrap(), diagnostic_before);
        let (raw_unsupported, unsupported) = usage(&services);
        let unchanged = unsupported.checked_workload_delta(&after).unwrap();
        assert_eq!(unchanged.worker_requests, 0);
        assert_eq!(unchanged.do_requests, 0);
        assert_eq!(unchanged.r2_class_a, 0);
        let case = json!({"name":format!("{name}-boundary"),"metadata_bytes":metadata_bound,
            "payload_bytes":payload,"whole_upload_bytes":12+metadata_bound+payload,
            "one_byte_over_returns_unsupported_before_io":true,"before":raw_before,"after":raw_after,
            "after_unsupported":raw_unsupported,"actual_delta":delta,"native_socket":diagnostic_before});
        write(root, &format!("{name}-boundary.json"), &case);
        cases.push(case);
        services.unregister().unwrap();
    }
}

#[test]
#[ignore = "explicit assigned local port 8840, 128-MiB legacy HTTP response"]
fn legacy_scalar_limit_uses_http_before_socket_io() {
    use std::io::{BufRead, BufReader, Write};
    let root = PathBuf::from(
        std::env::var("GIT_ZERO_SOCKET_ARTIFACTS").expect("explicit fresh artifacts root"),
    );
    fs::create_dir_all(&root).unwrap();
    let root = root.join("legacy");
    fs::create_dir(&root).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:8840").expect("owned port 8840 free");
    let server = std::thread::spawn(move || {
        let mut targets = Vec::new();
        for index in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            targets.push(line.trim().to_owned());
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
                    length = value.trim().parse::<u64>().unwrap();
                }
            }
            std::io::copy(
                &mut std::io::Read::take(&mut reader, length),
                &mut std::io::sink(),
            )
            .unwrap();
            let stream = reader.get_mut();
            if index == 0 {
                assert!(targets[0].starts_with("POST /_services/register "));
                let body =
                    serde_json::to_vec(&json!({"registered":true,"service_socket":{"version":1,
                    "protocol":"git-zero-service-rpc.v1","piece_bytes":16384,"header_bytes":4096,
                    "body_bytes":16_779_276,"rpcs":16,"session":"11".repeat(16)}}))
                    .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            } else {
                assert!(targets[1].starts_with("GET /_services/blob/chunks/"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    128 * 1024 * 1024
                )
                .unwrap();
                for _ in 0..8192 {
                    stream.write_all(&[b'L'; 16384]).unwrap();
                }
            }
        }
        targets
    });
    let services =
        HttpServices::new("http://127.0.0.1:8840", "legacy", "local-test-token").unwrap();
    services.enable_socket_rpc(true);
    services.register("http://127.0.0.1:8840").unwrap();
    let bytes = services
        .r2_get(&format!("chunks/{}", "a".repeat(64)))
        .unwrap();
    assert_eq!(bytes.len(), 128 * 1024 * 1024);
    assert!(bytes.iter().all(|b| *b == b'L'));
    let diagnostics = services.socket_diagnostics().unwrap();
    assert_eq!(diagnostics["upgrades_attempted"], 0);
    let targets = server.join().unwrap();
    write(
        &root,
        "legacy-128mib-http.json",
        &json!({"kind":"Rust loopback HTTP compatibility control; not workerd or benchmark",
        "body_bytes":bytes.len(),"sha256":hash(&bytes),"actual_requests":targets,"native_socket":diagnostics,
        "no_head_no_upgrade_no_retry":true}),
    );
}

#[test]
#[ignore = "explicit production workerd on owned 8839; no benchmark"]
fn production_socket_topology_and_maximum_buffers() {
    let root = PathBuf::from(
        std::env::var("GIT_ZERO_SOCKET_ARTIFACTS").expect("fresh explicit artifact root"),
    );
    fs::create_dir_all(&root).unwrap();
    let root = root.join("production");
    fs::create_dir(&root).unwrap();
    let before_sources = sources();
    write(&root, "source-before.json", &before_sources);
    let _runtime = Runtime::start(&root);
    let mut cases = Vec::new();
    let small = b"actual one-upgrade two-RPC receipt proof";
    let descriptor = Chunk {
        key: format!("chunks/{}", hash(small)),
        len: small.len() as u64,
    };
    let mut controls = Vec::new();
    for (name, enabled) in [("http", false), ("socket", true)] {
        let services = HttpServices::new(
            "http://127.0.0.1:8839",
            &format!("socket-{name}"),
            "git-zero-local-services",
        )
        .unwrap();
        services.enable_socket_rpc(enabled);
        services.register("http://127.0.0.1:8840").unwrap();
        let (raw_before, before) = usage(&services);
        services.r2_put(&descriptor.key, small).unwrap();
        assert_eq!(
            services
                .r2_get_many_verified(std::slice::from_ref(&descriptor))
                .unwrap()
                .into_data(std::slice::from_ref(&descriptor))
                .unwrap(),
            vec![small.to_vec()]
        );
        let (raw_after, after) = usage(&services);
        let startup = after.checked_workload_delta(&before).unwrap();
        assert_eq!(startup.worker_requests, if enabled { 1 } else { 2 });
        assert_eq!(startup.worker_service_requests, startup.worker_requests);
        assert_eq!(startup.do_requests, 2);
        assert_eq!(startup.do_rows_read, Some(15));
        assert_eq!(startup.do_rows_written, Some(3));
        assert_eq!(startup.r2_class_a, 1);
        assert_eq!(startup.r2_class_b, 1);
        assert_eq!(startup.r2_written_bytes, small.len() as u64);
        assert_eq!(startup.r2_read_bytes, small.len() as u64);
        assert_eq!(after.accounting_complete, Some(true));
        controls.push(startup.clone());
        cases.push(json!({"name":name,"before":raw_before,"after":raw_after,"actual_startup_delta":startup,"native_socket":services.socket_diagnostics().unwrap()}));
        if enabled {
            let maximum = vec![b'R'; 16 * 1024 * 1024];
            let max_chunk = Chunk {
                key: format!("chunks/{}", hash(&maximum)),
                len: maximum.len() as u64,
            };
            let baseline = LIVE.load(Ordering::SeqCst);
            PEAK.store(baseline, Ordering::SeqCst);
            services.r2_put(&max_chunk.key, &maximum).unwrap();
            let after_put = LIVE.load(Ordering::SeqCst);
            {
                let read = services
                    .r2_get_many_verified(std::slice::from_ref(&max_chunk))
                    .unwrap()
                    .into_data(std::slice::from_ref(&max_chunk))
                    .unwrap();
                assert_eq!(read[0], maximum);
            }
            let after_get = LIVE.load(Ordering::SeqCst);
            assert!(
                after_put.saturating_sub(baseline) < 256 * 1024,
                "retained PUT heap exceeded reservation"
            );
            assert!(
                after_get.saturating_sub(baseline) < 256 * 1024,
                "retained GET/shared allocation exceeded reservation"
            );
            let peak = PEAK.load(Ordering::SeqCst);
            for _ in 0..30 {
                let read = services
                    .r2_get_many_verified(std::slice::from_ref(&descriptor))
                    .unwrap();
                drop(read);
            }
            let (raw_final, final_usage) = usage(&services);
            let diagnostics = services.socket_diagnostics().unwrap();
            assert_eq!(diagnostics["rpcs_completed"], 34);
            assert_eq!(diagnostics["upgrades_attempted"], 3);
            assert_eq!(diagnostics["rotations"], 2);
            assert_eq!(diagnostics["request_rfc_frame_attempts"], 1091);
            assert_eq!(diagnostics["request_rfc_frames_written"], 1091);
            assert_eq!(diagnostics["ack_frame_attempts"], 34);
            assert_eq!(diagnostics["ack_frames_written"], 34);
            assert_eq!(
                diagnostics["response_binary_messages_read"],
                raw_final["socket_rpc"]["outgoing_messages"]
            );
            assert_eq!(raw_final["socket_rpc"]["dispatched_rpcs"], 34);
            assert_eq!(raw_final["socket_rpc"]["upgrades_accepted"], 3);
            assert_eq!(raw_final["socket_rpc"]["completed_rpcs"], 34);
            assert_eq!(final_usage.accounting_complete, Some(true));
            cases.push(json!({"name":"maximum-and-rotations","chunk_bytes":maximum.len(),"chunk_sha256":hash(&maximum),
                "allocator_baseline_live_requested_bytes":baseline,"allocator_after_max_put_live_requested_bytes":after_put,
                "allocator_after_max_get_and_drop_live_requested_bytes":after_get,"maximum_transient_live_requested_bytes":peak,
                "conservative_transport_reservation_bytes":256*1024,"actual":raw_final,"native_socket":diagnostics}));
            let before_error = LIVE.load(Ordering::SeqCst);
            let wrong_key = format!("chunks/{}", "0".repeat(64));
            assert!(services.r2_put(&wrong_key, &maximum).is_err());
            let after_error = LIVE.load(Ordering::SeqCst);
            assert!(
                after_error.saturating_sub(before_error) < 256 * 1024,
                "maximum failed frame retained hidden allocation"
            );
            assert_eq!(services.socket_diagnostics().unwrap()["connected"], false);
            let read = services
                .r2_get_many_verified(std::slice::from_ref(&descriptor))
                .unwrap();
            drop(read);
            let error_usage = services.usage().unwrap();
            cases.push(
                json!({"name":"maximum-error-no-replay-and-next-distinct-reconnect",
                "live_before_error":before_error,"live_after_error":after_error,
                "actual":error_usage,"native_socket":services.socket_diagnostics().unwrap()}),
            );
        }
        services.unregister().unwrap();
    }
    controls[0].worker_requests = controls[1].worker_requests;
    controls[0].worker_service_requests = controls[1].worker_service_requests;
    assert_eq!(
        controls[0], controls[1],
        "unchanged physical DO/R2/SQL operations"
    );
    boundary_publications(&root, &mut cases);
    write(
        &root,
        "topology-before-protocol.json",
        &json!({"cases":cases}),
    );
    protocol::protocol_rejections(&root, &mut cases);
    write(
        &root,
        "topology-and-maximum.json",
        &json!({"kind":"production workerd correctness topology only; no benchmark or score","cases":cases}),
    );
    let after_sources = sources();
    write(&root, "source-after.json", &after_sources);
    assert_eq!(before_sources, after_sources);
}

#[test]
#[ignore = "explicit owned local port 8840; counted unsupported Upgrade versus auth failure"]
fn unsupported_upgrade_falls_back_before_rpc_but_auth_failure_does_not() {
    use std::io::{BufRead, BufReader, Read, Write};
    let root =
        PathBuf::from(std::env::var("GIT_ZERO_SOCKET_ARTIFACTS").expect("explicit artifact root"));
    fs::create_dir_all(&root).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:8840").unwrap();
    let server = std::thread::spawn(move || {
        let mut actual = Vec::new();
        for status in [429, 401] {
            for index in 0..if status == 429 { 3 } else { 2 } {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let target = line.trim().to_owned();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
                        length = n.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let stream = reader.get_mut();
                actual.push(json!({"status_control":status,"target":target,"body_bytes":body.len(),"body_sha256":hash(&body)}));
                if index == 0 {
                    assert!(target.starts_with("POST /_services/register "));
                    let response=serde_json::to_vec(&json!({"registered":true,"service_socket":{"version":1,
                        "protocol":"git-zero-service-rpc.v1","piece_bytes":16384,"header_bytes":4096,
                        "body_bytes":16_779_276,"rpcs":16,"session":"11".repeat(16)}})).unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .unwrap();
                    stream.write_all(&response).unwrap();
                } else if index == 1 {
                    assert!(target.starts_with("GET /_services/socket "));
                    write!(stream,"HTTP/1.1 {status} Test-Control\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    assert!(target.starts_with("PUT /_services/blob/chunks/"));
                    assert_eq!(body, b"before-RPC fallback");
                    stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
            }
        }
        actual
    });
    let mut rows = Vec::new();
    for status in [429, 401] {
        let services =
            HttpServices::new("http://127.0.0.1:8840", "fallback", "local-test-token").unwrap();
        services.enable_socket_rpc(true);
        services.register("http://127.0.0.1:8840").unwrap();
        let payload = b"before-RPC fallback";
        let outcome = services.r2_put(&format!("chunks/{}", hash(payload)), payload);
        assert_eq!(outcome.is_ok(), status == 429);
        let diagnostics = services.socket_diagnostics().unwrap();
        assert_eq!(diagnostics["upgrades_attempted"], 1);
        assert_eq!(diagnostics["rpcs_completed"], 0);
        assert_eq!(diagnostics["connected"], false);
        assert_eq!(
            diagnostics["unsupported_upgrades"],
            u64::from(status == 429)
        );
        rows.push(
            json!({"status_control":status,"success":outcome.is_ok(),"diagnostics":diagnostics}),
        );
    }
    write(
        &root,
        "pre-rpc-upgrade-fallback.json",
        &json!({"kind":"Rust HTTP peer compatibility control; not workerd or benchmark",
        "controls":rows,"actual_requests":server.join().unwrap(),"application_rpc_frames_sent":0}),
    );
}
