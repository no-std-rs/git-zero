//! Rust owns actual Wrangler/R2/SQLite contracts on 8834. These are ignored so
//! ordinary tests cannot start services or collide with another runtime owner.
use git_zero_host::{accounting::ServiceUsage, hash};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ORIGIN: &str = "http://127.0.0.1:8834";
const PROOF: &str = "x-git-zero-retirement-proof";
const SQLITE_LIMIT: u64 = 24_821;
static REQUEST_ID: AtomicU64 = AtomicU64::new(0);

struct Runtime {
    child: Child,
}
impl Runtime {
    fn start(root: &Path, phase: usize) -> Self {
        let log_path = root.join(format!("wrangler-{phase}.log"));
        let log = fs::File::create(&log_path).unwrap();
        let cli = "/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js";
        assert!(Path::new(cli).is_file(), "reuse the installed Wrangler");
        let config =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/test/deferred-catalog.toml");
        let child = Command::new("setsid")
            .args(["node", cli, "dev", "--config"])
            .arg(config)
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                "8834",
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
        let mut runtime = Self { child };
        for _ in 0..300 {
            assert!(
                runtime.child.try_wait().unwrap().is_none(),
                "Wrangler exited; inspect {}",
                log_path.display()
            );
            if fs::read_to_string(&log_path)
                .unwrap()
                .contains("Ready on http://127.0.0.1:8834")
            {
                return runtime;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Wrangler not ready: {}", log_path.display());
    }
    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            assert!(
                Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", self.child.id())])
                    .status()
                    .unwrap()
                    .success()
            );
            self.child.wait().unwrap();
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

fn artifacts(label: &str) -> PathBuf {
    let reservation = std::net::TcpListener::bind("127.0.0.1:8834")
        .expect("8834 must be free; never stop another owner's runtime");
    let root = std::env::var("GIT_ZERO_DEFERRED_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "git-zero-deferred-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
        });
    fs::create_dir_all(&root).unwrap();
    assert!(!root.join("persist").exists(), "requires fresh persistence");
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fingerprints: BTreeMap<_, _> = [
        "cloudflare/src/index.js",
        "cloudflare/src/catalog-pages.js",
        "cloudflare/src/retirement-proof.js",
        "cloudflare/test/deferred-catalog-worker.js",
        "cloudflare/test/deferred-catalog.toml",
        "src/cloudflare.rs",
        "tests/deferred_catalog.rs",
    ]
    .into_iter()
    .map(|file| (file, hash(&fs::read(package.join(file)).unwrap())))
    .collect();
    fs::write(
        root.join("source.json"),
        serde_json::to_vec_pretty(&fingerprints).unwrap(),
    )
    .unwrap();
    drop(reservation);
    root
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}
impl Reply {
    fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        serde_json::from_slice(&self.body).unwrap()
    }
    fn proof(&self) -> Option<&str> {
        self.headers.get(PROOF).map(String::as_str)
    }
}

#[derive(Clone)]
struct Worker {
    repo: String,
}
impl Worker {
    fn new(label: &str) -> Self {
        Self {
            repo: format!(
                "deferred-{label}-{}-{}",
                std::process::id(),
                REQUEST_ID.fetch_add(1, Ordering::Relaxed)
            ),
        }
    }
    fn request(&self, method: &str, route: &str, body: &[u8], proof: Option<&str>) -> Reply {
        self.request_headers(method, route, body, proof, &[])
    }
    fn request_headers(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        proof: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Reply {
        let header_path = std::env::temp_dir().join(format!(
            "git-zero-deferred-headers-{}-{}",
            std::process::id(),
            REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new("curl");
        command
            .args([
                "--silent",
                "--show-error",
                "--max-time",
                if route.ends_with("/evict") {
                    "10"
                } else {
                    "120"
                },
                "--request",
                method,
                "--write-out",
                "\n%{http_code}",
                "--dump-header",
            ])
            .arg(&header_path);
        for (name, value) in [
            ("x-hosting-token", "git-zero-local-services"),
            ("x-hosting-repo", self.repo.as_str()),
            ("x-hosting-phase", "push"),
        ] {
            command.arg("--header").arg(format!("{name}: {value}"));
        }
        if let Some(proof) = proof {
            command.arg("--header").arg(format!("{PROOF}: {proof}"));
        }
        for (name, value) in extra {
            command.arg("--header").arg(format!("{name}: {value}"));
        }
        if !body.is_empty() {
            command.args(["--data-binary", "@-"]);
        }
        let mut child = command
            .arg(format!("{ORIGIN}{route}"))
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
        assert!(output.stdout.len() >= 4);
        let split = output.stdout.len() - 4;
        let status = std::str::from_utf8(&output.stdout[split + 1..])
            .unwrap()
            .parse()
            .unwrap();
        output.stdout.truncate(split);
        let headers = fs::read_to_string(&header_path)
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        fs::remove_file(header_path).unwrap();
        Reply {
            status,
            body: output.stdout,
            headers,
        }
    }
    fn json(&self, method: &str, route: &str, body: &Value, proof: Option<&str>) -> Reply {
        self.request(
            method,
            route,
            &if method == "GET" {
                Vec::new()
            } else {
                serde_json::to_vec(body).unwrap()
            },
            proof,
        )
    }
    fn register(&self, deferred: bool) -> Value {
        self.json(
            "POST",
            "/_services/register",
            &json!({
                "codec_origin":"http://127.0.0.1:1", "manifest_retire":1,
                "manifest_upload_retire":1, "catalog_peak":1, "catalog_pages":1,
                "catalog_deferred":usize::from(deferred)
            }),
            None,
        )
        .json()
    }
    fn state(&self) -> Value {
        self.json("GET", "/_services/test/deferred", &Value::Null, None)
            .json()
    }
    fn usage_json(&self) -> Value {
        self.json("GET", "/_services/metrics", &Value::Null, None)
            .json()
    }
    fn usage(&self) -> ServiceUsage {
        serde_json::from_value(self.usage_json()).unwrap()
    }
    fn put(&self, bytes: &[u8], proof: Option<&str>) -> String {
        let key = format!("chunks/{}", hash(bytes));
        let reply = self.request("PUT", &format!("/_services/blob/{key}"), bytes, proof);
        assert_eq!(
            reply.status,
            204,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        key
    }
    fn get(&self, key: &str) -> Reply {
        self.request("GET", &format!("/_services/blob/{key}"), &[], None)
    }
    fn fault(&self, mode: &str) {
        self.json(
            "POST",
            "/_services/test/deferred-fault",
            &json!({"mode":mode}),
            None,
        )
        .json();
    }
    fn page_fault(&self, mode: &str) {
        self.json(
            "POST",
            "/_services/test/page-fault",
            &json!({"mode":mode}),
            None,
        )
        .json();
    }
    fn retire(&self, expected: u64, retired: &[String], live: &[(String, usize)]) -> Reply {
        self.json(
            "POST",
            "/_services/manifest/retire",
            &json!({
                "version":1, "expected":expected,
                "next":manifest(expected + 1, live, retired)
            }),
            None,
        )
    }
    fn upload_retire(
        &self,
        expected: u64,
        retired: &[String],
        live: &[(String, usize)],
        data: &[u8],
    ) -> Reply {
        let chunk = (format!("chunks/{}", hash(data)), data.len());
        let mut next_live = live.to_vec();
        next_live.push(chunk.clone());
        let metadata = serde_json::to_vec(&json!({"version":1,"expected":expected,
            "next":manifest(expected + 1, &next_live, retired),
            "chunk":{"key":chunk.0,"len":chunk.1}}))
        .unwrap();
        let mut frame = b"GZUP".to_vec();
        frame.extend(1u32.to_le_bytes());
        frame.extend((metadata.len() as u32).to_le_bytes());
        frame.extend(metadata);
        frame.extend(data);
        self.request("POST", "/_services/manifest/upload-retire", &frame, None)
    }
    fn publish(&self, expected: u64, live: &[(String, usize)], proof: Option<&str>) -> Reply {
        self.json(
            "POST",
            "/_services/manifest",
            &json!({
                "expected":expected, "next":manifest(expected + 1, live, &[])
            }),
            proof,
        )
    }
    fn gc(&self, generation: u64, live: &[String]) -> Reply {
        self.json(
            "POST",
            "/_services/gc",
            &json!({
                "expected_generation":generation, "live_keys":live
            }),
            None,
        )
    }
    fn seed(&self, rows: Vec<Value>) {
        self.json(
            "POST",
            "/_services/test/seed",
            &json!({"reset":true,"rows":rows}),
            None,
        )
        .json();
    }
}

fn manifest(generation: u64, live: &[(String, usize)], garbage: &[String]) -> Value {
    let packs: Vec<_> = live
        .iter()
        .enumerate()
        .map(|(i, (key, size))| {
            json!({
                "id":format!("live-{i}"),"pack":[],"index":[],"bundle":[{"key":key,"len":size}],
                "pack_bytes":size,"index_bytes":0,"level":0
            })
        })
        .collect();
    json!({"generation":generation,"refs":{},"packs":packs,"garbage":garbage})
}

struct Fixture {
    worker: Worker,
    retired: Vec<(String, Vec<u8>)>,
    survivor: (String, usize),
}
impl Fixture {
    fn new(label: &str, deferred: bool) -> Self {
        let worker = Worker::new(label);
        let ack = worker.register(deferred);
        if deferred {
            assert_eq!(
                ack["catalog_deferred"],
                json!({"version":1,"proof_bytes":178})
            );
        } else {
            assert!(ack.get("catalog_deferred").is_none());
        }
        let retired = (1..=3)
            .map(|value| {
                let data = vec![value; 32];
                (worker.put(&data, None), data)
            })
            .collect();
        let survivor = (worker.put(&[4; 32], None), 32);
        let state = worker.state();
        assert_eq!(state["catalog"].as_array().unwrap().len(), 1);
        assert_eq!(members(&state).len(), 4);
        Self {
            worker,
            retired,
            survivor,
        }
    }
    fn keys(&self) -> Vec<String> {
        self.retired.iter().map(|(key, _)| key.clone()).collect()
    }
    fn retire(&self) -> Reply {
        self.worker
            .retire(0, &self.keys(), std::slice::from_ref(&self.survivor))
    }
}

fn members(state: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    for row in state["catalog"].as_array().unwrap() {
        if let Some(decoded) = row["decoded"].as_array() {
            keys.extend(
                decoded
                    .iter()
                    .map(|entry| format!("chunks/{}", entry["digest"].as_str().unwrap())),
            );
        } else {
            keys.push(row["key"].as_str().unwrap().to_string());
        }
    }
    keys.sort();
    keys
}
fn catalog_writes(state: &Value) -> u64 {
    state["sql"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            let sql = row["statement"].as_str().unwrap();
            sql.starts_with("INSERT INTO blob_catalog")
                || sql.starts_with("DELETE FROM blob_catalog")
        })
        .map(|row| row["rowsWritten"].as_u64().unwrap())
        .sum()
}
fn checked_proof(reply: &Reply) -> String {
    let proof = reply
        .proof()
        .expect("successful known retirement must mint proof");
    assert_eq!(proof.len(), 178);
    assert!(proof.starts_with("01"));
    assert!(
        proof
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    proof.to_string()
}
fn retired_ok(reply: &Reply, keys: &[String]) {
    let body = reply.json();
    assert_eq!(body["version"], 1);
    assert_eq!(body["published"], true);
    assert_eq!(
        body.as_object().unwrap().len(),
        3,
        "strict response shape stays stable"
    );
    let outcomes = body["retirement"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), keys.len());
    for (outcome, key) in outcomes.iter().zip(keys) {
        assert_eq!(outcome["key"], *key);
        assert_eq!(outcome["deleted"], true);
    }
}
fn assert_live(worker: &Worker, live: &[(String, usize)], peak: u64) -> Value {
    let usage = assert_inventory(worker, live, peak);
    assert_eq!(usage["accounting_complete"], true);
    usage
}
fn assert_inventory(worker: &Worker, live: &[(String, usize)], peak: u64) -> Value {
    let usage = worker.usage_json();
    assert_eq!(usage["r2_live_objects"], live.len());
    assert_eq!(
        usage["r2_live_bytes"],
        live.iter().map(|(_, size)| size).sum::<usize>()
    );
    assert_eq!(usage["r2_peak_bytes"], peak);
    for (key, size) in live {
        let response = worker.get(key);
        assert_eq!(response.status, 200);
        assert_eq!(response.body.len(), *size);
        assert_eq!(format!("chunks/{}", hash(&response.body)), *key);
    }
    usage
}
fn report(root: &Path, name: &str, reports: &[Value]) {
    fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec_pretty(reports).unwrap(),
    )
    .unwrap();
    println!("deferred catalog {name} artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8834; early actual R12/new-mode retirement and next-mutation cost gate"]
fn actual_deferred_catalog_normal_gate() {
    let root = artifacts("normal");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let mut deltas = Vec::new();
    for deferred in [false, true] {
        let fixture = Fixture::new(if deferred { "new" } else { "r12" }, deferred);
        let worker = &fixture.worker;
        worker.fault("");
        let before = worker.usage();
        let response = fixture.retire();
        retired_ok(&response, &fixture.keys());
        let proof = if deferred {
            Some(checked_proof(&response))
        } else {
            assert!(response.proof().is_none());
            None
        };
        let after = worker.usage();
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.worker_service_requests, 1);
        assert_eq!(delta.do_requests, 1);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.r2_class_a, 0);
        assert_eq!(delta.r2_class_b, 0);
        assert!(delta.do_rows_read.is_some());
        assert_eq!(delta.do_rows_written, Some(if deferred { 1 } else { 2 }));
        let retired_state = worker.state();
        assert_eq!(catalog_writes(&retired_state), u64::from(!deferred));
        assert_eq!(retired_state["high_water"].as_array().unwrap().len(), 0);
        assert_live(worker, std::slice::from_ref(&fixture.survivor), 128);
        for key in fixture.keys() {
            assert_eq!(worker.get(&key).status, 404);
        }

        worker.fault("");
        let before_put = worker.usage();
        let extra = (worker.put(&[5; 21], proof.as_deref()), 21);
        let after_put = worker.usage();
        let put_delta = after_put.checked_workload_delta(&before_put).unwrap();
        assert_eq!(put_delta.worker_service_requests, 1);
        assert_eq!(put_delta.do_requests, 1);
        assert_eq!(put_delta.r2_class_a, 1);
        assert_eq!(put_delta.r2_class_b, 0);
        assert_eq!(put_delta.r2_deletes, 0);
        assert_eq!(put_delta.do_rows_written, Some(1));
        let folded = worker.state();
        assert_eq!(
            catalog_writes(&folded),
            1,
            "fold removals into existing PUT write"
        );
        assert_eq!(members(&folded), {
            let mut keys = vec![fixture.survivor.0.clone(), extra.0.clone()];
            keys.sort();
            keys
        });
        if deferred {
            assert!(
                folded["catalog"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["applied_generation"] == 1)
            );
        }
        let live = vec![fixture.survivor.clone(), extra];
        worker.fault("");
        let before_cas = worker.usage();
        assert_eq!(
            worker.publish(1, &live, proof.as_deref()).json(),
            json!({"published":true})
        );
        let cas_delta = worker.usage().checked_workload_delta(&before_cas).unwrap();
        assert_eq!(cas_delta.do_rows_written, Some(1));
        assert_eq!(
            cas_delta.r2_class_a + cas_delta.r2_class_b + cas_delta.r2_deletes,
            0
        );
        let final_state = worker.state();
        assert_eq!(catalog_writes(&final_state), 0);
        assert_eq!(final_state["manifest"]["garbage"], json!([]));
        let metrics = assert_live(worker, &live, 128);
        assert!(metrics["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
        deltas.push(delta.clone());
        reports.push(json!({"deferred":deferred,"retirement":response.json(),
            "retirement_delta":delta,"retired_state":retired_state,
            "put_delta":put_delta,"folded_state":folded,"cas_delta":cas_delta,
            "final_state":final_state,"metrics":metrics}));
    }
    assert_eq!(
        deltas[0].do_rows_written,
        deltas[1].do_rows_written.map(|n| n + 1)
    );
    runtime.stop();
    report(&root, "normal", &reports);
}

#[test]
#[ignore = "owns 8834; actual proof rejection, DO eviction, registry loss and old-client recovery"]
fn actual_deferred_catalog_proofs_and_compatibility() {
    let root = artifacts("proofs");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let gate = Worker::new("gate");
    for body in [
        json!({"catalog_deferred":1}),
        json!({"catalog_deferred":1,"catalog_pages":1}),
        json!({"catalog_deferred":1,"catalog_peak":1}),
    ] {
        let mut body = body;
        body["codec_origin"] = json!("http://127.0.0.1:1");
        let ack = gate.json("POST", "/_services/register", &body, None).json();
        assert!(ack.get("catalog_deferred").is_none());
    }
    let spoof = Worker::new("header-spoof");
    spoof.register(false);
    let forged = [
        ("x-git-zero-internal-catalog-deferred", "1"),
        (
            "x-git-zero-internal-retirement-confirmation",
            "[1,1,\"forged\"]",
        ),
        ("x-git-zero-internal-unrecognized", "forged"),
    ];
    let headers = spoof
        .request_headers("GET", "/_services/test/headers", &[], Some("01"), &forged)
        .json();
    assert!(
        headers
            .get("x-git-zero-internal-catalog-deferred")
            .is_none()
    );
    assert!(
        headers
            .get("x-git-zero-internal-retirement-confirmation")
            .is_none()
    );
    assert!(headers.get("x-git-zero-internal-unrecognized").is_none());
    assert!(headers.get(PROOF).is_none());
    let data = b"forged private header cannot activate durable v2";
    let key = format!("chunks/{}", hash(data));
    assert_eq!(
        spoof
            .request_headers(
                "PUT",
                &format!("/_services/blob/{key}"),
                data,
                Some("01"),
                &forged
            )
            .status,
        204
    );
    assert_eq!(spoof.state()["catalog"][0]["entries"][4], 1);

    let codec = TcpListener::bind("127.0.0.1:0").unwrap();
    let codec_origin = format!("http://{}", codec.local_addr().unwrap());
    let callback = std::thread::spawn(move || {
        let (mut stream, _) = codec.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 2048];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let headers: BTreeMap<_, _> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        let body = serde_json::to_vec(&headers).unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
    });
    let public = Worker::new("public-header-spoof");
    public
        .json(
            "POST",
            "/_services/register",
            &json!({"codec_origin":codec_origin}),
            None,
        )
        .json();
    let callback_headers = public
        .request_headers(
            "GET",
            &format!(
                "/{}/repo.git/info/refs?service=git-upload-pack",
                public.repo
            ),
            &[],
            Some("01"),
            &forged,
        )
        .json();
    callback.join().unwrap();
    assert!(
        !callback_headers
            .as_object()
            .unwrap()
            .keys()
            .any(|name| name.starts_with("x-git-zero-"))
    );
    reports.push(json!({"case":"service-and-public-private-header-spoof","service":headers,"public":callback_headers}));
    let donor = Fixture::new("donor", true);
    let donor_proof = checked_proof(&donor.retire());
    for invalid in [
        "missing",
        "short",
        "uppercase",
        "wrong-repo",
        "oversized",
        "session",
    ] {
        let fixture = Fixture::new(invalid, true);
        let good = checked_proof(&fixture.retire());
        let token = match invalid {
            "missing" => None,
            "short" => Some(good[..176].to_string()),
            "uppercase" => Some(good.to_ascii_uppercase()),
            "wrong-repo" => Some(donor_proof.clone()),
            "oversized" => Some("0".repeat(513)),
            "session" => {
                fixture.worker.register(true);
                Some(good)
            }
            _ => unreachable!(),
        };
        assert!(
            fixture
                .worker
                .json("POST", "/_services/test/evict", &json!({}), None)
                .status
                >= 500
        );
        fixture.worker.fault("");
        let before = fixture.worker.usage();
        let token = if invalid == "oversized" {
            let key = format!("chunks/{}", hash(&[9; 17]));
            let response = fixture.worker.request(
                "PUT",
                &format!("/_services/blob/{key}"),
                &[9; 17],
                token.as_deref(),
            );
            assert_eq!(
                response.status, 400,
                "oversized header is rejected before DO dispatch"
            );
            assert_eq!(
                fixture.worker.usage().calls.r2_class_b,
                before.calls.r2_class_b
            );
            None
        } else {
            token
        };
        let extra = (fixture.worker.put(&[9; 17], token.as_deref()), 17);
        let after = fixture.worker.usage();
        assert_eq!(
            after.calls.r2_class_b - before.calls.r2_class_b,
            3,
            "{invalid}: paid current-debt HEAD fallback"
        );
        assert_eq!(
            after.operations["r2_put"].attempts - before.operations["r2_put"].attempts,
            1
        );
        assert_eq!(after.calls.r2_deletes, before.calls.r2_deletes);
        let live = vec![fixture.survivor.clone(), extra];
        assert_eq!(
            fixture.worker.publish(1, &live, token.as_deref()).json()["published"],
            true
        );
        let metrics = assert_inventory(&fixture.worker, &live, 128);
        assert_eq!(metrics["accounting_complete"], false);
        assert_eq!(members(&fixture.worker.state()).len(), 2);
        reports.push(json!({"case":invalid,"before":before,"after":after,"metrics":metrics}));
    }

    let evicted = Fixture::new("do-eviction", true);
    let token = checked_proof(&evicted.retire());
    let epoch = evicted.worker.usage().do_instance_epoch;
    let eviction = evicted
        .worker
        .json("POST", "/_services/test/evict", &json!({}), None);
    assert!(eviction.status >= 500);
    let before = evicted.worker.usage();
    assert_ne!(
        before.do_instance_epoch, epoch,
        "actual DO instance must change"
    );
    let extra = (evicted.worker.put(&[10; 13], Some(&token)), 13);
    let after = evicted.worker.usage();
    assert_eq!(
        after.calls.r2_class_b - before.calls.r2_class_b,
        0,
        "Worker session survives DO-only eviction"
    );
    assert_eq!(
        after.operations["r2_put"].attempts - before.operations["r2_put"].attempts,
        1
    );
    let live = vec![evicted.survivor.clone(), extra];
    assert_eq!(
        evicted.worker.publish(1, &live, Some(&token)).json()["published"],
        true
    );
    let metrics = evicted.worker.usage_json();
    assert_eq!(
        metrics["accounting_complete"], false,
        "abort truthfully has no receipt"
    );
    assert_eq!(metrics["r2_live_objects"], 2);
    assert_eq!(metrics["r2_live_bytes"], 45);
    assert_eq!(metrics["r2_peak_bytes"], 128);
    assert_eq!(members(&evicted.worker.state()).len(), 2);
    reports
        .push(json!({"case":"DO-only-eviction","before":before,"after":after,"metrics":metrics}));

    let legacy = Fixture::new("v1-conversion", false);
    legacy.worker.register(true);
    legacy.worker.fault("");
    let response = legacy.retire();
    retired_ok(&response, &legacy.keys());
    let state = legacy.worker.state();
    assert!(
        catalog_writes(&state) >= 1,
        "v1 cannot claim free deferred retirement"
    );
    assert_eq!(members(&state), vec![legacy.survivor.0.clone()]);
    reports.push(json!({"case":"v1-paid-conversion","state":state}));

    let old_native = Fixture::new("old-native", true);
    old_native.retire().json();
    old_native.worker.register(false);
    assert!(
        old_native
            .worker
            .json("POST", "/_services/test/evict", &json!({}), None)
            .status
            >= 500
    );
    let before = old_native.worker.usage();
    old_native.worker.put(&[11; 19], None);
    let after = old_native.worker.usage();
    assert_eq!(after.calls.r2_class_b - before.calls.r2_class_b, 3);
    assert_eq!(members(&old_native.worker.state()).len(), 2);
    reports.push(json!({"case":"old-native-pending-v2","before":before,"after":after}));

    let lost = Fixture::new("registry-loss", true);
    let stale = checked_proof(&lost.retire());
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let before = lost.worker.usage();
    let extra = (lost.worker.put(&[12; 15], Some(&stale)), 15);
    let delta = lost.worker.usage().checked_workload_delta(&before).unwrap();
    assert_eq!(
        delta.r2_class_b, 3,
        "registry loss cannot trust old session"
    );
    let live = vec![lost.survivor.clone(), extra];
    assert_eq!(
        lost.worker.publish(1, &live, Some(&stale)).json()["published"],
        true
    );
    reports.push(json!({"case":"full-runtime-registry-loss","delta":delta,"metrics":assert_live(&lost.worker,&live,128)}));
    runtime.stop();
    report(&root, "proofs", &reports);
}

#[test]
#[ignore = "owns 8834; same-generation proof replay and pre-PUT durability across faults/restart"]
fn actual_deferred_catalog_resurrection_replay() {
    let root = artifacts("resurrection");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let mut reopen = Vec::new();
    for mode in ["", "barrier-sync", "catalog-after-put", "sync-after-put"] {
        let fixture = Fixture::new(if mode.is_empty() { "clean" } else { mode }, true);
        let proof = checked_proof(&fixture.retire());
        fixture.worker.fault(mode);
        let (key, data) = &fixture.retired[0];
        let before = fixture.worker.usage();
        let response =
            fixture
                .worker
                .request("PUT", &format!("/_services/blob/{key}"), data, Some(&proof));
        let after = fixture.worker.usage();
        let delta = after.checked_workload_delta(&before).unwrap();
        let state = fixture.worker.state();
        let trace = state["trace"].as_array().unwrap();
        let put = trace
            .iter()
            .position(|event| event.as_str().unwrap().starts_with("r2:put:"));
        if mode == "barrier-sync" {
            assert!(response.status >= 500);
            assert!(
                put.is_none(),
                "ambiguous pre-PUT barrier must prevent R2 PUT"
            );
            assert_eq!(delta.r2_class_a, 0);
            assert_eq!(fixture.worker.get(key).status, 404);
        } else {
            let put = put.expect("actual resurrection R2 PUT");
            assert!(
                trace[..put].iter().any(|event| event == "sync:durable"),
                "acknowledge removal durably before resurrecting R2 key"
            );
            assert!(trace[..put].iter().any(|event| {
                event
                    .as_str()
                    .unwrap()
                    .starts_with("INSERT INTO blob_catalog")
            }));
            assert_eq!(delta.r2_class_a, 1);
            assert_eq!(fixture.worker.get(key).body, *data);
            assert_eq!(response.status, if mode.is_empty() { 204 } else { 500 });
        }
        assert_eq!(delta.r2_class_b, 0);
        if mode.is_empty() {
            let extra = (fixture.worker.put(&[15; 11], Some(&proof)), 11);
            assert!(
                members(&fixture.worker.state()).contains(key),
                "same-g replay cannot remove resurrection"
            );
            let live = vec![fixture.survivor.clone(), (key.clone(), data.len()), extra];
            assert_live(&fixture.worker, &live, 128);
            reopen.push((fixture.worker.clone(), live, 1, proof, false));
        } else {
            assert_eq!(state["certain"], false);
            let mut live = vec![fixture.survivor.clone()];
            if mode != "barrier-sync" {
                live.push((key.clone(), data.len()));
            }
            reopen.push((fixture.worker.clone(), live, 1, proof, true));
        }
        reports.push(json!({"case":mode,"status":response.status,"delta":delta,"state":state}));
    }
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    for (worker, live, generation, stale, needs_gc) in reopen {
        worker.register(true);
        let keys: Vec<_> = live.iter().map(|(key, _)| key.clone()).collect();
        if needs_gc {
            worker.gc(generation, &keys).json();
        }
        let before = worker.usage();
        let extra = (worker.put(&[16; 9], Some(&stale)), 9);
        let delta = worker.usage().checked_workload_delta(&before).unwrap();
        assert_eq!(
            delta.r2_class_b, 0,
            "durable acknowledgement/GC prevents old debt replay"
        );
        let mut live = live;
        live.push(extra);
        assert_eq!(
            worker.publish(generation, &live, Some(&stale)).json()["published"],
            true
        );
        let metrics = assert_live(&worker, &live, 128);
        let state = worker.state();
        assert_eq!(state["manifest"]["garbage"], json!([]));
        assert_eq!(members(&state).len(), live.len());
        assert!(metrics["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
        reports.push(
            json!({"case":"reopened-old-token","repo":worker.repo,"state":state,"metrics":metrics}),
        );
    }
    runtime.stop();
    report(&root, "resurrection", &reports);
}

fn encoded_page(
    prefix: &str,
    entries: &[(String, usize)],
    peak: u64,
    generation: Option<u64>,
) -> Value {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut bytes = vec![
        0x47,
        0x5a,
        0x43,
        0x50,
        if generation.is_some() { 2 } else { 1 },
        sorted.len() as u8,
    ];
    if let Some(generation) = generation {
        bytes.extend(generation.to_le_bytes());
    }
    for (key, size) in &sorted {
        let digest = key.strip_prefix("chunks/").unwrap();
        for offset in (0..64).step_by(2) {
            bytes.push(u8::from_str_radix(&digest[offset..offset + 2], 16).unwrap());
        }
        if generation.is_some() {
            bytes.extend((*size as u32).to_le_bytes());
        } else {
            bytes.extend((*size as u64).to_le_bytes());
        }
    }
    json!({"key":format!("pages/{prefix}"),
        "size":sorted.iter().map(|(_, size)| size).sum::<usize>(),
        "peak_bytes":peak,"entries":bytes})
}

fn matching_data(prefix: char, count: usize, label: &str) -> Vec<Vec<u8>> {
    let mut values = Vec::new();
    for nonce in 0..100_000 {
        let data = format!("{label} matching actual R2 member {nonce}").into_bytes();
        if hash(&data).starts_with(prefix) {
            values.push(data);
        }
        if values.len() == count {
            return values;
        }
    }
    panic!("bounded fixture search could not find SHA256 prefix");
}

#[test]
#[ignore = "owns 8834; no-PUT CAS, unrelated-page mutation, split and full-GC acknowledgement propagation"]
fn actual_deferred_catalog_flush_split_and_gc() {
    let root = artifacts("flush");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();

    let cas = Fixture::new("no-put-cas", true);
    let proof = checked_proof(&cas.retire());
    cas.worker.fault("");
    let before = cas.worker.usage();
    assert_eq!(
        cas.worker
            .publish(1, std::slice::from_ref(&cas.survivor), Some(&proof))
            .json()["published"],
        true
    );
    let delta = cas.worker.usage().checked_workload_delta(&before).unwrap();
    assert_eq!(delta.r2_class_a + delta.r2_class_b + delta.r2_deletes, 0);
    assert_eq!(
        delta.do_rows_written,
        Some(2),
        "flush page before replacing durable debt marker"
    );
    let state = cas.worker.state();
    assert_eq!(catalog_writes(&state), 1);
    assert_eq!(members(&state), vec![cas.survivor.0.clone()]);
    assert_eq!(state["manifest"]["garbage"], json!([]));
    reports.push(json!({"case":"no-put-CAS","delta":delta,"state":state}));

    let stale_cas = Fixture::new("stale-CAS-unknown", true);
    checked_proof(&stale_cas.retire());
    assert!(
        stale_cas
            .worker
            .json("POST", "/_services/test/evict", &json!({}), None)
            .status
            >= 500
    );
    stale_cas.worker.fault("");
    let before = stale_cas.worker.usage();
    assert_eq!(
        stale_cas
            .worker
            .retire(
                0,
                &stale_cas.keys(),
                std::slice::from_ref(&stale_cas.survivor)
            )
            .json(),
        json!({"version":1,"published":false,"retirement":null})
    );
    assert_eq!(
        stale_cas
            .worker
            .publish(0, std::slice::from_ref(&stale_cas.survivor), None)
            .json(),
        json!({"published":false})
    );
    let after = stale_cas.worker.usage();
    assert_eq!(
        after.calls.r2_class_b, before.calls.r2_class_b,
        "stale CAS cannot HEAD current debt"
    );
    assert_eq!(after.calls.r2_deletes, before.calls.r2_deletes);
    assert_eq!(
        after.operations["r2_put"].attempts,
        before.operations["r2_put"].attempts
    );
    let state = stale_cas.worker.state();
    assert_eq!(catalog_writes(&state), 0);
    assert_eq!(
        members(&state).len(),
        4,
        "stale CAS leaves durable pending page unchanged"
    );
    assert_eq!(state["manifest"]["generation"], 1);
    assert_eq!(state["manifest"]["garbage"].as_array().unwrap().len(), 3);
    let before_recovery = stale_cas.worker.usage();
    let extra = (stale_cas.worker.put(&[24; 23], None), 23);
    let recovered = stale_cas.worker.usage();
    assert_eq!(
        recovered.calls.r2_class_b - before_recovery.calls.r2_class_b,
        3
    );
    let live = vec![stale_cas.survivor.clone(), extra];
    stale_cas.worker.publish(1, &live, None).json();
    reports.push(json!({"case":"stale-CAS-does-no-retirement-IO","before":before,"after":after,
        "state":state,"recovered":recovered,"metrics":assert_inventory(&stale_cas.worker,&live,128)}));

    let unrelated = Worker::new("unrelated-page");
    unrelated.register(true);
    let a = matching_data('0', 4, "debt-page");
    let b = matching_data('f', 2, "unrelated-page");
    let a_keys: Vec<_> = a
        .iter()
        .map(|data| (unrelated.put(data, None), data.len()))
        .collect();
    let b_key = (unrelated.put(&b[0], None), b[0].len());
    let peak = a.iter().map(Vec::len).sum::<usize>() + b[0].len();
    unrelated.seed(vec![
        encoded_page("0", &a_keys, peak as u64, Some(0)),
        encoded_page("f", std::slice::from_ref(&b_key), peak as u64, Some(0)),
    ]);
    let retired: Vec<_> = a_keys[..3].iter().map(|(key, _)| key.clone()).collect();
    let mut live = vec![a_keys[3].clone(), b_key];
    let proof = checked_proof(&unrelated.retire(0, &retired, &live));
    unrelated.fault("");
    let before = unrelated.usage();
    let extra = (unrelated.put(&b[1], Some(&proof)), b[1].len());
    let delta = unrelated.usage().checked_workload_delta(&before).unwrap();
    assert_eq!(delta.r2_class_a, 1);
    assert_eq!(delta.r2_class_b, 0);
    assert_eq!(delta.do_rows_written, Some(2));
    let state = unrelated.state();
    assert_eq!(catalog_writes(&state), 2);
    let trace = state["trace"].as_array().unwrap();
    let put = trace
        .iter()
        .position(|event| event.as_str().unwrap().starts_with("r2:put:"))
        .unwrap();
    assert!(trace[..put].iter().any(|event| event == "sync:durable"));
    live.push(extra);
    assert_eq!(
        unrelated.publish(1, &live, Some(&proof)).json()["published"],
        true
    );
    assert_live(&unrelated, &live, peak as u64);
    reports.push(json!({"case":"unrelated-page-barrier","delta":delta,"state":state}));

    let split = Worker::new("split-propagation");
    split.register(true);
    let data: Vec<_> = (0..37)
        .map(|index| format!("split real payload {index:03}").into_bytes())
        .collect();
    let mut initial: Vec<_> = data[..32]
        .iter()
        .map(|value| (split.put(value, None), value.len()))
        .collect();
    assert_eq!(split.state()["catalog"].as_array().unwrap().len(), 1);
    let retired: Vec<_> = initial[..3].iter().map(|(key, _)| key.clone()).collect();
    let mut live = initial.split_off(3);
    let proof = checked_proof(&split.retire(0, &retired, &live));
    live.push((split.put(&data[0], Some(&proof)), data[0].len()));
    for value in &data[32..35] {
        live.push((split.put(value, Some(&proof)), value.len()));
    }
    let state = split.state();
    assert_eq!(members(&state).len(), 33);
    assert!(state["catalog"].as_array().unwrap().len() > 1);
    assert!(
        state["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["applied_generation"].as_u64().unwrap() >= 1),
        "every split child inherits the applied generation"
    );
    split.put(&data[0], Some(&proof));
    assert!(members(&split.state()).contains(&retired[0]));
    assert_eq!(
        split.publish(1, &live, Some(&proof)).json()["published"],
        true
    );
    let keep = vec![live[0].clone(), live[29].clone()];
    split
        .gc(
            2,
            &keep.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    let rebuilt = split.state();
    assert_eq!(members(&rebuilt).len(), 2);
    assert!(
        rebuilt["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["applied_generation"].as_u64().unwrap() >= 1)
    );
    let before = split.usage();
    let extra = (split.put(&data[35], Some(&proof)), data[35].len());
    let delta = split.usage().checked_workload_delta(&before).unwrap();
    assert_eq!(delta.r2_class_b, 0);
    let mut keep = keep;
    keep.push(extra);
    assert_eq!(
        split.publish(2, &keep, Some(&proof)).json()["published"],
        true
    );
    let peak = split.usage().r2_peak_bytes;
    assert_live(&split, &keep, peak);
    reports.push(
        json!({"case":"split-and-full-GC","split_state":state,"rebuilt":rebuilt,"delta":delta}),
    );

    let stale = Fixture::new("wrong-generation", true);
    let old = checked_proof(&stale.retire());
    let extra = (stale.worker.put(&[18; 15], Some(&old)), 15);
    let live = vec![stale.survivor.clone(), extra.clone()];
    stale.worker.publish(1, &live, Some(&old)).json();
    let fresh = stale
        .worker
        .retire(2, &[extra.0], std::slice::from_ref(&stale.survivor));
    checked_proof(&fresh);
    assert!(
        stale
            .worker
            .json("POST", "/_services/test/evict", &json!({}), None)
            .status
            >= 500
    );
    let before = stale.worker.usage();
    let extra = (stale.worker.put(&[19; 12], Some(&old)), 12);
    let after = stale.worker.usage();
    assert_eq!(
        after.calls.r2_class_b - before.calls.r2_class_b,
        1,
        "old generation token cannot resolve current debt"
    );
    stale
        .worker
        .publish(3, &[stale.survivor, extra], Some(&old))
        .json();
    reports.push(json!({"case":"wrong-generation","before":before,"after":after,"state":stale.worker.state()}));
    runtime.stop();
    report(&root, "flush", &reports);
}

#[test]
#[ignore = "owns 8834; real partial DELETE/unknown SQL receipts and observer/reload uncertainty isolation"]
fn actual_deferred_catalog_failure_receipts() {
    let root = artifacts("failures");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();

    for mode in [
        "malformed-retirement-reply",
        "wrong-outcome-order",
        "lost-retirement-reply",
    ] {
        let fixture = Fixture::new(mode, true);
        fixture.worker.fault(mode);
        let before = fixture.worker.usage();
        let response = fixture.retire();
        assert!(
            response.proof().is_none(),
            "{mode}: invalid reply must not mint proof"
        );
        assert_eq!(
            response.status,
            if mode == "lost-retirement-reply" {
                502
            } else {
                200
            }
        );
        let after = fixture.worker.usage();
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.worker_service_requests, 1);
        assert_eq!(delta.do_requests, 1);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.r2_class_a + delta.r2_class_b, 0);
        assert!(delta.do_rows_written.is_some());
        assert_eq!(
            after.operations["manifest_cas"].successes
                - before
                    .operations
                    .get("manifest_cas")
                    .map_or(0, |op| op.successes),
            1
        );
        let metrics = assert_live(
            &fixture.worker,
            std::slice::from_ref(&fixture.survivor),
            128,
        );
        reports.push(json!({"case":mode,"status":response.status,"body":String::from_utf8_lossy(&response.body),"delta":delta,"metrics":metrics}));
    }
    for mode in ["", "wrong-uploaded-member"] {
        let fixture = Fixture::new(if mode.is_empty() { "fused-good" } else { mode }, true);
        fixture.worker.fault(mode);
        let data = [22; 25];
        let before = fixture.worker.usage();
        let response = fixture.worker.upload_retire(
            0,
            &fixture.keys(),
            std::slice::from_ref(&fixture.survivor),
            &data,
        );
        let body = response.json();
        assert_eq!(body["published"], true);
        if mode.is_empty() {
            checked_proof(&response);
        } else {
            assert!(
                response.proof().is_none(),
                "fused receipt must match uploaded member"
            );
        }
        let delta = fixture
            .worker
            .usage()
            .checked_workload_delta(&before)
            .unwrap();
        assert_eq!(delta.worker_service_requests, 1);
        assert_eq!(delta.do_requests, 1);
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_class_b, 0);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.do_rows_written, Some(2));
        let live = vec![
            fixture.survivor.clone(),
            (format!("chunks/{}", hash(&data)), data.len()),
        ];
        let metrics = assert_live(&fixture.worker, &live, 153);
        reports.push(json!({"case":"fused-proof-member-validation","fault":mode,"response":body,"delta":delta,"metrics":metrics}));
    }

    let reload = Fixture::new("resolved-pending-reload", true);
    checked_proof(&reload.retire());
    assert_eq!(
        reload
            .worker
            .json("POST", "/_services/test/reload", &json!({}), None)
            .json()["certain"],
        true
    );
    let before = reload.worker.usage();
    let extra = (reload.worker.put(&[23; 27], None), 27);
    let delta = reload
        .worker
        .usage()
        .checked_workload_delta(&before)
        .unwrap();
    assert_eq!(
        delta.r2_class_b, 3,
        "reload discards resolved volatile deltas with stale SQL totals"
    );
    assert_eq!(delta.r2_class_a, 1);
    let live = vec![reload.survivor, extra];
    let metrics = assert_live(&reload.worker, &live, 128);
    reload.worker.publish(1, &live, None).json();
    reports.push(json!({"case":"resolved-pending-reload","delta":delta,"metrics":metrics,"state":reload.worker.state()}));

    let partial = Fixture::new("partial-delete", true);
    partial.worker.page_fault("delete-second");
    let before = partial.worker.usage();
    let response = partial.retire();
    assert!(
        response.proof().is_none(),
        "partial success cannot mint an all-deleted proof"
    );
    let body = response.json();
    let outcomes: Vec<_> = body["retirement"]["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|outcome| outcome["deleted"].as_bool().unwrap())
        .collect();
    assert_eq!(outcomes, [true, false, true]);
    let after = partial.worker.usage();
    let delta = after.checked_workload_delta(&before).unwrap();
    assert_eq!(delta.r2_deletes, 3);
    assert_eq!(
        after.operations["r2_delete"].failures
            - before
                .operations
                .get("r2_delete")
                .map_or(0, |op| op.failures),
        1
    );
    assert_eq!(
        partial.worker.get(&partial.retired[1].0).body,
        partial.retired[1].1
    );
    assert_eq!(partial.worker.get(&partial.retired[0].0).status, 404);
    reports.push(json!({"case":"partial-delete","response":body,"delta":delta,"state":partial.worker.state()}));

    let unknown = Fixture::new("unknown-cursor", true);
    unknown.worker.fault("cursor-unknown");
    let before = unknown.worker.usage();
    let response = unknown.retire();
    retired_ok(&response, &unknown.keys());
    assert!(
        response.proof().is_none(),
        "unknown SQL counters cannot mint proof"
    );
    let after = unknown.worker.usage();
    assert_eq!(after.calls.do_rows_read, None);
    assert_eq!(after.calls.do_rows_written, None);
    assert_eq!(after.calls.r2_deletes - before.calls.r2_deletes, 3);
    assert_eq!(
        after.operations["manifest_cas"].successes
            - before
                .operations
                .get("manifest_cas")
                .map_or(0, |op| op.successes),
        1
    );
    assert_eq!(
        after.operations["r2_delete"].successes
            - before
                .operations
                .get("r2_delete")
                .map_or(0, |op| op.successes),
        3
    );
    reports.push(json!({"case":"known-side-effects-unknown-SQL","response":response.json(),"usage":unknown.worker.usage_json()}));

    let uncertain = Fixture::new("arbitrary-uncertainty", true);
    let token = checked_proof(&uncertain.retire());
    uncertain.worker.fault("catalog-after-put");
    let key = format!("chunks/{}", hash(&[20; 25]));
    let response = uncertain.worker.request(
        "PUT",
        &format!("/_services/blob/{key}"),
        &[20; 25],
        Some(&token),
    );
    assert_eq!(response.status, 500);
    assert_eq!(uncertain.worker.state()["certain"], false);
    for _ in 0..2 {
        uncertain.worker.usage_json();
        assert_eq!(
            uncertain
                .worker
                .json("POST", "/_services/test/reload", &json!({}), None)
                .json()["certain"],
            false
        );
        assert_eq!(uncertain.worker.state()["certain"], false);
    }
    let blocked = uncertain.worker.request(
        "PUT",
        &format!("/_services/blob/{}", uncertain.retired[0].0),
        &uncertain.retired[0].1,
        Some(&token),
    );
    assert_eq!(
        blocked.status, 409,
        "proof cannot clear arbitrary uncertainty"
    );
    let live = vec![uncertain.survivor.clone(), (key, 25)];
    uncertain
        .worker
        .gc(
            1,
            &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    uncertain.worker.publish(1, &live, Some(&token)).json();
    let metrics = assert_live(&uncertain.worker, &live, 128);
    assert_eq!(uncertain.worker.state()["manifest"]["garbage"], json!([]));
    reports.push(json!({"case":"arbitrary-uncertainty-isolation","metrics":metrics,"state":uncertain.worker.state()}));
    runtime.stop();
    report(&root, "failures", &reports);
}

#[test]
#[ignore = "owns 8834; durable v1/v2 decoding independent of registration and compact bounded storage after GC/reopen"]
fn actual_deferred_catalog_durable_codec_and_storage() {
    let root = artifacts("storage");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let empty = Worker::new("empty-scalar");
    empty.register(true);
    let empty_key = empty.put(b"", None);
    assert_eq!(empty.get(&empty_key).body, b"");
    let empty_metrics = assert_live(&empty, &[(empty_key, 0)], 0);
    reports.push(json!({"case":"empty-scalar-PUT","metrics":empty_metrics,"state":empty.state()}));
    for generation in [None, Some(0), Some(1), Some(9_007_199_254_740_991)] {
        let worker = Worker::new("codec");
        worker.register(true);
        let data = b"durable decoding does not depend on the current registration";
        let key = worker.put(data, None);
        let row = encoded_page(
            "",
            &[(key.clone(), data.len())],
            data.len() as u64,
            generation,
        );
        assert_eq!(
            row["entries"].as_array().unwrap().len(),
            if generation.is_some() { 50 } else { 46 }
        );
        worker.seed(vec![row]);
        worker.register(false);
        let state = worker.state();
        assert_eq!(state["catalog"][0]["applied_generation"], json!(generation));
        assert_eq!(members(&state), vec![key.clone()]);
        assert_eq!(worker.get(&key).body, data);
        reports.push(json!({"case":"durable-codec","generation":generation,"state":state}));
    }
    let maximum = Worker::new("maximum-v2");
    maximum.register(true);
    maximum.put(
        b"enable actual durable schema before bounded codec seeding",
        None,
    );
    let boundary: Vec<_> = (0..32)
        .map(|index| {
            (
                format!("chunks/{index:064x}"),
                if index == 0 { 0 } else { 16 * 1024 * 1024 },
            )
        })
        .collect();
    let page = encoded_page("", &boundary, 31 * 16 * 1024 * 1024, Some(0));
    assert_eq!(page["entries"].as_array().unwrap().len(), 1166);
    maximum.seed(vec![page.clone()]);
    assert_eq!(
        maximum
            .json(
                "POST",
                "/_services/test/lookup",
                &json!({"key":boundary[31].0}),
                None
            )
            .json()["size"],
        16 * 1024 * 1024
    );
    assert_eq!(members(&maximum.state()).len(), 32);
    for kind in ["generation", "size", "length", "count"] {
        let invalid = Worker::new(kind);
        invalid.register(true);
        invalid.put(
            b"enable actual durable schema before invalid codec seeding",
            None,
        );
        let mut row = encoded_page("", &[(boundary[1].0.clone(), 1)], 1, Some(0));
        let bytes = row["entries"].as_array_mut().unwrap();
        match kind {
            "generation" => bytes[6..14].fill(json!(255)),
            "size" => {
                let oversized = (16 * 1024 * 1024 + 1u32).to_le_bytes();
                for (offset, byte) in oversized.iter().enumerate() {
                    bytes[46 + offset] = json!(byte);
                }
            }
            "length" => {
                bytes.pop();
            }
            "count" => bytes[5] = json!(33),
            _ => unreachable!(),
        }
        invalid.seed(vec![row]);
        assert_eq!(
            invalid
                .json(
                    "POST",
                    "/_services/test/lookup",
                    &json!({"key":boundary[1].0}),
                    None
                )
                .status,
            500
        );
        assert_eq!(invalid.state()["certain"], false);
        reports.push(json!({"case":"invalid-v2","kind":kind,"state":invalid.state()}));
    }
    reports.push(json!({"case":"maximum-compact-v2","state":maximum.state()}));
    let worker = Worker::new("real-shape-churn");
    worker.register(true);
    let mut generation = 0;
    let mut live = Vec::new();
    let mut peak = 0;
    for cycle in 0..6 {
        let mut added = Vec::new();
        for member in 0..4 {
            let data = format!(
                "repository bundle cycle={cycle} member={member} {}",
                "payload".repeat(128)
            )
            .into_bytes();
            added.push((worker.put(&data, None), data.len()));
        }
        peak = peak.max(
            live.iter()
                .chain(added.iter())
                .map(|(_, size)| *size as u64)
                .sum(),
        );
        let retired: Vec<_> = live.iter().map(|(key, _)| key.clone()).collect();
        let response = worker.retire(generation, &retired, &added);
        response.json();
        generation += 1;
        let token = response.proof().map(str::to_string);
        worker.publish(generation, &added, token.as_deref()).json();
        generation += 1;
        live = added;
        let metrics = assert_live(&worker, &live, peak);
        assert!(metrics["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
        assert_eq!(worker.state()["manifest"]["garbage"], json!([]));
        reports.push(json!({"case":"real-shape-cycle","cycle":cycle,"metrics":metrics}));
    }
    worker
        .gc(
            generation,
            &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    let final_metrics = assert_live(&worker, &live, peak);
    assert!(final_metrics["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);

    // Match the existing realistic SQLite shape: thirteen actual 5 MiB chunks,
    // eleven pack descriptors and thirty-three realistic reference names.
    let shape = Worker::new("thirteen-key-shape");
    shape.register(true);
    let mut shape_live = Vec::new();
    for index in 0..13 {
        let data = vec![index as u8; 5 * 1024 * 1024 + index];
        shape_live.push((shape.put(&data, None), data.len()));
    }
    let shape_peak: u64 = shape_live.iter().map(|(_, size)| *size as u64).sum();
    let refs: serde_json::Map<String, Value> = (0..33)
        .map(|index| {
            (
                format!("refs/heads/backend-shape-reference-{index:02}"),
                json!(format!("{index:040x}")),
            )
        })
        .collect();
    let shape_manifest = |generation, chunks: &[(String, usize)], garbage: &[String]| {
        let mut offset = 0;
        let pack_count = chunks.len().min(11);
        let packs: Vec<_> = (0..pack_count)
            .map(|index| {
                let count = if index < chunks.len() - pack_count {
                    2
                } else {
                    1
                };
                let bundle: Vec<_> = chunks[offset..offset + count]
                    .iter()
                    .map(|(key, len)| json!({"key":key,"len":len}))
                    .collect();
                let size: usize = chunks[offset..offset + count]
                    .iter()
                    .map(|(_, size)| size)
                    .sum();
                offset += count;
                json!({"id":format!("{index:040x}"),"pack":[],"index":[],"bundle":bundle,
                "pack_bytes":size - 20,"index_bytes":20,"level":index % 3})
            })
            .collect();
        json!({"generation":generation,"refs":refs,"packs":packs,"garbage":garbage})
    };
    assert_eq!(
        shape
            .json(
                "POST",
                "/_services/manifest",
                &json!({
                    "expected":0,"next":shape_manifest(1, &shape_live, &[])
                }),
                None
            )
            .json()["published"],
        true
    );
    let initial_shape = shape.usage_json();
    assert!(initial_shape["manifest_bytes"].as_u64().unwrap() >= 4_800);
    assert!(initial_shape["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
    let retired: Vec<_> = shape_live.drain(..3).map(|(key, _)| key).collect();
    let retirement = shape.json(
        "POST",
        "/_services/manifest/retire",
        &json!({
            "version":1,"expected":1,"next":shape_manifest(2, &shape_live, &retired)
        }),
        None,
    );
    retired_ok(&retirement, &retired);
    let token = checked_proof(&retirement);
    shape_live.push((shape.put(&[42; 4096], Some(&token)), 4096));
    assert_eq!(
        shape
            .json(
                "POST",
                "/_services/manifest",
                &json!({
                    "expected":2,"next":shape_manifest(3, &shape_live, &[])
                }),
                Some(&token)
            )
            .json()["published"],
        true
    );
    shape
        .gc(
            3,
            &shape_live
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
        )
        .json();
    let shape_final = shape.usage_json();
    assert_eq!(shape_final["r2_live_objects"], 11);
    assert_eq!(
        shape_final["r2_live_bytes"],
        shape_live.iter().map(|(_, size)| size).sum::<usize>()
    );
    assert_eq!(shape_final["r2_peak_bytes"], shape_peak);
    assert_eq!(shape_final["accounting_complete"], true);
    assert!(shape_final["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
    assert_eq!(shape.state()["manifest"]["garbage"], json!([]));
    reports.push(
        json!({"case":"realistic-thirteen-key-shape","initial":initial_shape,
        "final":shape_final,"state":shape.state()}),
    );
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let reopened = assert_live(&worker, &live, peak);
    assert!(reopened["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
    assert_eq!(worker.state()["manifest"]["garbage"], json!([]));
    reports.push(json!({"case":"final-GC-and-reopen","before":final_metrics,"reopened":reopened,"state":worker.state()}));
    let shape_reopened = shape.usage_json();
    assert!(shape_reopened["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
    assert_eq!(shape_reopened["r2_live_objects"], 11);
    assert_eq!(shape_reopened["r2_peak_bytes"], shape_peak);
    assert_eq!(shape.state()["manifest"]["garbage"], json!([]));
    reports.push(
        json!({"case":"realistic-shape-reopened","metrics":shape_reopened,
        "state":shape.state()}),
    );
    runtime.stop();
    report(&root, "storage", &reports);
}
