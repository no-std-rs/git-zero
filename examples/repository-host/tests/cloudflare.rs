//! Real local workerd contracts. Run explicitly, against `wrangler dev --local`:
//! GIT_ZERO_CLOUDFLARE_ORIGIN=http://127.0.0.1:8787 \
//! GIT_ZERO_CLOUDFLARE_TOKEN=git-zero-local-services \
//! cargo test -p git-zero-host --test cloudflare real_ -- --ignored --test-threads=1
//! Missing endpoint/token is an error on this opt-in path, never a silent skip.
//! For process persistence, also set GIT_ZERO_CLOUDFLARE_RESTART_REPO to a fresh
//! stable namespace; run seed_before_workerd_restart, restart Wrangler with the
//! same persist-dir, then run verify_after_workerd_restart as separate commands.
use git_zero_host::{
    Repository,
    accounting::{OperationUsage, ServiceUsage},
    cloudflare::HttpServices,
    hash,
    http::Request,
    native::{NativeHost, command},
    storage::{BlobStore, Chunk, DurableStateStore, Manifest, R2BlobStore, StateStore},
};
use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct RealWorker {
    origin: String,
    token: String,
    namespace: String,
}

impl RealWorker {
    fn new(label: &str) -> Self {
        let required = |name| {
            std::env::var(name).unwrap_or_else(|_| {
                panic!("{name} is required; start real local Wrangler and run the ignored test explicitly")
            })
        };
        Self {
            origin: required("GIT_ZERO_CLOUDFLARE_ORIGIN"),
            token: required("GIT_ZERO_CLOUDFLARE_TOKEN"),
            namespace: format!(
                "contract-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
        }
    }

    fn services(&self) -> Arc<HttpServices> {
        Arc::new(HttpServices::new(&self.origin, &self.namespace, &self.token).unwrap())
    }
}

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(directory: &std::path::Path, arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(directory).args(arguments);
    command(process, input).unwrap().stdout
}

const RESTART_DATA: &[u8] = b"persisted through a real local workerd process restart\n";

fn restart_fixture() -> RealWorker {
    let mut worker = RealWorker::new("restart");
    worker.namespace = std::env::var("GIT_ZERO_CLOUDFLARE_RESTART_REPO").expect(
        "GIT_ZERO_CLOUDFLARE_RESTART_REPO must name the same fresh test namespace in both phases",
    );
    // Validate before using the namespace as a local path component.
    worker.services();
    worker
}

fn restart_marker(worker: &RealWorker) -> PathBuf {
    std::env::temp_dir().join(format!("git-zero-worker-restart-{}.json", worker.namespace))
}

fn packet(payload: &str) -> Vec<u8> {
    format!("{:04x}{payload}", payload.len() + 4).into_bytes()
}

fn rpc(service: &str, body: Vec<u8>) -> Request {
    Request {
        method: "POST".into(),
        target: format!("/repo.git/git-{service}-pack"),
        headers: BTreeMap::from([(
            "content-type".into(),
            format!("application/x-git-{service}-pack-request"),
        )]),
        body,
    }
}

fn operation(usage: &ServiceUsage, name: &str) -> OperationUsage {
    usage.operations.get(name).cloned().unwrap_or_default()
}

fn usage(services: &HttpServices) -> ServiceUsage {
    serde_json::from_value(services.usage().unwrap()).unwrap()
}

// Small Rust-only HTTP test glue; curl preserves gateway streaming and exact
// queries while the listener exposes the trusted headers before native dispatch.
fn local_request(
    origin: &str,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Vec<u8>) {
    use std::io::Write;
    use std::process::Stdio;
    let mut process = Command::new("curl");
    process.args([
        "--silent",
        "--show-error",
        "--max-time",
        "20",
        "--path-as-is",
        "--request",
        method,
        "--write-out",
        "\n%{http_code}",
    ]);
    for (name, value) in headers {
        process.arg("--header").arg(format!("{name}: {value}"));
    }
    if !body.is_empty() {
        process.args(["--data-binary", "@-"]);
    }
    let mut child = process
        .arg(format!("{origin}{target}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(body).unwrap();
    let mut output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "curl transport failed: {}",
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

fn gateway_exchange(
    worker: &RealWorker,
    listener: &std::net::TcpListener,
    suffix: &str,
    extra_headers: &[(&str, &str)],
    handler: impl FnOnce(Request) -> git_zero_host::http::Response,
) -> Request {
    use std::io::{BufRead, BufReader, Write};
    let target = format!("/{}/repo.git/{suffix}", worker.namespace);
    let origin = worker.origin.clone();
    let headers: Vec<_> = extra_headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let client = std::thread::spawn(move || {
        let headers: Vec<_> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        local_request(&origin, "GET", &target, &headers, &[])
    });
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "gateway did not forward to the codec"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("codec accept failed: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap().into();
    let target = parts.next().unwrap().into();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(!line.is_empty() && line.len() <= 8192);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().into());
    }
    let request = Request {
        method,
        target,
        headers,
        body: vec![],
    };
    let response = handler(request.clone());
    write!(
        reader.get_mut(),
        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    )
    .unwrap();
    reader.get_mut().write_all(&response.body).unwrap();
    drop(reader);
    let (status, body) = client.join().unwrap();
    assert_eq!(status, response.status);
    assert_eq!(body.as_slice(), response.body.as_ref());
    request
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_gateway_generation_witness_receipts_compatibility_and_queued_generation() {
    use git_zero_host::http::Response;
    const HEADER: &str = "x-git-zero-internal-generation-witness";
    const SUFFIX: &str = "info/refs?service=git-receive-pack&case=%2F";
    let worker = RealWorker::new("witness");
    let services = worker.services();
    let state = DurableStateStore::new(services.clone());
    assert!(
        state
            .compare_and_swap(
                0,
                &Manifest {
                    generation: 1,
                    ..Manifest::default()
                }
            )
            .unwrap()
    );
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let mut host = NativeHost::open_cloudflare(
        directory.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        false,
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let codec_origin = format!("http://{}", listener.local_addr().unwrap());
    let registration = host.cloudflare_services().unwrap();
    registration.register(&codec_origin).unwrap();
    let before = usage(&services);
    let forwarded = gateway_exchange(
        &worker,
        &listener,
        SUFFIX,
        &[
            (HEADER, "public-forgery"),
            ("x-git-zero-internal-future-proof", "forged"),
            ("x-hosting-repo", "wrong-repo"),
            ("x-hosting-phase", "stats"),
        ],
        |request| host.handle(request),
    );
    assert_eq!(forwarded.target, format!("/repo.git/{SUFFIX}"));
    assert!(
        forwarded
            .headers
            .get(HEADER)
            .is_some_and(|proof| proof.len() < 256 && proof != "public-forgery")
    );
    assert!(
        !forwarded
            .headers
            .contains_key("x-git-zero-internal-future-proof")
    );
    assert_eq!(forwarded.headers["x-hosting-repo"], worker.namespace);
    assert_eq!(forwarded.headers["x-hosting-phase"], "push");
    assert_eq!(host.metrics.generation_witness_hits, 1);
    assert_eq!(host.metrics.generation_witness_fallbacks, 0);
    let first = usage(&services);
    let delta = first.checked_workload_delta(&before).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_gateway_requests,
            delta.worker_service_requests,
            delta.do_requests
        ),
        (1, 1, 0, 1)
    );
    assert_eq!(
        (delta.do_rows_read, delta.do_rows_written),
        (Some(1), Some(0))
    );
    assert_eq!(
        operation(&first, "manifest_get").attempts - operation(&before, "manifest_get").attempts,
        1
    );
    // A genuine discovery cache hit still does its one authority read, wholly
    // inside the existing gateway invocation, with no native manifest callback.
    gateway_exchange(&worker, &listener, SUFFIX, &[], |request| {
        host.handle(request)
    });
    let cached = usage(&services);
    let delta = cached.checked_workload_delta(&first).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_gateway_requests,
            delta.worker_service_requests,
            delta.do_requests
        ),
        (1, 1, 0, 1)
    );
    assert_eq!(
        (
            delta.do_rows_read,
            delta.do_rows_written,
            delta.r2_class_a,
            delta.r2_class_b
        ),
        (Some(1), Some(0), 0, 0)
    );
    assert_eq!(
        operation(&cached, "manifest_get").attempts - operation(&first, "manifest_get").attempts,
        1
    );
    assert_eq!(host.metrics.discovery_cache_hits, 1);

    // Direct requests, forged proofs, bad versions and a changed exact query
    // use the ordinary callback even when the response itself is cached.
    for variant in 0..5 {
        let mut request = forwarded.clone();
        match variant {
            0 => {
                request.headers.remove(HEADER);
            }
            1 => {
                request.headers.insert(HEADER.into(), "malformed".into());
            }
            2 => {
                request.headers.insert(
                    HEADER.into(),
                    format!("2{}", &forwarded.headers[HEADER][1..]),
                );
            }
            3 => {
                let mut proof = forwarded.headers[HEADER].clone();
                proof.pop();
                proof.push(if forwarded.headers[HEADER].ends_with('0') {
                    '1'
                } else {
                    '0'
                });
                request.headers.insert(HEADER.into(), proof);
            }
            _ => {
                request.target.push_str("&changed=1");
            }
        }
        assert_eq!(host.handle(request).status, 200);
    }
    let direct = usage(&services);
    let delta = direct.checked_workload_delta(&cached).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_service_requests,
            delta.worker_gateway_requests,
            delta.do_requests
        ),
        (5, 5, 0, 5)
    );
    assert_eq!(
        operation(&direct, "manifest_get").attempts - operation(&cached, "manifest_get").attempts,
        5
    );
    assert_eq!(host.metrics.generation_witness_fallbacks, 5);

    // A repeated registration safely rotates the private session. The old
    // proof is rejected even for the same generation and exact request.
    registration.register(&codec_origin).unwrap();
    let fallbacks = host.metrics.generation_witness_fallbacks;
    assert_eq!(host.handle(forwarded).status, 200);
    assert_eq!(host.metrics.generation_witness_fallbacks, fallbacks + 1);
    let register_headers = [
        ("x-hosting-token", worker.token.as_str()),
        ("x-hosting-repo", worker.namespace.as_str()),
        ("x-hosting-phase", "stats"),
    ];
    let legacy = serde_json::to_vec(&serde_json::json!({"codec_origin": codec_origin})).unwrap();
    let (status, response) = local_request(
        &worker.origin,
        "POST",
        "/_services/register",
        &register_headers,
        &legacy,
    );
    assert_eq!(status, 200);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&response)
            .unwrap()
            .get("generation_witness")
            .is_none()
    );
    let old_before = usage(&services);
    let old_request = gateway_exchange(
        &worker,
        &listener,
        SUFFIX,
        &[(HEADER, "forged"), ("x-git-zero-internal-any", "forged")],
        |request| host.handle(request),
    );
    assert!(
        !old_request
            .headers
            .keys()
            .any(|name| name.starts_with("x-git-zero-internal-"))
    );
    let old_after = usage(&services);
    let delta = old_after.checked_workload_delta(&old_before).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_gateway_requests,
            delta.worker_service_requests,
            delta.do_requests
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(
        operation(&old_after, "manifest_get").attempts
            - operation(&old_before, "manifest_get").attempts,
        1
    );
    let other = serde_json::to_vec(
        &serde_json::json!({"codec_origin": "http://127.0.0.1:1", "generation_witness": 1}),
    )
    .unwrap();
    assert_eq!(
        local_request(
            &worker.origin,
            "POST",
            "/_services/register",
            &register_headers,
            &other
        )
        .0,
        409
    );

    registration.register(&codec_origin).unwrap();
    // Capture a request queued before a later publication, in the same session.
    let queued = gateway_exchange(&worker, &listener, SUFFIX, &[], |_| {
        Response::error(200, "queued")
    });
    assert!(
        state
            .compare_and_swap(
                1,
                &Manifest {
                    generation: 2,
                    ..Manifest::default()
                }
            )
            .unwrap()
    );
    let before_reload = usage(&services);
    let fallbacks = host.metrics.generation_witness_fallbacks;
    gateway_exchange(&worker, &listener, SUFFIX, &[], |request| {
        host.handle(request)
    });
    assert_eq!(host.metrics.generation_witness_fallbacks, fallbacks + 1);
    let reloaded = usage(&services);
    // Prefetch + the unchanged dispatch reload + recover reload. The recovery
    // GC remains present and counted, including its actual inventory LIST.
    assert_eq!(
        operation(&reloaded, "manifest_get").attempts
            - operation(&before_reload, "manifest_get").attempts,
        3
    );
    let delta = reloaded.checked_workload_delta(&before_reload).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_service_requests,
            delta.worker_gateway_requests,
            delta.do_requests
        ),
        (4, 3, 1, 4)
    );
    assert_eq!(delta.r2_class_a, 1);
    assert_eq!(host.handle(queued).status, 200);
    let after_queue = usage(&services);
    let delta = after_queue.checked_workload_delta(&reloaded).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_service_requests,
            delta.do_requests
        ),
        (1, 1, 1)
    );
    assert_eq!(
        operation(&after_queue, "manifest_get").attempts
            - operation(&reloaded, "manifest_get").attempts,
        1
    );
    assert_eq!(after_queue.accounting_complete, Some(true));
    assert_eq!(after_queue.counter_epoch, before.counter_epoch);
    let stats = host.handle(Request {
        method: "GET".into(),
        target: "/_object-stats".into(),
        headers: BTreeMap::new(),
        body: vec![],
    });
    let stats: serde_json::Value = serde_json::from_slice(&stats.body).unwrap();
    assert_eq!(stats["manifest"]["generation"], 2);
    registration.unregister().unwrap();
}

#[test]
#[ignore = "requires workerd CODEC_ORIGIN and matching GIT_ZERO_CLOUDFLARE_CODEC_ORIGIN loopback origin"]
fn codec_origin_fallback_keeps_one_authoritative_callback_without_prefetch() {
    let worker = RealWorker::new("codec-origin");
    let codec_origin = std::env::var("GIT_ZERO_CLOUDFLARE_CODEC_ORIGIN")
        .expect("set the codec origin configured in workerd CODEC_ORIGIN");
    HttpServices::new(&codec_origin, &worker.namespace, &worker.token).unwrap();
    let listener =
        std::net::TcpListener::bind(codec_origin.strip_prefix("http://").unwrap()).unwrap();
    let services = worker.services();
    let state = DurableStateStore::new(services.clone());
    assert!(
        state
            .compare_and_swap(
                0,
                &Manifest {
                    generation: 1,
                    ..Manifest::default()
                }
            )
            .unwrap()
    );
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let mut host = NativeHost::open_cloudflare(
        directory.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        false,
    )
    .unwrap();
    let before = usage(&services);
    let request = gateway_exchange(
        &worker,
        &listener,
        "info/refs?service=git-receive-pack",
        &[("x-git-zero-internal-generation-witness", "forged")],
        |request| host.handle(request),
    );
    assert!(
        !request
            .headers
            .keys()
            .any(|name| name.starts_with("x-git-zero-internal-"))
    );
    assert_eq!(host.metrics.generation_witness_hits, 0);
    assert_eq!(host.metrics.generation_witness_fallbacks, 1);
    let after = usage(&services);
    let delta = after.checked_workload_delta(&before).unwrap();
    assert_eq!(
        (
            delta.worker_requests,
            delta.worker_gateway_requests,
            delta.worker_service_requests,
            delta.do_requests
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(
        (delta.do_rows_read, delta.do_rows_written),
        (Some(1), Some(0))
    );
    assert_eq!(
        operation(&after, "manifest_get").attempts - operation(&before, "manifest_get").attempts,
        1
    );
}

struct NativeProcess {
    child: std::process::Child,
    origin: String,
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_registered_native(
    worker: &RealWorker,
    root: &std::path::Path,
    port: u16,
) -> NativeProcess {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let child = Command::new(env!("CARGO_BIN_EXE_repository-host"))
        .args([
            "--cloudflare",
            &worker.origin,
            "--repo",
            &worker.namespace,
            "--token",
            &worker.token,
            "--view-cache",
            "warm",
        ])
        .arg(root)
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut native = NativeProcess {
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
        .expect("native codec failed before registration")
        .into();
    native
}

fn native_metrics(native: &NativeProcess) -> serde_json::Value {
    let (status, body) = local_request(&native.origin, "GET", "/metrics", &[], &[]);
    assert_eq!(status, 200);
    serde_json::from_slice(&body).unwrap()
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_registered_gateway_push_fetch_invalidates_cache_and_recovers_after_codec_restart() {
    let worker = RealWorker::new("witness-git");
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let source = directory.0.join("source.git");
    fs::create_dir_all(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    git(&source, &["config", "user.name", "Gateway contract"], &[]);
    git(
        &source,
        &["config", "user.email", "contract@example.invalid"],
        &[],
    );
    let root = directory.0.join("native");
    let mut native = start_registered_native(&worker, &root, 0);
    let port: u16 = native.origin.rsplit(':').next().unwrap().parse().unwrap();
    let url = format!("{}/{}/repo.git", worker.origin, worker.namespace);
    let mut parent: Option<String> = None;
    let mut last_blob = String::new();
    let mut last_tree = String::new();
    for generation in 1..=2 {
        let data = format!("gateway generation {generation}\n").repeat(512);
        let blob = String::from_utf8(git(
            &source,
            &["hash-object", "-w", "--stdin"],
            data.as_bytes(),
        ))
        .unwrap()
        .trim()
        .to_string();
        let tree = String::from_utf8(git(
            &source,
            &["mktree"],
            format!("100644 blob {blob}\tdata.txt\n").as_bytes(),
        ))
        .unwrap()
        .trim()
        .to_string();
        let mut args = vec!["commit-tree", &tree];
        if let Some(parent) = &parent {
            args.extend(["-p", parent]);
        }
        let tip = String::from_utf8(git(&source, &args, b"gateway fixture\n"))
            .unwrap()
            .trim()
            .to_string();
        git(
            &source,
            &["push", "--quiet", &url, &format!("{tip}:refs/heads/main")],
            &[],
        );
        let metrics = native_metrics(&native);
        assert_eq!(
            metrics["cache_entries"], 0,
            "publication must invalidate all cached responses"
        );
        assert_eq!(metrics["generation_witness_fallbacks"], 0);
        assert!(metrics["generation_witness_hits"].as_u64().unwrap() >= generation * 2);
        let clone = format!("clone-{generation}.git");
        git(
            &directory.0,
            &["clone", "--bare", "--quiet", &url, &clone],
            &[],
        );
        let clone = directory.0.join(clone);
        git(&clone, &["fsck", "--strict"], &[]);
        assert_eq!(
            String::from_utf8(git(&clone, &["rev-parse", "refs/heads/main"], &[]))
                .unwrap()
                .trim(),
            tip
        );
        assert_eq!(
            String::from_utf8(git(&clone, &["rev-parse", "refs/heads/main^{tree}"], &[]))
                .unwrap()
                .trim(),
            tree
        );
        assert_eq!(
            git(&clone, &["cat-file", "blob", &blob], &[]),
            data.as_bytes()
        );
        parent = Some(tip);
        last_blob = blob;
        last_tree = tree;
    }
    let before = usage(&worker.services());
    let epoch = before.counter_epoch.clone();
    drop(native);
    fs::remove_dir_all(root.join("view")).unwrap();
    native = start_registered_native(&worker, &root, port);
    git(
        &directory.0,
        &["clone", "--bare", "--quiet", &url, "clone-restarted.git"],
        &[],
    );
    let clone = directory.0.join("clone-restarted.git");
    git(&clone, &["fsck", "--strict"], &[]);
    assert_eq!(
        String::from_utf8(git(&clone, &["rev-parse", "refs/heads/main^{tree}"], &[]))
            .unwrap()
            .trim(),
        last_tree
    );
    assert_eq!(
        git(&clone, &["cat-file", "blob", &last_blob], &[]),
        "gateway generation 2\n".repeat(512).as_bytes()
    );
    let metrics = native_metrics(&native);
    assert_eq!(metrics["generation_witness_fallbacks"], 0);
    assert!(metrics["generation_witness_hits"].as_u64().unwrap() >= 2);
    let after = usage(&worker.services());
    assert_eq!(after.accounting_complete, Some(true));
    assert_eq!(after.counter_epoch, epoch);
    after.checked_workload_delta(&before).unwrap();
    worker.services().unregister().unwrap();
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_snapshot_bulk_reads_cross_packs_and_account_partial_missing_reads() {
    let worker = RealWorker::new("bulk");
    let services = worker.services();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    let legacy = repository
        .store_pack("legacy".into(), b"legacy-pack", b"legacy-index", 0)
        .unwrap();
    let bundled = repository
        .store_bundled_pack("bundle".into(), b"bundled-pack", b"bundled-index", 0)
        .unwrap();
    let manifest = Manifest {
        packs: vec![legacy.clone(), bundled.clone()],
        ..Manifest::default()
    };
    let before = usage(&services);
    assert_eq!(
        repository.read_snapshot_files(&manifest).unwrap(),
        vec![
            (b"legacy-pack".to_vec(), b"legacy-index".to_vec()),
            (b"bundled-pack".to_vec(), b"bundled-index".to_vec()),
        ]
    );
    let after = usage(&services);
    let delta = after
        .workload_counters()
        .unwrap()
        .checked_sub(&before.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            delta.worker_service_requests,
            delta.do_requests,
            delta.r2_class_b
        ),
        (1, 1, 3)
    );
    assert_eq!(
        delta.r2_read_bytes,
        manifest
            .packs
            .iter()
            .map(|pack| pack.pack_bytes + pack.index_bytes)
            .sum::<u64>()
    );
    assert_eq!(
        (delta.r2_class_a, delta.r2_deletes, delta.r2_written_bytes),
        (0, 0, 0)
    );
    assert_eq!(after.accounting_complete, Some(true));

    let duplicate = [legacy.pack[0].clone(), legacy.pack[0].clone()];
    assert_eq!(
        repository.blobs.get_many(&duplicate).unwrap(),
        vec![b"legacy-pack".to_vec(); 2]
    );
    let repeated = usage(&services);
    let delta = repeated
        .workload_counters()
        .unwrap()
        .checked_sub(&after.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            delta.worker_service_requests,
            delta.do_requests,
            delta.r2_class_b
        ),
        (1, 1, 2)
    );
    assert_eq!(delta.r2_read_bytes, 2 * legacy.pack_bytes);

    let missing = Chunk {
        key: format!("chunks/{}", hash(b"absent")),
        len: 6,
    };
    assert_eq!(
        repository
            .blobs
            .get_many(&[legacy.pack[0].clone(), missing, legacy.index[0].clone()])
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let failed = usage(&services);
    let delta = failed
        .workload_counters()
        .unwrap()
        .checked_sub(&repeated.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            delta.worker_service_requests,
            delta.do_requests,
            delta.r2_class_b
        ),
        (1, 1, 2)
    );
    assert_eq!(delta.r2_read_bytes, legacy.pack_bytes);
    assert_eq!(
        operation(&failed, "r2_get").attempts - operation(&repeated, "r2_get").attempts,
        2
    );
    assert_eq!(failed.accounting_complete, Some(true));

    let mut wrong_length = legacy.pack[0].clone();
    wrong_length.len += 1;
    assert!(repository.blobs.get_many(&[wrong_length]).is_err());
    let mismatch = usage(&services);
    let delta = mismatch
        .workload_counters()
        .unwrap()
        .checked_sub(&failed.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            delta.worker_service_requests,
            delta.do_requests,
            delta.r2_class_b
        ),
        (1, 1, 1)
    );
    assert_eq!(delta.r2_read_bytes, legacy.pack_bytes);
    assert_eq!(mismatch.accounting_complete, Some(true));
    assert_eq!(operation(&mismatch, "r2_put"), operation(&before, "r2_put"));
    assert_eq!(
        operation(&mismatch, "r2_delete"),
        operation(&before, "r2_delete")
    );
    let empty_before = mismatch.workload_counters().unwrap();
    assert!(repository.blobs.get_many(&[]).unwrap().is_empty());
    assert_eq!(usage(&services).workload_counters().unwrap(), empty_before);
}

fn bulk_post_status(worker: &RealWorker, body: &[u8]) -> u16 {
    bulk_post_status_framed(worker, body, false)
}

fn bulk_post_status_framed(worker: &RealWorker, body: &[u8], chunked: bool) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let authority = worker.origin.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream, "POST /_services/blobs/read HTTP/1.1\r\nHost: {authority}\r\nx-hosting-token: {}\r\nx-hosting-repo: {}\r\nx-hosting-phase: fetch\r\n", worker.token, worker.namespace).unwrap();
    if chunked {
        write!(
            stream,
            "Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").unwrap();
    } else {
        write!(
            stream,
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_bulk_read_fits_two_full_chunks_and_charges_each_repeated_get() {
    let worker = RealWorker::new("bulk-full");
    let services = worker.services();
    let blobs = R2BlobStore::new(services.clone());
    let data = vec![0x61; 8 * 1024 * 1024];
    let chunk = Chunk {
        key: format!("chunks/{}", hash(&data)),
        len: data.len() as u64,
    };
    blobs.put(&chunk.key, &data).unwrap();
    let before = usage(&services);
    assert_eq!(
        blobs.get_many(&[chunk.clone(), chunk]).unwrap(),
        vec![data.clone(), data]
    );
    let after = usage(&services);
    let delta = after
        .workload_counters()
        .unwrap()
        .checked_sub(&before.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            delta.worker_service_requests,
            delta.do_requests,
            delta.r2_class_b,
            delta.r2_read_bytes
        ),
        (1, 1, 2, 16 * 1024 * 1024)
    );
    assert_eq!(
        (delta.r2_class_a, delta.r2_deletes, delta.r2_written_bytes),
        (0, 0, 0)
    );
    let reads_before = operation(&before, "r2_get");
    let reads_after = operation(&after, "r2_get");
    assert_eq!(reads_after.attempts - reads_before.attempts, 2);
    assert_eq!(reads_after.successes - reads_before.successes, 2);
    assert_eq!(
        reads_after.read_bytes - reads_before.read_bytes,
        16 * 1024 * 1024
    );
    assert_eq!(after.accounting_complete, Some(true));
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_bulk_read_validates_all_bounds_before_binding_io() {
    let worker = RealWorker::new("bulk-bounds");
    let services = worker.services();
    let key = format!("chunks/{}", hash(b"bound"));
    let before = usage(&services);
    let chunk = serde_json::json!({"key": key, "len": 1});
    let invalid = vec![
        serde_json::json!({"chunks": []}),
        serde_json::json!({"chunks": vec![chunk.clone(); 257]}),
        serde_json::json!({"chunks": [chunk.clone(), {"key": "chunks/invalid", "len": 1}]}),
        serde_json::json!({"chunks": [{"key": format!("{key}\n"), "len": 1}]}),
        serde_json::json!({"chunks": [chunk.clone(), {"key": key, "len": 2}]}),
        serde_json::json!({"chunks": [{"key": key, "len": 0}]}),
        serde_json::json!({"chunks": [{"key": key, "len": 8 * 1024 * 1024 + 1}]}),
        serde_json::json!({"chunks": [{"key": key, "len": u64::MAX}]}),
    ];
    for body in &invalid {
        assert_eq!(
            bulk_post_status(&worker, &serde_json::to_vec(body).unwrap()),
            400
        );
    }
    let too_large = serde_json::json!({"chunks": [
        {"key": key, "len": 8 * 1024 * 1024},
        {"key": key, "len": 8 * 1024 * 1024},
        {"key": format!("chunks/{}", hash(b"extra")), "len": 1}
    ]});
    assert_eq!(
        bulk_post_status(&worker, &serde_json::to_vec(&too_large).unwrap()),
        413
    );
    assert_eq!(bulk_post_status(&worker, &vec![b' '; 1_500_001]), 413);
    assert_eq!(
        bulk_post_status_framed(&worker, &vec![b' '; 1_500_001], true),
        413
    );
    let after = usage(&services);
    let delta = after
        .workload_counters()
        .unwrap()
        .checked_sub(&before.workload_counters().unwrap())
        .unwrap();
    assert_eq!(delta.worker_service_requests, invalid.len() as u64 + 3);
    // Both oversized JSON bodies are rejected by the Worker before DO dispatch;
    // Invalid chunks within the cap and an oversized payload budget reach it.
    assert_eq!(delta.do_requests, invalid.len() as u64 + 1);
    assert_eq!(
        (
            delta.r2_class_b,
            delta.r2_read_bytes,
            delta.r2_class_a,
            delta.r2_deletes
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(after.accounting_complete, Some(true));
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_native_bundle_uses_one_put_and_survives_restart_and_fetch() {
    let worker = RealWorker::new("bundle");
    let services = worker.services();
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let source = directory.0.join("source.git");
    fs::create_dir_all(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    let data = b"a small native pack and its index share one immutable R2 object\n";
    let object_id = String::from_utf8(git(&source, &["hash-object", "-w", "--stdin"], data))
        .unwrap()
        .trim()
        .to_string();
    let packed = git(
        &source,
        &["pack-objects", "--stdout"],
        format!("{object_id}\n").as_bytes(),
    );
    let mut body = packet(&format!(
        "0000000000000000000000000000000000000000 {object_id} refs/tags/bundle-fixture\0report-status atomic\n"
    ));
    body.extend_from_slice(b"0000");
    body.extend_from_slice(&packed);
    let root = directory.0.join("host");
    let mut host = NativeHost::open_cloudflare(
        root.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    let before: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    let response = host.handle(rpc("receive", body));
    assert_eq!(response.status, 200);
    assert!(String::from_utf8_lossy(&response.body).contains("ok refs/tags/bundle-fixture\n"));
    assert_eq!(host.metrics.blob_puts, 1);
    let state = DurableStateStore::new(services.clone());
    let published = state.load().unwrap();
    assert_eq!(published.packs.len(), 1);
    let stored = &published.packs[0];
    assert_eq!(stored.bundle.len(), 1);
    assert!(stored.pack.is_empty() && stored.index.is_empty());
    assert_eq!(
        host.metrics.blob_put_bytes,
        stored.pack_bytes + stored.index_bytes
    );
    let pushed: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(
        operation(&pushed, "r2_put").successes - operation(&before, "r2_put").successes,
        1
    );
    assert_eq!(pushed.r2_live_objects, 1);
    assert_eq!(pushed.r2_live_bytes, stored.pack_bytes + stored.index_bytes);
    drop(host);

    fs::remove_dir_all(root.join("view")).unwrap();
    let mut recovered = NativeHost::open_cloudflare(
        root.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    let reopened: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(
        operation(&reopened, "r2_get").successes - operation(&pushed, "r2_get").successes,
        1
    );
    git(&root.join("view/repo.git"), &["fsck", "--strict"], &[]);
    assert_eq!(
        git(
            &root.join("view/repo.git"),
            &["cat-file", "blob", &object_id],
            &[]
        ),
        data
    );

    let mut fetch = packet(&format!("want {object_id}\n"));
    fetch.extend_from_slice(b"0000");
    fetch.extend(packet("done\n"));
    let fetched = recovered.handle(rpc("upload", fetch));
    assert_eq!(fetched.status, 200);
    let offset = fetched
        .body
        .windows(4)
        .position(|bytes| bytes == b"PACK")
        .unwrap();
    let destination = directory.0.join("fetched.git");
    fs::create_dir(&destination).unwrap();
    git(
        &destination,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    git(
        &destination,
        &["index-pack", "--stdin", "--strict"],
        &fetched.body[offset..],
    );
    git(
        &destination,
        &["update-ref", "refs/tags/bundle-fixture", &object_id],
        &[],
    );
    git(&destination, &["fsck", "--strict"], &[]);
    assert_eq!(
        git(&destination, &["cat-file", "blob", &object_id], &[]),
        data
    );
    assert_eq!(state.load().unwrap(), published);
    assert_eq!(recovered.metrics.blob_puts, 0);
    let after: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(
        operation(&after, "r2_get").successes - operation(&reopened, "r2_get").successes,
        1
    );
    assert_eq!(operation(&after, "r2_put"), operation(&pushed, "r2_put"));
    assert_eq!(
        operation(&after, "r2_delete"),
        operation(&pushed, "r2_delete")
    );
    assert_eq!(
        operation(&after, "manifest_cas"),
        operation(&pushed, "manifest_cas")
    );
    assert_eq!(after.r2_live_objects, 1);
    assert_eq!(after.r2_live_bytes, pushed.r2_live_bytes);
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_r2_immutability_namespaces_and_operation_accounting() {
    let worker = RealWorker::new("r2");
    let services = worker.services();
    let blobs = R2BlobStore::new(services.clone());
    let data = b"compressed immutable contract fixture";
    let key = format!("chunks/{}", hash(data));
    blobs.put(&key, data).unwrap();
    blobs.put(&key, data).unwrap();
    assert_eq!(blobs.get(&key).unwrap(), data);
    assert_eq!(
        blobs.put(&key, b"different bytes").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        blobs.put("../escape", data).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    // A fresh HTTP adapter has no local blob cache to satisfy this read.
    let reopened = R2BlobStore::new(worker.services());
    assert_eq!(reopened.get(&key).unwrap(), data);
    let other = RealWorker::new("isolated");
    assert_eq!(
        R2BlobStore::new(other.services())
            .get(&key)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );

    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 1);
    assert_eq!(usage.r2_live_bytes, data.len() as u64);
    assert_eq!(usage.operations["r2_put"].successes, 2);
    assert!(usage.operations["r2_get"].successes >= 2);
    assert!(usage.operations["r2_list"].successes >= 1);
    assert_eq!(
        usage.workload_counters().unwrap().worker_observer_requests,
        0
    );
    assert!(usage.calls.worker_cpu_ms.is_none());
    assert!(usage.calls.do_gb_seconds.is_none());
    assert!(usage.counter_epoch.is_some());

    blobs.delete(&key).unwrap();
    blobs.delete(&key).unwrap();
    assert_eq!(blobs.get(&key).unwrap_err().kind(), io::ErrorKind::NotFound);
    let final_usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(final_usage.r2_live_objects, 0);
    assert_eq!(final_usage.r2_live_bytes, 0);
    assert!(final_usage.r2_peak_bytes >= data.len() as u64);
    assert_eq!(final_usage.calls.r2_deletes, 2);
}

#[test]
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_durable_object_cas_and_failed_publication_recovery() {
    let worker = RealWorker::new("cas");
    let services = worker.services();
    let state = Arc::new(DurableStateStore::new(services.clone()));
    assert_eq!(state.load().unwrap(), Manifest::default());
    assert_eq!(
        services.usage().unwrap()["manifest_generation"].as_u64(),
        Some(0)
    );
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let state = state.clone();
            std::thread::spawn(move || {
                state
                    .compare_and_swap(
                        0,
                        &Manifest {
                            generation: 1,
                            ..Manifest::default()
                        },
                    )
                    .unwrap()
            })
        })
        .collect();
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);

    let reopened = DurableStateStore::new(worker.services());
    let published = reopened.load().unwrap();
    assert_eq!(published.generation, 1);
    assert_eq!(
        services.usage().unwrap()["manifest_generation"].as_u64(),
        Some(1)
    );
    assert!(!reopened.compare_and_swap(0, &published).unwrap());
    assert_eq!(
        reopened.compare_and_swap(1, &published).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    // Simulate a writer that persisted chunks, then lost the ref CAS. A fresh
    // native codec must load the existing generation and collect those orphans.
    let data = b"unpublished candidate pack";
    let key = format!("chunks/{}", hash(data));
    let blobs = R2BlobStore::new(services.clone());
    blobs.put(&key, data).unwrap();
    assert!(!state.compare_and_swap(0, &published).unwrap());
    assert_eq!(
        services.usage().unwrap()["manifest_generation"].as_u64(),
        Some(1)
    );
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let host = NativeHost::open_cloudflare(
        directory.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    assert_eq!(reopened.load().unwrap(), published);
    assert_eq!(blobs.get(&key).unwrap_err().kind(), io::ErrorKind::NotFound);
    drop(host);
    fs::remove_dir_all(directory.0.join("view")).unwrap();
    NativeHost::open_cloudflare(
        directory.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    assert_eq!(reopened.load().unwrap(), published);
}

#[test]
#[ignore = "requires real local Wrangler/workerd and its 10-second idle DO eviction"]
fn real_do_idle_eviction_keeps_complete_cumulative_accounting() {
    let worker = RealWorker::new("idle");
    let services = worker.services();
    let blobs = R2BlobStore::new(services.clone());
    let data = b"actual R2 operations survive an idle Durable Object eviction";
    let key = format!("chunks/{}", hash(data));
    blobs.put(&key, data).unwrap();
    assert_eq!(blobs.get(&key).unwrap(), data);
    let before: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(before.accounting_complete, Some(true));
    assert!(before.do_instance_epoch.is_some());
    assert!(before.counter_epoch.is_some());

    // No health, stats, storage or gateway calls during either idle interval.
    // Prove eviction using the instance identity, rather than assuming sleep
    // alone caused it. One retry accommodates delayed local timer delivery.
    std::thread::sleep(Duration::from_secs(13));
    let mut after: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    if after.do_instance_epoch == before.do_instance_epoch {
        std::thread::sleep(Duration::from_secs(13));
        after = serde_json::from_value(services.usage().unwrap()).unwrap();
    }
    assert_ne!(
        after.do_instance_epoch, before.do_instance_epoch,
        "workerd did not evict and recreate the Durable Object within 26 idle seconds"
    );
    assert_eq!(after.counter_epoch, before.counter_epoch);
    assert_eq!(after.accounting_complete, Some(true));
    assert!(after.calls.r2_class_a >= before.calls.r2_class_a);
    assert!(after.calls.r2_class_b >= before.calls.r2_class_b);
    assert!(after.calls.do_requests >= before.calls.do_requests);
    assert!(after.calls.do_rows_read.unwrap() >= before.calls.do_rows_read.unwrap());
    assert!(after.calls.do_rows_written.unwrap() >= before.calls.do_rows_written.unwrap());
    after.checked_workload_delta(&before).unwrap();

    assert_eq!(blobs.get(&key).unwrap(), data);
    let final_usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    let actual = final_usage.checked_workload_delta(&after).unwrap();
    assert_eq!(actual.r2_class_b, 1);
    assert_eq!(actual.worker_requests, 1);
    assert_eq!(actual.do_requests, 1);
    assert_eq!(final_usage.r2_live_objects, 1);
    assert_eq!(final_usage.r2_live_bytes, data.len() as u64);
    blobs.delete(&key).unwrap();
}

#[test]
#[ignore = "two-phase real Wrangler restart test; run seed, restart same persist-dir, then verify"]
fn seed_before_workerd_restart() {
    let worker = restart_fixture();
    let services = worker.services();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    let previous = repository.state.load().unwrap();
    assert_eq!(
        previous,
        Manifest::default(),
        "restart proof requires a fresh namespace"
    );
    let source = Directory(std::env::temp_dir().join(format!("{}-source", worker.namespace)));
    fs::create_dir(&source.0).unwrap();
    git(
        &source.0,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    let object_id = String::from_utf8(git(
        &source.0,
        &["hash-object", "-w", "--stdin"],
        RESTART_DATA,
    ))
    .unwrap()
    .trim()
    .to_string();
    let data = git(
        &source.0,
        &["pack-objects", "--stdout"],
        format!("{object_id}\n").as_bytes(),
    );
    let indexed = String::from_utf8(git(
        &source.0,
        &["index-pack", "--stdin", "--strict"],
        &data,
    ))
    .unwrap();
    let id = indexed.split_whitespace().last().unwrap();
    let index = fs::read(source.0.join(format!("objects/pack/pack-{id}.idx"))).unwrap();
    let stored = repository
        .store_bundled_pack(id.into(), &data, &index, 0)
        .unwrap();
    let next = Manifest {
        generation: 1,
        refs: BTreeMap::from([("refs/tags/restart-fixture".into(), object_id.clone())]),
        packs: vec![stored],
        ..Manifest::default()
    };
    assert!(repository.publish(&previous, next.clone()).unwrap());
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 1);
    let marker = serde_json::json!({
        "manifest":next, "object_id":object_id, "counter_epoch":usage.counter_epoch,
    });
    fs::write(
        restart_marker(&worker),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "two-phase real Wrangler restart test; seed and restart same persist-dir first"]
fn verify_after_workerd_restart() {
    let worker = restart_fixture();
    let marker: serde_json::Value = serde_json::from_slice(
        &fs::read(restart_marker(&worker))
            .expect("run seed_before_workerd_restart before restarting Wrangler"),
    )
    .unwrap();
    let expected: Manifest = serde_json::from_value(marker["manifest"].clone()).unwrap();
    let object_id = marker["object_id"].as_str().unwrap();
    let services = worker.services();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    assert_eq!(repository.state.load().unwrap(), expected);
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_ne!(
        serde_json::to_value(&usage.counter_epoch).unwrap(),
        marker["counter_epoch"],
        "Worker/DO runtime did not restart between the two test phases"
    );
    for pack in &expected.packs {
        let (data, index) = repository.read_pack_files(pack).unwrap();
        assert_eq!(data.len() as u64, pack.pack_bytes);
        assert_eq!(index.len() as u64, pack.index_bytes);
    }
    let view = Directory(std::env::temp_dir().join(format!("{}-recovered", worker.namespace)));
    let host = NativeHost::open_cloudflare(
        view.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    assert_eq!(
        git(
            &view.0.join("view/repo.git"),
            &["cat-file", "blob", object_id],
            &[]
        ),
        RESTART_DATA
    );
    drop(host);
    let mut empty = Manifest::default();
    empty.retire_replaced(&expected);
    assert!(repository.publish(&expected, empty).unwrap());
    let retired = repository.state.load().unwrap();
    assert!(retired.collect_garbage(&repository.blobs).is_empty());
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 0);
    fs::remove_file(restart_marker(&worker)).unwrap();
}
