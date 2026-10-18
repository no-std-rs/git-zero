//! Rust owns actual Wrangler/R2/SQLite contracts on 8834. These are ignored so
//! ordinary tests cannot start services or collide with another runtime owner.
use git_zero_host::{accounting::ServiceUsage, hash};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/test/fused-metadata.toml");
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
    let root = std::env::var("GIT_ZERO_INLINE_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!(
                "git-zero-inline-{label}-{}-{}",
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
        "cloudflare/test/catalog-pages-worker.js",
        "cloudflare/test/peak-catalog-worker.js",
        "src/native.rs",
        "src/lib.rs",
        "cloudflare/test/fused-metadata.toml",
        "src/cloudflare.rs",
        "tests/fused_metadata.rs",
        "cloudflare/src/inline-catalog.js",
        "cloudflare/test/fused-metadata-worker.js",
        "cloudflare/test/fused-metadata.toml",
    ]
    .into_iter()
    .map(|file| (file, hash(&fs::read(package.join(file)).unwrap())))
    .collect();
    fs::write(
        root.join("source.json"),
        serde_json::to_vec_pretty(&fingerprints).unwrap(),
    )
    .unwrap();
    let binary = std::env::current_exe().unwrap();
    fs::write(root.join("runner.json"),serde_json::to_vec_pretty(&json!({"path":binary,"sha256":hash(&fs::read(&binary).unwrap()),"args":std::env::args().collect::<Vec<_>>()})).unwrap()).unwrap();
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
            "git-zero-inline-headers-{}-{}",
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
                "catalog_deferred":usize::from(deferred),"catalog_inline":usize::from(deferred)
            }),
            None,
        )
        .json()
    }
    fn usage_json(&self) -> Value {
        self.json("GET", "/_services/metrics", &Value::Null, None)
            .json()
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

#[test]
#[ignore = "owns 8834; mandatory actual realistic inline-owner storage and one-row feasibility gate"]
fn actual_inline_metadata_early_shape_gate() {
    let root = artifacts("early-shape");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let mut workers = Vec::new();
    for keys in [11usize, 13] {
        let worker = Worker::new(&format!("shape-{keys}"));
        worker.register(true);
        let mut live = Vec::new();
        for index in 0..keys - 1 {
            let data = vec![index as u8; 5 * 1024 * 1024 + index];
            live.push((worker.put(&data, None), data.len()));
        }
        let refs: serde_json::Map<String, Value> = (0..33)
            .map(|index| {
                (
                    format!("refs/heads/backend-shape-reference-{index:02}"),
                    json!(format!("{:040x}", index + 1)),
                )
            })
            .collect();
        let shape = |generation: u64, chunks: &[(String, usize)]| {
            let mut offset = 0;
            let packs:Vec<_>=(0..11.min(chunks.len())).map(|i| {
                let count=if i<chunks.len()-11.min(chunks.len()) {2} else {1};
                let bundle:Vec<_>=chunks[offset..offset+count].iter().map(|(key,len)|json!({"key":key,"len":len})).collect();
                let size:usize=chunks[offset..offset+count].iter().map(|(_,len)|len).sum();offset+=count;
                json!({"id":format!("{i:040x}"),"pack":[],"index":[],"bundle":bundle,"pack_bytes":size-20,"index_bytes":20,"level":i%3})
            }).collect();
            json!({"generation":generation,"refs":refs,"packs":packs,"garbage":[]})
        };
        assert_eq!(
            worker
                .json(
                    "POST",
                    "/_services/manifest",
                    &json!({"expected":0,"next":shape(1,&live)}),
                    None
                )
                .json()["published"],
            true
        );
        let before = worker.usage_json();
        let control = Worker::new(&format!("control-{keys}"));
        control.register(true);
        for (index, item) in live.iter().enumerate() {
            let data = vec![index as u8; 5 * 1024 * 1024 + index];
            assert_eq!(control.put(&data, None), item.0);
        }
        assert_eq!(
            control
                .json(
                    "POST",
                    "/_services/manifest",
                    &json!({"expected":0,"next":shape(1,&live)}),
                    None
                )
                .json()["published"],
            true
        );
        let control_before = control.usage_json();
        let data = vec![99u8; 4096];
        let key = control.put(&data, None);
        let mut control_live = live.clone();
        control_live.push((key, 4096));
        assert_eq!(
            control
                .json(
                    "POST",
                    "/_services/manifest",
                    &json!({"expected":1,"next":shape(2,&control_live)}),
                    None
                )
                .json()["published"],
            true
        );
        let control_after = control.usage_json();
        assert_eq!(
            control_after["do_rows_written"].as_u64().unwrap()
                - control_before["do_rows_written"].as_u64().unwrap(),
            2
        );
        let promoted = worker
            .json(
                "POST",
                "/_services/test/inline-prototype",
                &json!({"promote":true}),
                None,
            )
            .json();
        assert!(
            promoted["bytes"].as_u64().unwrap() <= SQLITE_LIMIT,
            "{promoted}"
        );
        let data = vec![99u8; 4096];
        let key = format!("chunks/{}", hash(&data));
        live.push((key.clone(), data.len()));
        let next = shape(2, &live);
        assert!(serde_json::to_vec(&next).unwrap().len() >= 4800);
        let fused = worker
            .json(
                "POST",
                "/_services/test/inline-prototype",
                &json!({"expected":1,"next":next,"chunk":key,"bytes":data}),
                None,
            )
            .json();
        assert_eq!(fused["published"], true);
        assert_eq!(fused["delta"]["do_rows_written"], 1);
        assert_eq!(fused["delta"]["r2_class_a"], 1);
        assert_eq!(fused["delta"]["r2_written_bytes"], 4096);
        assert_eq!(fused["delta"]["r2_deletes"], 0);
        assert!(fused["bytes"].as_u64().unwrap() <= SQLITE_LIMIT, "{fused}");
        let state = worker
            .json(
                "GET",
                "/_services/test/inline-prototype-state",
                &Value::Null,
                None,
            )
            .json();
        assert_eq!(state["objects"], keys);
        assert_eq!(state["manifest"]["packs"].as_array().unwrap().len(), 11);
        assert_eq!(state["manifest"]["refs"].as_object().unwrap().len(), 33);
        assert!(state["bytes"].as_u64().unwrap() <= SQLITE_LIMIT, "{state}");
        let gc = worker
            .json(
                "POST",
                "/_services/test/inline-prototype",
                &json!({"gc":true,"live":live.iter().map(|(key,_)|key).collect::<Vec<_>>()}),
                None,
            )
            .json();
        assert!(gc["bytes"].as_u64().unwrap() <= SQLITE_LIMIT, "{gc}");
        reports.push(json!({"keys":keys,"before":before,"control_before":control_before,"control_after":control_after,"promotion":promoted,"fused":fused,"state":state,"gc":gc}));
        workers.push(worker);
    }
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    for worker in workers {
        let state = worker
            .json(
                "GET",
                "/_services/test/inline-prototype-state",
                &Value::Null,
                None,
            )
            .json();
        assert!(state["bytes"].as_u64().unwrap() <= SQLITE_LIMIT, "{state}");
        reports.push(json!({"reopened":state}));
    }
    runtime.stop();
    fs::write(
        root.join("early-shape.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}

#[test]
#[ignore = "owns8834; production fused metadata normal receipts, old API, debt, stale, GC and reopen"]
fn actual_inline_metadata_production_gate() {
    let root = artifacts("production");
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("production");
    let ack = worker.register(true);
    assert_eq!(
        ack["catalog_inline"],
        json!({"version":1,"members":32,"manifest_bytes":8192})
    );
    let mut live = Vec::new();
    let mut reports = Vec::new();
    for generation in 0..5u64 {
        let data = vec![generation as u8 + 1; 4096];
        let key = format!("chunks/{}", hash(&data));
        let before = worker.usage_json();
        let reply = worker.upload_retire(generation, &[], &live, &data);
        let receipt = reply.json();
        assert_eq!(receipt["published"], true);
        live.push((key, 4096));
        let after = worker.usage_json();
        let state = worker
            .json("GET", "/_services/test/inline-state", &Value::Null, None)
            .json();
        assert_eq!(state["catalog"], json!([]));
        assert_eq!(
            state["root"]["entries"].as_array().unwrap().len(),
            live.len()
        );
        if generation > 0 {
            assert_eq!(
                after["do_rows_written"].as_u64().unwrap()
                    - before["do_rows_written"].as_u64().unwrap(),
                1
            );
        }
        assert_eq!(state["root"]["appliedGeneration"], generation);
        reports.push(
            json!({"g":generation+1,"before":before,"after":after,"state":state,"receipt":receipt}),
        );
    }
    let retired: Vec<_> = live.drain(..3).map(|(key, _)| key).collect();
    let data = vec![8u8; 4096];
    let key = format!("chunks/{}", hash(&data));
    let retirement = worker.upload_retire(5, &retired, &live, &data);
    assert_eq!(retirement.json()["published"], true);
    let token = retirement.proof().expect("known receipt proof").to_string();
    live.push((key, 4096));
    let state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(state["root"]["appliedGeneration"], 5);
    assert_eq!(state["root"]["entries"].as_array().unwrap().len(), 6);
    assert!(!state["debt"].is_null());
    let before = worker.usage_json();
    let data = vec![9u8; 4096];
    let key = format!("chunks/{}", hash(&data));
    let next = worker.upload_retire(6, &[], &live, &data);
    assert_eq!(next.json()["published"], true);
    live.push((key, 4096));
    let after = worker.usage_json();
    assert_eq!(
        after["do_rows_written"].as_u64().unwrap() - before["do_rows_written"].as_u64().unwrap(),
        1
    );
    assert_eq!(after["r2_class_b"], before["r2_class_b"]);
    // Stale fused upload keeps its real orphan/peak and never retires.
    let data = vec![10u8; 4096];
    let before = worker.usage_json();
    let stale = worker
        .upload_retire(6, &[live[0].0.clone()], &live[1..], &data)
        .json();
    assert_eq!(stale["published"], false);
    let after = worker.usage_json();
    assert_eq!(after["r2_deletes"], before["r2_deletes"]);
    assert_eq!(
        after["r2_live_objects"].as_u64().unwrap(),
        live.len() as u64 + 1
    );
    let current = worker
        .json("GET", "/_services/manifest", &Value::Null, None)
        .json();
    assert_eq!(current["generation"], 7);
    // GC must actually remove the stale orphan, preserve its temporary peak and rebuild sole ownership.
    let peak = after["r2_peak_bytes"].as_u64().unwrap();
    assert_eq!(
        worker
            .gc(
                7,
                &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>()
            )
            .json()["deleted"],
        1
    );
    let state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(state["root"]["peak"], peak);
    assert_eq!(state["catalog"], json!([]));
    assert_eq!(
        state["root"]["entries"].as_array().unwrap().len(),
        live.len()
    );
    // An old registration and independent scalar PUT/CAS remain productive.
    worker.register(false);
    let data = vec![11u8; 4096];
    let key = worker.put(&data, None);
    live.push((key, 4096));
    assert_eq!(
        worker.publish(7, &live, Some(&token)).json()["published"],
        true
    );
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let reopened = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert!(reopened["root"].is_object());
    assert_eq!(reopened["catalog"], json!([]));
    assert_eq!(worker.publish(8, &[], None).json()["published"], true);
    assert_eq!(worker.gc(9, &[]).json()["deleted"], live.len());
    let empty = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(empty["root"]["entries"], json!([]));
    assert!(empty["root"]["peak"].as_u64().unwrap() >= peak);
    reports.push(json!({"retirement":state,"reopened":reopened,"empty":empty}));
    runtime.stop();
    runtime = Runtime::start(&root, 3);
    let empty = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(empty["root"]["entries"], json!([]));
    assert!(empty["root"]["peak"].as_u64().unwrap() >= peak);
    reports.push(json!({"empty_reopened":empty}));
    runtime.stop();
    fs::write(
        root.join("production.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}

#[test]
#[ignore = "owns8834; coupled SQL/sync/DELETE failures retain debt and exact physical recovery"]
fn actual_inline_metadata_faults_and_replay() {
    let root = artifacts("faults");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    for mode in [
        "fused-sql-before",
        "fused-sql-after",
        "fused-transaction",
        "fused-sync-before",
        "fused-sync-after",
        "fused-delete-first",
        "delete-second",
        "cursor-unknown",
        "lost-retirement-reply",
        "malformed-retirement-reply",
    ] {
        let worker = Worker::new(mode);
        worker.register(true);
        let mut live = Vec::new();
        for g in 0..4u64 {
            let data = vec![g as u8 + 1; 32];
            let key = format!("chunks/{}", hash(&data));
            assert_eq!(
                worker.upload_retire(g, &[], &live, &data).json()["published"],
                true
            );
            live.push((key, 32));
        }
        let retired: Vec<_> = live.drain(..3).map(|(key, _)| key).collect();
        let data = vec![9u8; 32];
        let key = format!("chunks/{}", hash(&data));
        let before = worker.usage_json();
        worker.fault(mode);
        worker.page_fault(if mode == "delete-second" { mode } else { "" });
        let response = worker.upload_retire(4, &retired, &live, &data);
        let success =
            mode == "fused-delete-first" || mode == "delete-second" || mode == "cursor-unknown";
        if success {
            assert_eq!(response.status, 200, "{mode}");
        } else if mode == "malformed-retirement-reply" {
            assert_eq!(response.status, 200);
            assert_eq!(response.body, b"{");
        } else {
            assert!(
                response.status >= 500,
                "{mode}:{}",
                String::from_utf8_lossy(&response.body)
            );
        }
        let state = worker
            .json("GET", "/_services/test/inline-state", &Value::Null, None)
            .json();
        let after = worker.usage_json();
        fs::write(
            root.join(format!("raw-{mode}.json")),
            serde_json::to_vec_pretty(
                &json!({"state":state,"before":before,"after":after,"status":response.status}),
            )
            .unwrap(),
        )
        .unwrap();
        let rolled = matches!(
            mode,
            "fused-sql-before" | "fused-sql-after" | "fused-transaction"
        );
        let generation = if rolled { 4 } else { 5 };
        assert_eq!(
            state["manifest"]["generation"], generation,
            "{mode}:{state}"
        );
        assert_eq!(
            after["r2_written_bytes"].as_u64().unwrap()
                - before["r2_written_bytes"].as_u64().unwrap(),
            32
        );
        let deletes =
            after["r2_deletes"].as_u64().unwrap() - before["r2_deletes"].as_u64().unwrap();
        assert_eq!(
            deletes,
            if mode.starts_with("fused-sql")
                || mode.starts_with("fused-sync")
                || mode == "fused-transaction"
            {
                0
            } else {
                3
            },
            "{mode}"
        );
        assert!(
            response.proof().is_none(),
            "failed/unknown receipt cannot mint proof:{mode}"
        );
        if generation == 5 {
            assert_eq!(
                state["root"]["appliedGeneration"],
                if mode == "fused-delete-first" || mode == "delete-second" {
                    5
                } else {
                    4
                },
                "new debt must remain unacknowledged before actual retirement:{mode}"
            );
            live.push((key, 32));
        } else {
            live = retired
                .iter()
                .cloned()
                .map(|key| (key, 32))
                .chain(live)
                .collect();
        }
        worker.fault("");
        worker.page_fault("");
        let expected_peak = 5 * 32u64;
        worker
            .gc(
                generation,
                &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            )
            .json();
        let repaired = worker
            .json("GET", "/_services/test/inline-state", &Value::Null, None)
            .json();
        assert_eq!(repaired["root"]["peak"], expected_peak);
        assert_eq!(repaired["certain"], true);
        assert_eq!(repaired["catalog"], json!([]));
        assert_eq!(worker.usage_json()["r2_live_objects"], live.len());
        reports.push(json!({"mode":mode,"status":response.status,"before":before,"after":after,"failed":state,"repaired":repaired}));
    }
    // The old absence proof must be durably acknowledged before resurrection PUT.
    let worker = Worker::new("resurrection");
    worker.register(true);
    let mut live = Vec::new();
    let mut retired_data = Vec::new();
    for g in 0..4u64 {
        let data = vec![g as u8 + 1; 32];
        let key = format!("chunks/{}", hash(&data));
        assert_eq!(
            worker.upload_retire(g, &[], &live, &data).json()["published"],
            true
        );
        live.push((key, 32));
        retired_data.push(data);
    }
    let retired: Vec<_> = live.drain(..3).map(|(key, _)| key).collect();
    let retirement = worker.upload_retire(4, &retired, &live, &[9; 32]);
    let token = retirement.proof().unwrap().to_string();
    assert_eq!(retirement.json()["published"], true);
    live.push((format!("chunks/{}", hash(&[9; 32])), 32));
    worker.fault("fused-sql-before");
    let failed = worker.upload_retire(5, &[], &live, &retired_data[0]);
    assert!(failed.status >= 500);
    let state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(state["root"]["appliedGeneration"], 5);
    assert_eq!(state["manifest"]["generation"], 5);
    assert_eq!(worker.get(&retired[0]).status, 200);
    let before = worker.usage_json();
    let replay = worker.request(
        "PUT",
        &format!("/_services/blob/chunks/{}", hash(&[10; 32])),
        &[10; 32],
        Some(&token),
    );
    assert_eq!(replay.status, 409);
    let after = worker.usage_json();
    assert_eq!(after["r2_written_bytes"], before["r2_written_bytes"]);
    worker.fault("");
    worker
        .gc(
            5,
            &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    reports.push(json!({"resurrection":state,"replay_status":replay.status,"repaired":worker.json("GET","/_services/test/inline-state",&Value::Null,None).json()}));
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let reopened = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(reopened["root"]["appliedGeneration"], 5);
    assert_eq!(reopened["root"]["entries"].as_array().unwrap().len(), 2);
    reports.push(json!({"resurrection_reopened":reopened}));
    runtime.stop();
    fs::write(
        root.join("faults.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}

fn realistic_manifest(generation: u64, chunks: &[(String, usize)], garbage: &[String]) -> Value {
    let refs: serde_json::Map<String, Value> = (0..33)
        .map(|i| {
            (
                format!("refs/heads/backend-shape-reference-{i:02}"),
                json!(format!("{:040x}", i + 1)),
            )
        })
        .collect();
    let mut offset = 0;
    let packs:Vec<_>=(0..11.min(chunks.len())).map(|i|{let count=if i<chunks.len()-11.min(chunks.len()){2}else{1};let bundle:Vec<_>=chunks[offset..offset+count].iter().map(|(key,len)|json!({"key":key,"len":len})).collect();let size:usize=chunks[offset..offset+count].iter().map(|(_,len)|len).sum();offset+=count;json!({"id":format!("{i:040x}"),"pack":[],"index":[],"bundle":bundle,"pack_bytes":size-20,"index_bytes":20,"level":i%3})}).collect();
    json!({"generation":generation,"refs":refs,"packs":packs,"garbage":garbage})
}
fn framed_upload(
    worker: &Worker,
    expected: u64,
    next: Value,
    data: &[u8],
    proof: Option<&str>,
) -> Reply {
    let metadata=serde_json::to_vec(&json!({"version":1,"expected":expected,"next":next,"chunk":{"key":format!("chunks/{}",hash(data)),"len":data.len()}})).unwrap();
    let mut frame = b"GZUP".to_vec();
    frame.extend(1u32.to_le_bytes());
    frame.extend((metadata.len() as u32).to_le_bytes());
    frame.extend(metadata);
    frame.extend(data);
    worker.request("POST", "/_services/manifest/upload-retire", &frame, proof)
}
#[test]
#[ignore = "owns8834; final production 13-to11 actual 5MiB object shape, orphan GC and durable reopen"]
fn actual_inline_metadata_realistic_storage() {
    let root = artifacts("realistic-storage");
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("realistic");
    worker.register(true);
    let mut live = Vec::new();
    for i in 0..12 {
        let data = vec![i as u8; 5 * 1024 * 1024 + i];
        live.push((worker.put(&data, None), data.len()));
    }
    assert_eq!(
        worker
            .json(
                "POST",
                "/_services/manifest",
                &json!({"expected":0,"next":realistic_manifest(1,&live,&[])}),
                None
            )
            .json()["published"],
        true
    );
    let data = vec![99u8; 5 * 1024 * 1024 + 12];
    live.push((format!("chunks/{}", hash(&data)), data.len()));
    let next = realistic_manifest(2, &live, &[]);
    assert!(serde_json::to_vec(&next).unwrap().len() >= 4923);
    assert_eq!(
        framed_upload(&worker, 1, next, &data, None).json()["published"],
        true
    );
    let initial = worker.usage_json();
    assert_eq!(initial["r2_live_objects"], 13);
    assert!(
        initial["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT,
        "{initial}"
    );
    let before = worker.usage_json();
    assert_eq!(
        framed_upload(&worker, 2, realistic_manifest(3, &live, &[]), &data, None).json()["published"],
        true
    );
    let normal = worker.usage_json();
    assert_eq!(
        normal["do_rows_written"].as_u64().unwrap() - before["do_rows_written"].as_u64().unwrap(),
        1
    );
    assert!(normal["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT);
    let retired: Vec<_> = live.drain(..3).map(|(key, _)| key).collect();
    let receipt = worker.json(
        "POST",
        "/_services/manifest/retire",
        &json!({"version":1,"expected":3,"next":realistic_manifest(4,&live,&retired)}),
        None,
    );
    let token = receipt.proof().unwrap().to_string();
    assert_eq!(receipt.json()["published"], true);
    let data = vec![42; 4096];
    live.push((format!("chunks/{}", hash(&data)), data.len()));
    let next = realistic_manifest(5, &live, &[]);
    assert!(serde_json::to_vec(&next).unwrap().len() >= 4923);
    let before_fold = worker.usage_json();
    assert_eq!(
        framed_upload(&worker, 4, next, &data, Some(&token)).json()["published"],
        true
    );
    let folded = worker.usage_json();
    assert_eq!(
        folded["do_rows_written"].as_u64().unwrap()
            - before_fold["do_rows_written"].as_u64().unwrap(),
        1
    );
    assert_eq!(folded["r2_class_b"], before_fold["r2_class_b"]);
    worker.put(&[33; 4096], None);
    let gc = worker
        .gc(
            5,
            &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    assert_eq!(gc["deleted"], 1);
    let final_metrics = worker.usage_json();
    let final_state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(final_metrics["r2_live_objects"], 11);
    assert_eq!(final_metrics["r2_peak_bytes"], initial["r2_peak_bytes"]);
    assert!(
        final_metrics["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT,
        "{final_metrics}"
    );
    assert_eq!(final_state["catalog"], json!([]));
    assert_eq!(
        final_state["manifest"]["packs"].as_array().unwrap().len(),
        11
    );
    assert_eq!(
        final_state["manifest"]["refs"].as_object().unwrap().len(),
        33
    );
    let mut reports = vec![
        json!({"initial":initial,"normal_before":before,"normal":normal,"fold_before":before_fold,"folded":folded,"final":final_metrics,"state":final_state}),
    ];
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let reopened = worker.usage_json();
    let state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(reopened["r2_live_objects"], 11);
    assert_eq!(reopened["r2_peak_bytes"], initial["r2_peak_bytes"]);
    assert!(
        reopened["do_sqlite_bytes"].as_u64().unwrap() <= SQLITE_LIMIT,
        "{reopened}"
    );
    assert!(reopened["manifest_bytes"].as_u64().unwrap() >= 4923);
    assert_eq!(state["root"]["entries"].as_array().unwrap().len(), 11);
    reports.push(json!({"reopened":reopened,"state":state}));
    runtime.stop();
    fs::write(
        root.join("storage.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}
#[test]
#[ignore = "owns8834; demotion preserves valid API for33rdmember, large manifests, invalid ownership and scalar257"]
fn actual_inline_metadata_fallback_and_corruption() {
    let root = artifacts("fallback");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let worker = Worker::new("split");
    worker.register(true);
    let mut live = Vec::new();
    for g in 0..33u64 {
        let data = vec![g as u8; 32];
        let key = format!("chunks/{}", hash(&data));
        assert_eq!(
            worker.upload_retire(g, &[], &live, &data).json()["published"],
            true
        );
        live.push((key, 32));
        if g == 31 {
            let state = worker
                .json("GET", "/_services/test/inline-state", &Value::Null, None)
                .json();
            assert_eq!(state["root"]["entries"].as_array().unwrap().len(), 32);
        }
    }
    let split = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert!(split["root"].is_null());
    assert!(split["catalog"].as_array().unwrap().len() >= 2);
    assert_eq!(worker.usage_json()["r2_live_objects"], 33);
    reports.push(json!({"split":split}));
    let worker = Worker::new("large");
    worker.register(true);
    let data = [1u8; 32];
    assert_eq!(
        worker.upload_retire(0, &[], &[], &data).json()["published"],
        true
    );
    let live = vec![(format!("chunks/{}", hash(&data)), 32)];
    let mut next = manifest(2, &live, &[]);
    next["refs"] = Value::Object(
        (0..150)
            .map(|i| {
                (
                    format!("refs/heads/valid-large-reference-{i:04}"),
                    json!(format!("{:040x}", i + 1)),
                )
            })
            .collect(),
    );
    assert!(serde_json::to_vec(&next).unwrap().len() > 8192);
    let before = worker.usage_json();
    assert_eq!(worker.json("POST","/_services/manifest",&json!({"expected":0,"next":{ "generation":1,"refs":next["refs"],"packs":next["packs"]}}),None).json()["published"],false);
    let stale = worker.usage_json();
    let delta = serde_json::from_value::<ServiceUsage>(stale.clone())
        .unwrap()
        .checked_workload_delta(&serde_json::from_value(before.clone()).unwrap())
        .unwrap();
    assert_eq!(delta.r2_class_a, 0);
    assert_eq!(delta.r2_class_b, 0);
    assert_eq!(delta.do_rows_written, Some(0));
    assert_eq!(
        worker
            .json(
                "POST",
                "/_services/manifest",
                &json!({"expected":1,"next":next}),
                None
            )
            .json()["published"],
        true
    );
    let large = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert!(large["root"].is_null());
    assert_eq!(large["catalog"].as_array().unwrap().len(), 1);
    assert!(worker.usage_json()["manifest_bytes"].as_u64().unwrap() > 8192);
    reports.push(json!({"large":large}));
    let large_worker = worker.clone();
    // A valid >256-key public CAS/deletion remains eager and productive.
    let worker = Worker::new("scalar257");
    worker.register(true);
    let data = [2u8; 32];
    assert_eq!(
        worker.upload_retire(0, &[], &[], &data).json()["published"],
        true
    );
    let keys: Vec<_> = (0..257).map(|i| format!("chunks/{i:064x}")).collect();
    let next = manifest(2, &[], &keys);
    assert_eq!(
        worker
            .json(
                "POST",
                "/_services/manifest",
                &json!({"expected":1,"next":next}),
                None
            )
            .json()["published"],
        true
    );
    for key in &keys {
        assert_eq!(
            worker
                .request("DELETE", &format!("/_services/blob/{key}"), &[], None)
                .status,
            204
        );
    }
    worker.gc(2, &[]).json();
    assert_eq!(worker.publish(2, &[], None).json()["published"], true);
    reports.push(json!({"scalar257":worker.json("GET","/_services/test/inline-state",&Value::Null,None).json()}));
    for kind in [
        "future-ack",
        "unsafe-peak",
        "duplicate",
        "bad-size",
        "bad-magic",
    ] {
        let worker = Worker::new(kind);
        worker.register(true);
        let mut live = Vec::new();
        for g in 0..2u64 {
            let data = vec![g as u8 + 1; 32];
            let key = format!("chunks/{}", hash(&data));
            assert_eq!(
                worker.upload_retire(g, &[], &live, &data).json()["published"],
                true
            );
            live.push((key, 32));
        }
        if kind != "unsafe-peak" && kind != "bad-magic" {
            worker
                .json(
                    "POST",
                    "/_services/test/inline-corrupt",
                    &json!({"kind":"valid-high-peak","peak":500}),
                    None,
                )
                .json();
        }
        let corrupt = worker
            .json(
                "POST",
                "/_services/test/inline-corrupt",
                &json!({"kind":kind}),
                None,
            )
            .json();
        assert_eq!(corrupt["certain"], false);
        assert_eq!(
            worker
                .json("GET", "/_services/manifest", &Value::Null, None)
                .json()["generation"],
            2
        );
        assert_eq!(
            worker
                .request(
                    "PUT",
                    &format!("/_services/blob/chunks/{}", hash(&[9; 32])),
                    &[9; 32],
                    None
                )
                .status,
            409
        );
        worker
            .gc(
                2,
                &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            )
            .json();
        let state = worker
            .json("GET", "/_services/test/inline-state", &Value::Null, None)
            .json();
        assert_eq!(state["certain"], true);
        assert_eq!(state["root"]["entries"].as_array().unwrap().len(), 2);
        if kind != "unsafe-peak" && kind != "bad-magic" {
            assert_eq!(state["root"]["peak"], 500);
        }
        reports.push(json!({"kind":kind,"repaired":state}));
    }
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    let reopened = large_worker.usage_json();
    assert!(reopened["manifest_bytes"].as_u64().unwrap() > 8192);
    reports.push(json!({"large_reopened":reopened}));
    runtime.stop();
    fs::write(
        root.join("fallback.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}

#[test]
#[ignore = "owns8834; future normalized v2 ACK with empty debt fails before all promotion/R2 writes"]
fn actual_inline_metadata_future_ack_precondition() {
    let root = artifacts("future-ack");
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("future-ack");
    worker.register(true);
    let mut live = Vec::new();
    for g in 0..2u64 {
        let data = vec![g as u8 + 1; 32];
        let key = format!("chunks/{}", hash(&data));
        assert_eq!(
            worker.upload_retire(g, &[], &live, &data).json()["published"],
            true
        );
        live.push((key, 32));
    }
    worker
        .json(
            "POST",
            "/_services/test/inline-normalized-future",
            &json!({}),
            None,
        )
        .json();
    let before = worker.usage_json();
    let failed = worker.upload_retire(2, &[], &live, &[9; 32]);
    let after = worker.usage_json();
    let state = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    fs::write(root.join("attempt.json"),serde_json::to_vec_pretty(&json!({"before":before,"after":after,"state":state,"status":failed.status,"body":String::from_utf8_lossy(&failed.body)})).unwrap()).unwrap();
    assert_eq!(failed.status, 409);
    let delta = serde_json::from_value::<ServiceUsage>(after)
        .unwrap()
        .checked_workload_delta(&serde_json::from_value(before).unwrap())
        .unwrap();
    assert_eq!(delta.r2_class_a + delta.r2_class_b + delta.r2_deletes, 0);
    assert_eq!(delta.do_rows_written, Some(0));
    assert!(state["root"].is_null());
    assert_eq!(state["manifest"]["generation"], 2);
    assert_eq!(state["certain"], false);
    worker
        .gc(
            2,
            &live.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
        )
        .json();
    assert_eq!(
        worker.upload_retire(2, &[], &live, &[9; 32]).json()["published"],
        true
    );
    let repaired = worker
        .json("GET", "/_services/test/inline-state", &Value::Null, None)
        .json();
    assert_eq!(repaired["certain"], true);
    assert_eq!(repaired["root"]["entries"].as_array().unwrap().len(), 3);
    assert_eq!(repaired["root"]["peak"], 96);
    runtime.stop();
    runtime = Runtime::start(&root, 2);
    assert_eq!(
        worker
            .json("GET", "/_services/manifest", &Value::Null, None)
            .json()["generation"],
        3
    );
    runtime.stop();
    fs::write(
        root.join("repaired.json"),
        serde_json::to_vec_pretty(&repaired).unwrap(),
    )
    .unwrap();
    println!("{}", root.display());
}
