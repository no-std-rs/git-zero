//! Rust owns the actual local Worker/R2/SQLite correctness runtimes on 8833.
use git_zero_host::{
    Repository,
    accounting::ServiceUsage,
    cloudflare::HttpServices,
    hash,
    http::Request,
    native::{NativeHost, command},
    storage::{BlobStore, DurableStateStore, Manifest, R2BlobStore, RetiringCas, StateStore},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Runtime {
    child: Child,
}
impl Runtime {
    fn start(root: &Path, phase: usize) -> Self {
        Self::start_config(root, phase, "catalog-pages")
    }
    fn start_config(root: &Path, phase: usize, name: &str) -> Self {
        let log = fs::File::create(root.join(format!("wrangler-{phase}.log"))).unwrap();
        let cli = "/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js";
        let config =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("cloudflare/test/{name}.toml"));
        let child = Command::new("setsid")
            .args(["node", cli, "dev", "--config"])
            .arg(config)
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                "8833",
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
        let mut runtime = Self { child };
        for _ in 0..200 {
            assert!(
                runtime.child.try_wait().unwrap().is_none(),
                "Wrangler exited: {}",
                root.display()
            );
            if fs::read_to_string(root.join(format!("wrangler-{phase}.log")))
                .unwrap()
                .contains("Ready on http://127.0.0.1:8833")
            {
                return runtime;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Wrangler not ready: {}", root.display());
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
    let reservation = std::net::TcpListener::bind("127.0.0.1:8833")
        .expect("8833 must be free; never stop another owner's runtime");
    let root = std::env::var("GIT_ZERO_CATALOG_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("git-zero-catalog-{label}-{}", std::process::id()))
        });
    fs::create_dir_all(&root).unwrap();
    assert!(
        !root.join("persist").exists(),
        "requires fresh artifact/persist directory"
    );
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fingerprints: BTreeMap<_, _> = [
        "cloudflare/src/index.js",
        "cloudflare/src/catalog-pages.js",
        "src/cloudflare.rs",
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
struct Worker {
    repo: String,
    services: Arc<HttpServices>,
}
impl Worker {
    fn new(label: &str) -> Self {
        Self::named(format!(
            "pages-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn named(repo: String) -> Self {
        let services = Arc::new(
            HttpServices::new("http://127.0.0.1:8833", &repo, "git-zero-local-services").unwrap(),
        );
        Self { repo, services }
    }
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        headers: &[(&str, &str)],
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
        for (key, value) in [
            ("x-hosting-token", "git-zero-local-services"),
            ("x-hosting-repo", self.repo.as_str()),
            ("x-hosting-phase", "push"),
        ]
        .into_iter()
        .chain(headers.iter().copied())
        {
            process.arg("--header").arg(format!("{key}: {value}"));
        }
        if !body.is_empty() {
            process.args(["--data-binary", "@-"]);
        }
        let mut child = process
            .arg(format!("http://127.0.0.1:8833{route}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(body).unwrap();
        let mut output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let split = output.stdout.len() - 4;
        let status = std::str::from_utf8(&output.stdout[split + 1..])
            .unwrap()
            .parse()
            .unwrap();
        output.stdout.truncate(split);
        (status, output.stdout)
    }
    fn json(&self, method: &str, route: &str, value: &Value) -> Value {
        let data = if method == "GET" {
            Vec::new()
        } else {
            serde_json::to_vec(value).unwrap()
        };
        let (status, body) = self.request(method, route, &data, &[]);
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }
    fn state(&self) -> Value {
        self.json("GET", "/_services/test/pages", &Value::Null)
    }
    fn register(&self, mode: usize) -> Value {
        self.json("POST", "/_services/register", &json!({"codec_origin":"http://127.0.0.1:1", "manifest_retire":1,"manifest_upload_retire":1,"catalog_peak":usize::from(mode >= 1),"catalog_pages":usize::from(mode >= 2)}))
    }
    fn blobs(&self) -> R2BlobStore<HttpServices> {
        R2BlobStore::new(self.services.clone())
    }
    fn put(&self, data: &[u8]) -> String {
        let key = format!("chunks/{}", hash(data));
        self.blobs().put(&key, data).unwrap();
        key
    }
    fn usage_json(&self) -> Value {
        self.services.usage().unwrap()
    }
    fn usage(&self) -> ServiceUsage {
        serde_json::from_value(self.usage_json()).unwrap()
    }
    fn fault(&self, mode: &str) {
        self.json("POST", "/_services/test/fault", &json!({"mode":mode}));
    }
    fn page_fault(&self, mode: &str) {
        self.json("POST", "/_services/test/page-fault", &json!({"mode":mode}));
    }
    fn gc(&self, generation: u64, keys: &[String]) -> Value {
        self.json(
            "POST",
            "/_services/gc",
            &json!({"expected_generation":generation,"live_keys":keys}),
        )
    }
    fn seed(&self, rows: Vec<Value>) {
        self.json(
            "POST",
            "/_services/test/seed",
            &json!({"reset":true,"rows":rows}),
        );
    }
    fn metadata_put(&self, digest: &str, size: u64) -> (u16, Vec<u8>) {
        self.request(
            "POST",
            "/_services/test/metadata-put",
            &serde_json::to_vec(&json!({"key":format!("chunks/{digest}"),"size":size})).unwrap(),
            &[],
        )
    }
    fn lookup(&self, digest: &str) -> (u16, Vec<u8>) {
        self.request(
            "POST",
            "/_services/test/lookup",
            &serde_json::to_vec(&json!({"key":format!("chunks/{digest}")})).unwrap(),
            &[],
        )
    }
    fn empty_pages(label: &str) -> Self {
        let worker = Self::new(label);
        worker.register(2);
        worker
            .blobs()
            .delete(&format!("chunks/{}", hash(b"absent")))
            .unwrap();
        worker
    }
}
fn schema(state: &Value, name: &str) -> bool {
    state["schema"]
        .as_array()
        .unwrap()
        .iter()
        .any(|column| column["name"] == name)
}
fn encoded_page(prefix: &str, entries: &[(String, u64)], peak: u64) -> Value {
    let mut bytes = vec![0x47, 0x5a, 0x43, 0x50, 1, entries.len() as u8];
    let mut size = 0;
    for (digest, length) in entries {
        for offset in (0..64).step_by(2) {
            bytes.push(u8::from_str_radix(&digest[offset..offset + 2], 16).unwrap());
        }
        bytes.extend(length.to_le_bytes());
        size += length;
    }
    json!({"key":format!("pages/{prefix}"),"size":size,"peak_bytes":peak,"entries":bytes})
}
fn entries(state: &Value) -> usize {
    state["catalog"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["decoded"].as_array().map_or(1, Vec::len))
        .sum()
}
fn retirement_body(keys: &[String], survivors: &[(String, usize)]) -> Value {
    let packs: Vec<_> = survivors.iter().enumerate().map(|(i, (key, size))| json!({"id":format!("survivor-{i}"),"pack":[],"index":[],"bundle":[{"key":key,"len":size}],"pack_bytes":size,"index_bytes":0,"level":0})).collect();
    json!({"version":1,"expected":0,"next":{"generation":1,"refs":{},"packs":packs,"garbage":keys}})
}

#[test]
#[ignore = "owns 8833; actual routing, mutation, capability and eager retirement contracts"]
fn actual_catalog_pages_routes_and_mutations() {
    let root = artifacts("mutations");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let gate = Worker::new("gate");
    gate.register(0);
    let data = b"forged page capability";
    let key = format!("chunks/{}", hash(data));
    assert_eq!(
        gate.request(
            "PUT",
            &format!("/_services/blob/{key}"),
            data,
            &[
                ("x-git-zero-internal-catalog-pages", "1"),
                ("x-git-zero-internal-catalog-peak", "1")
            ]
        )
        .0,
        204
    );
    assert!(!schema(&gate.state(), "entries"));
    let ack = gate.json(
        "POST",
        "/_services/register",
        &json!({"codec_origin":"http://127.0.0.1:1","catalog_pages":1}),
    );
    assert!(ack.get("catalog_pages").is_none());
    gate.register(2);
    let before = gate.usage();
    assert_eq!(
        gate.request("GET", &format!("/_services/blob/{key}"), &[], &[])
            .0,
        200
    );
    assert_eq!(
        gate.request("PUT", &format!("/_services/blob/{key}"), b"wrong", &[])
            .0,
        409
    );
    assert_eq!(
        gate.request(
            "POST",
            "/_services/gc",
            br#"{"expected_generation":1,"live_keys":[]}"#,
            &[]
        )
        .0,
        409
    );
    assert_eq!(gate.request("POST", "/_services/manifest/retire", &serde_json::to_vec(&json!({"version":1,"expected":1,"next":{"generation":2,"refs":{},"packs":[],"garbage":[key]}})).unwrap(), &[]).0, 200);
    assert!(!schema(&gate.state(), "entries"));
    assert_eq!(
        gate.usage()
            .checked_workload_delta(&before)
            .unwrap()
            .do_rows_written,
        Some(0)
    );
    gate.put(b"new mixed member");
    let mixed = gate.state();
    assert_eq!(mixed["catalog"].as_array().unwrap().len(), 2);
    assert_eq!(entries(&mixed), 2);
    let before = gate.usage();
    gate.put(data);
    let after = gate.usage();
    assert_eq!(after.r2_live_bytes, before.r2_live_bytes);
    assert_eq!(
        after
            .checked_workload_delta(&before)
            .unwrap()
            .do_rows_written,
        Some(1)
    );
    gate.services.unregister().unwrap();
    gate.put(b"registry lost still decodes pages");
    gate.register(0);
    gate.put(b"old Native still decodes pages");
    assert!(schema(&gate.state(), "entries"));
    reports.push(json!({"case":"capability-mixed-duplicate-old-client","state":gate.state(),"usage":gate.usage_json()}));

    let mut normal = Vec::new();
    for mode in [1, 2] {
        let worker = Worker::new(&format!("fused-{mode}"));
        worker.services.register("http://127.0.0.1:1").unwrap();
        worker.register(mode);
        let repository = Repository::new(
            worker.blobs(),
            DurableStateStore::new(worker.services.clone()),
        );
        let mut previous = Manifest {
            generation: 1,
            ..Manifest::default()
        };
        for i in 0..3 {
            previous.packs.push(
                repository
                    .store_bundled_pack(
                        format!("old-{i}"),
                        format!("old-{i}").as_bytes(),
                        b"index",
                        0,
                    )
                    .unwrap(),
            );
        }
        assert!(
            repository
                .publish(&Manifest::default(), previous.clone())
                .unwrap()
        );
        let (pack, pending) = repository
            .stage_bundled_pack("new".into(), b"new-pack", b"new-index", 1)
            .unwrap()
            .into_parts();
        let mut next = Manifest {
            generation: 2,
            packs: vec![pack],
            ..Manifest::default()
        };
        next.retire_replaced(&previous);
        worker.fault("");
        let before = worker.usage();
        let RetiringCas::Published(receipt) = repository
            .publish_staged_retiring(&previous, next, Some(pending))
            .unwrap()
        else {
            panic!("not published")
        };
        assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
        let after = worker.usage();
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_class_b, 0);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.worker_service_requests, 1);
        assert_eq!(delta.do_requests, 1);
        assert_eq!(after.r2_peak_bytes, 47);
        assert_eq!(after.r2_live_bytes, 17);
        let state = worker.state();
        assert!(state["high_water"].as_array().unwrap().is_empty());
        let trace = state["trace"].as_array().unwrap();
        let put = trace.iter().position(|event| event == "r2:put").unwrap();
        let cas = trace
            .iter()
            .position(|event| {
                event
                    .as_str()
                    .is_some_and(|s| s.starts_with("INSERT INTO repository_state"))
            })
            .unwrap();
        let delete = trace.iter().position(|event| event == "r2:delete").unwrap();
        assert!(
            trace[put + 1..cas]
                .iter()
                .any(|event| event == "sync:durable")
        );
        assert!(
            trace[cas + 1..delete]
                .iter()
                .any(|event| event == "sync:durable")
        );
        if mode == 2 {
            let catalog_writes: Vec<_> = state["sql"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|cursor| {
                    cursor["statement"]
                        .as_str()
                        .unwrap()
                        .starts_with("INSERT INTO blob_catalog")
                })
                .collect();
            assert_eq!(
                catalog_writes.len(),
                2,
                "one upload and one survivor rewrite"
            );
            assert_eq!(entries(&state), 1);
        }
        normal.push(delta);
        reports.push(
            json!({"case":"normal-fused","mode":mode,"state":state,"usage":worker.usage_json()}),
        );
    }
    assert_eq!(
        normal[0].do_rows_written.unwrap(),
        normal[1].do_rows_written.unwrap() + 2
    );
    assert_eq!(normal[0].r2_written_bytes, normal[1].r2_written_bytes);

    let shared = Worker::new("shared-live");
    shared.register(2);
    let shared_keys: Vec<_> = (0..3)
        .map(|i| shared.put(format!("shared {i}").as_bytes()))
        .collect();
    let before = shared.usage();
    let response = shared.json(
        "POST",
        "/_services/manifest/retire",
        &retirement_body(&shared_keys, &[(shared_keys[0].clone(), 8)]),
    );
    assert_eq!(
        response["retirement"]["outcomes"].as_array().unwrap().len(),
        2
    );
    let delta = shared.usage().checked_workload_delta(&before).unwrap();
    assert_eq!(delta.r2_deletes, 2);
    assert_eq!(delta.r2_class_b, 0);
    assert_eq!(delta.do_rows_written, Some(2));
    assert_eq!(shared.blobs().get(&shared_keys[0]).unwrap(), b"shared 0");
    reports.push(json!({"case":"shared-live-excluded","state":shared.state(),"response":response}));

    let deep = Worker::empty_pages("deep-split");
    let digests: Vec<_> = (0..33)
        .map(|i| (format!("{}{i:02x}", "0".repeat(62)), 1))
        .collect();
    let before = deep.usage();
    deep.seed(vec![encoded_page("", &digests[..32], 33)]);
    assert_eq!(deep.metadata_put(&digests[32].0, 1).0, 200);
    let state = deep.state();
    assert_eq!(state["catalog"].as_array().unwrap().len(), 3);
    assert_eq!(entries(&state), 33);
    assert!(
        state["catalog"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["key"].as_str().unwrap().len() == 6 + 63)
    );
    assert_eq!(
        deep.usage()
            .checked_workload_delta(&before)
            .unwrap()
            .r2_class_a,
        0,
        "synthetic routing exercises SQL only"
    );
    for (digest, _) in &digests {
        assert_eq!(deep.lookup(digest).0, 200);
    }
    reports.push(json!({"case":"adversarial-33-split","state":state}));
    let rollback = Worker::empty_pages("split-rollback");
    rollback.seed(vec![encoded_page("", &digests[..32], 33)]);
    rollback.page_fault("transaction");
    assert_eq!(rollback.metadata_put(&digests[32].0, 1).0, 500);
    let state = rollback.state();
    assert_eq!(state["certain"], false);
    assert_eq!(state["catalog"].as_array().unwrap().len(), 1);
    assert_eq!(entries(&state), 32);
    rollback.gc(0, &[]);
    assert!(rollback.state()["catalog"].as_array().unwrap().is_empty());
    reports.push(json!({"case":"split-transaction-rollback","failed_state":state,"reconciled":rollback.state()}));
    let fanout = Worker::empty_pages("fanout");
    let digests: Vec<_> = (0..33)
        .map(|i| (format!("{:x}{:063x}", i % 16, i / 16), 1))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    fanout.seed(vec![encoded_page("", &digests[..32], 33)]);
    assert_eq!(fanout.metadata_put(&digests[32].0, 1).0, 200);
    assert_eq!(fanout.state()["catalog"].as_array().unwrap().len(), 16);
    reports.push(json!({"case":"sixteen-child-split","state":fanout.state()}));
    let gaps = Worker::empty_pages("gaps");
    gaps.seed(vec![
        encoded_page("2", &[(format!("2{:063x}", 0), 1)], 10),
        encoded_page("8", &[(format!("8{:063x}", 0), 1)], 10),
    ]);
    for digit in ["0", "5", "f"] {
        assert_eq!(
            gaps.metadata_put(&format!("{digit}{}", "0".repeat(63)), 1)
                .0,
            200
        );
    }
    assert_eq!(gaps.state()["catalog"].as_array().unwrap().len(), 5);
    reports.push(json!({"case":"indexed-gaps","state":gaps.state()}));
    for corrupt in [
        "duplicate",
        "overlap",
        "encoding",
        "version",
        "count",
        "length",
        "unsafe-size",
        "aggregate",
        "duplicate-digest",
        "route-prefix",
    ] {
        let worker = Worker::empty_pages(corrupt);
        let key = worker.put(b"real valid hash separate from synthetic routing");
        let digest = key[7..].to_owned();
        let mut rows = vec![encoded_page("", &[(digest.clone(), 47)], 47)];
        match corrupt {
            "duplicate" => rows.push(json!({"key":key,"size":47,"peak_bytes":47,"entries":null})),
            "overlap" => rows.push(encoded_page(&digest[..1], &[(digest.clone(), 47)], 47)),
            "version" => rows[0]["entries"][4] = json!(2),
            "count" => rows[0]["entries"][5] = json!(0),
            "length" => rows[0]["entries"]
                .as_array_mut()
                .unwrap()
                .pop()
                .map(|_| ())
                .unwrap(),
            "unsafe-size" => {
                rows[0]["entries"].as_array_mut().unwrap()[38..46].fill(json!(255));
            }
            "aggregate" => rows[0]["size"] = json!(48),
            "duplicate-digest" => {
                rows[0] = encoded_page("", &[(digest.clone(), 47), (digest.clone(), 47)], 94)
            }
            "route-prefix" => {
                let wrong = if digest.starts_with('0') { "1" } else { "0" };
                rows[0] = encoded_page(
                    &digest[..1],
                    &[(format!("{wrong}{}", &digest[1..]), 47)],
                    47,
                );
            }
            _ => rows[0]["entries"][0] = json!(0),
        }
        worker.seed(rows);
        assert_eq!(worker.lookup(&digest).0, 500);
        assert_eq!(worker.state()["certain"], false);
        worker.gc(0, std::slice::from_ref(&key));
        assert_eq!(worker.state()["certain"], true);
        assert_eq!(entries(&worker.state()), 1);
        worker.json(
            "POST",
            "/_services/test/orphan",
            &json!({"key":"pages/","bytes":[1,2,3]}),
        );
        worker.gc(0, std::slice::from_ref(&key));
        assert_eq!(entries(&worker.state()), 1);
        assert_eq!(
            worker.blobs().get(&key).unwrap(),
            b"real valid hash separate from synthetic routing"
        );
        reports.push(json!({"case":corrupt,"reconciled":worker.state()}));
    }
    let churn = Worker::empty_pages("churn");
    for cycle in 0..3 {
        let keys: Vec<_> = (0..40)
            .map(|i| churn.put(format!("churn {cycle} {i}").as_bytes()))
            .collect();
        assert_eq!(churn.put(format!("churn {cycle} 0").as_bytes()), keys[0]);
        assert_eq!(entries(&churn.state()), 40);
        assert!(churn.state()["catalog"].as_array().unwrap().len() <= keys.len());
        for key in keys {
            churn.blobs().delete(&key).unwrap();
        }
        assert!(churn.state()["catalog"].as_array().unwrap().is_empty());
    }
    let empty = churn.put(b"");
    churn.blobs().delete(&empty).unwrap();
    reports.push(json!({"case":"churn-zero-empty-payload","state":churn.state()}));
    runtime.stop();
    fs::write(
        root.join("mutations.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("mutation artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8833; actual partial R2, SQL/barrier faults, union witnesses, orphan GC and process restart"]
fn actual_catalog_pages_faults_and_restart() {
    let root = artifacts("faults");
    let mut runtime = Runtime::start(&root, 1);
    let mut reports = Vec::new();
    let mut reopen = Vec::new();
    let corrupt = Worker::new("corrupt-length-reopen");
    corrupt.register(2);
    let survivor = corrupt.put(&[1; 100]);
    let mut rows = corrupt.state()["catalog"].as_array().unwrap().clone();
    rows[0]["entries"].as_array_mut().unwrap().pop();
    corrupt.seed(rows);
    assert_eq!(corrupt.state()["certain"], false);
    reopen.push((corrupt.repo, survivor, 100, 0));
    for mode in ["entries-before", "entries-after"] {
        let worker = Worker::new(mode);
        worker.register(2);
        worker.page_fault(mode);
        assert!(
            worker
                .blobs()
                .put(
                    &format!("chunks/{}", hash(b"schema failure")),
                    b"schema failure"
                )
                .is_err()
        );
        assert_eq!(worker.state()["certain"], false);
        assert_eq!(
            worker
                .usage()
                .operations
                .get("r2_put")
                .map_or(0, |op| op.attempts),
            0
        );
        // Rediscovery repairs an ALTER whose reply was lost, including after
        // the registration no longer advertises either new capability.
        if mode == "entries-after" {
            worker.services.unregister().unwrap();
            worker.register(0);
        }
        worker.gc(0, &[]);
        worker.put(b"after schema recovery");
        assert!(schema(&worker.state(), "entries"));
        reports.push(json!({"case":mode,"state":worker.state(),"usage":worker.usage_json()}));
    }
    for mode in [
        "delete-second",
        "retire-catalog",
        "retire-transaction",
        "retire-sync",
        "lost",
    ] {
        let worker = Worker::new(mode);
        worker.register(2);
        let retired: Vec<_> = (0..3)
            .map(|i| worker.put(format!("retire {mode} {i}").as_bytes()))
            .collect();
        let survivor_data = b"survives ambiguous retirement";
        let survivor = worker.put(survivor_data);
        let before = worker.usage();
        worker.fault(if mode == "lost" { "lost" } else { "" });
        if mode != "lost" {
            worker.page_fault(mode);
        }
        let (status, body) = worker.request(
            "POST",
            "/_services/manifest/retire",
            &serde_json::to_vec(&retirement_body(
                &retired,
                &[(survivor.clone(), survivor_data.len())],
            ))
            .unwrap(),
            &[],
        );
        if mode == "delete-second" {
            assert_eq!(status, 200);
            let receipt: Value = serde_json::from_slice(&body).unwrap();
            let outcomes = receipt["retirement"]["outcomes"].as_array().unwrap();
            assert_eq!(
                outcomes
                    .iter()
                    .map(|outcome| outcome["deleted"].as_bool().unwrap())
                    .collect::<Vec<_>>(),
                [true, false, true]
            );
            assert_eq!(entries(&worker.state()), 2);
        } else {
            assert!(status >= 500, "{mode}: {status}");
        }
        let state = worker.state();
        if mode != "lost" {
            assert_eq!(state["certain"], false);
        }
        let after = worker.usage();
        assert_eq!(
            after.operations["manifest_cas"].successes
                - before
                    .operations
                    .get("manifest_cas")
                    .map_or(0, |op| op.successes),
            1
        );
        assert_eq!(
            after.operations["r2_delete"].attempts
                - before
                    .operations
                    .get("r2_delete")
                    .map_or(0, |op| op.attempts),
            3
        );
        assert_eq!(
            after.operations["r2_delete"].failures
                - before
                    .operations
                    .get("r2_delete")
                    .map_or(0, |op| op.failures),
            u64::from(mode == "delete-second")
        );
        if mode == "retire-catalog" {
            assert_eq!(after.calls.do_rows_written, None);
        }
        if mode != "lost" {
            assert!(
                worker
                    .blobs()
                    .put(&format!("chunks/{}", hash(b"blocked")), b"blocked")
                    .is_err()
            );
        }
        reports.push(json!({"case":mode,"status":status,"response":String::from_utf8_lossy(&body),"state":state,"usage":worker.usage_json()}));
        reopen.push((worker.repo, survivor, after.r2_peak_bytes, 1));
    }
    for mode in ["catalog-put", "sync", "catalog-after"] {
        let worker = Worker::new(mode);
        worker.register(2);
        let survivor = worker.put(&[1; 100]);
        if mode == "catalog-after" {
            worker.page_fault(mode);
        } else {
            worker.fault(mode);
        }
        let key = format!("chunks/{}", hash(&[2; 70]));
        assert!(worker.blobs().put(&key, &[2; 70]).is_err());
        assert_eq!(worker.state()["certain"], false);
        reports.push(json!({"case":mode,"state":worker.state(),"usage":worker.usage_json()}));
        reopen.push((worker.repo, survivor, 170, 0));
    }
    for mixed in [false, true] {
        for partial in [false, true] {
            let worker = Worker::new(&format!("union-{mixed}-{partial}"));
            worker.register(if mixed { 1 } else { 2 });
            let mut members = Vec::new();
            if mixed {
                let a = worker.put(b"legacy carrier");
                worker.register(2);
                let b = worker.put(b"page carrier");
                members.extend([a, b]);
            } else {
                let mut buckets: [Vec<(String, Vec<u8>)>; 2] = [Vec::new(), Vec::new()];
                for i in 0..10000 {
                    let data = format!("full page union fixture {i}").into_bytes();
                    let digest = hash(&data);
                    if let Some(index) = match digest.as_bytes()[0] {
                        b'0' => Some(0),
                        b'1' => Some(1),
                        _ => None,
                    } {
                        if buckets[index].len() < 32 {
                            buckets[index].push((format!("chunks/{digest}"), data));
                        }
                    }
                    if buckets.iter().all(|bucket| bucket.len() == 32) {
                        break;
                    }
                }
                for bucket in &mut buckets {
                    bucket.sort_by(|left, right| left.0.cmp(&right.0));
                    for (key, data) in bucket {
                        assert_eq!(worker.put(data), *key);
                        members.push(key.clone());
                    }
                }
            }
            let peak = worker.usage().r2_peak_bytes;
            let mut rows = worker.state()["catalog"].as_array().unwrap().clone();
            assert_eq!(rows.len(), 2);
            for row in &mut rows {
                row["peak_bytes"] = json!(peak);
            }
            worker.seed(rows);
            assert!(worker.state()["high_water"].as_array().unwrap().is_empty());
            worker.fault("");
            worker.page_fault(if partial { "delete-second" } else { "" });
            let response = worker.json(
                "POST",
                "/_services/manifest/retire",
                &retirement_body(&members, &[]),
            );
            let outcomes = response["retirement"]["outcomes"].as_array().unwrap();
            assert_eq!(outcomes.len(), members.len());
            for (i, (outcome, key)) in outcomes.iter().zip(&members).enumerate() {
                assert_eq!(&outcome["key"], key);
                assert_eq!(outcome["deleted"], !(partial && i == 1));
            }
            let state = worker.state();
            assert_eq!(state["high_water"][0]["bytes"], peak);
            let trace = state["page_fault"]["trace"].as_array().unwrap();
            let fallback = trace
                .iter()
                .position(|event| event == "fallback:write")
                .unwrap();
            let delete = trace.iter().position(|event| event == "r2:delete").unwrap();
            assert!(
                trace[fallback + 1..delete]
                    .iter()
                    .any(|event| event == "sync:durable")
            );
            assert_eq!(
                state["catalog"].as_array().unwrap().len(),
                usize::from(partial)
            );
            reports.push(json!({"case":"union-witness","mixed":mixed,"partial":partial,"state":state,"usage":worker.usage_json()}));
            reopen.push((worker.repo, String::new(), peak, 1));
        }
    }
    for mode in ["gc-reset", "sync"] {
        let worker = Worker::new(&format!("gc-{mode}"));
        worker.register(2);
        let a = worker.put(&[1; 100]);
        worker.put(&[2; 70]);
        worker.fault(mode);
        assert_eq!(
            worker
                .request(
                    "POST",
                    "/_services/gc",
                    &serde_json::to_vec(&json!({"expected_generation":0,"live_keys":[a]})).unwrap(),
                    &[]
                )
                .0,
            500
        );
        let state = worker.state();
        assert_eq!(state["high_water"][0]["bytes"], 170);
        if mode == "sync" {
            assert!(
                !state["trace"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event == "r2:delete")
            );
        }
        reports
            .push(json!({"case":format!("gc-{mode}"),"state":state,"usage":worker.usage_json()}));
        reopen.push((worker.repo, a, 170, 0));
    }
    runtime.stop();
    let mut runtime = Runtime::start(&root, 2);
    for (repo, survivor, peak, generation) in &reopen {
        let worker = Worker::named(repo.clone());
        let initial = worker.state();
        assert!(schema(&initial, "entries"));
        // A direct client explicitly reconciles after restart. SQL-only reload
        // cannot reconstruct an unrecorded R2 PUT, just as in R11.
        worker.gc(
            *generation,
            &if survivor.is_empty() {
                vec![]
            } else {
                vec![survivor.clone()]
            },
        );
        let state = worker.state();
        let usage = worker.usage();
        assert_eq!(usage.r2_peak_bytes, *peak);
        assert_eq!(state["certain"], true);
        assert_eq!(usage.r2_live_objects, u64::from(!survivor.is_empty()));
        assert_eq!(state["high_water"][0]["bytes"], *peak);
        worker.register(0);
        let old_client_key = worker.put(b"");
        worker.blobs().delete(&old_client_key).unwrap();
        assert!(
            schema(&worker.state(), "entries"),
            "older Native on reopened durable schema still uses pages"
        );
        reports.push(json!({"case":"reopen-authoritative-reconcile","repo":repo,"initial":initial,"state":state,"usage":worker.usage_json()}));
    }
    runtime.stop();
    let mut runtime = Runtime::start(&root, 3);
    for (repo, survivor, peak, _) in &reopen {
        let worker = Worker::named(repo.clone());
        assert_eq!(worker.usage().r2_peak_bytes, *peak);
        assert_eq!(entries(&worker.state()), usize::from(!survivor.is_empty()));
    }
    runtime.stop();
    fs::write(
        root.join("faults-restart.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("fault/restart artifacts: {}", root.display());
}

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(root).args(args);
    command(process, input).unwrap().stdout
}

#[test]
#[ignore = "owns 8833; compare actual SQLite cursor reads of bounded equivalent point lookup forms"]
fn actual_catalog_page_lookup_cursor_forms() {
    let root = artifacts("lookup");
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("lookup-forms");
    worker.register(1);
    let legacy = worker.put(b"legacy lookup");
    worker.register(2);
    let page = worker.put(b"page lookup");
    let mut report = Vec::new();
    for key in [legacy, page, format!("chunks/{}", hash(b"absent lookup"))] {
        let forms = worker.json("POST", "/_services/test/probe-forms", &json!({"key":key}));
        let values = forms.as_array().unwrap();
        let points = values.last().unwrap();
        assert!(points["rowsRead"].as_u64().unwrap() < forms[0]["rowsRead"].as_u64().unwrap());
        for form in values.iter().filter(|form| form.get("error").is_none()) {
            assert_eq!(form["keys"], points["keys"]);
            if form["form"] != "in" {
                assert_eq!(form["rowsRead"], points["rowsRead"]);
            }
        }
        report.push(json!({"key":key,"forms":forms}));
    }
    runtime.stop();
    fs::write(
        root.join("lookup-forms.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("lookup artifacts: {}", root.display());
}
fn git_text(root: &Path, args: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(root, args, input))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
#[ignore = "owns 8833; genuine new Native registration, first timed PUT DDL, partial-debt recovery and Git fsck"]
fn actual_catalog_pages_native_partial_recovery() {
    native_recovery("delete-second");
}

#[test]
#[ignore = "owns 8833; same Native next push recovers after a lost successful retirement-sync response"]
fn actual_catalog_pages_native_ambiguous_retirement_recovery() {
    native_recovery("retire-sync");
}

fn native_recovery(fault: &str) {
    let ambiguous = fault == "retire-sync";
    let root = artifacts(&format!("native-{fault}"));
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("native");
    let source = root.join("source.git");
    fs::create_dir_all(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    git(&source, &["config", "user.name", "Catalog contract"], &[]);
    git(
        &source,
        &["config", "user.email", "contract@example.invalid"],
        &[],
    );
    let native_root = root.join("native");
    let mut host = NativeHost::open_cloudflare(
        native_root.clone(),
        "http://127.0.0.1:8833",
        &worker.repo,
        "git-zero-local-services",
        false,
    )
    .unwrap();
    assert!(!schema(&worker.state(), "entries"));
    host.cloudflare_services()
        .unwrap()
        .register("http://127.0.0.1:1")
        .unwrap();
    assert!(
        !schema(&worker.state(), "entries"),
        "Native registration must not migrate"
    );
    let mut previous = "0".repeat(40);
    let mut reports = Vec::new();
    let mut partial = None;
    for generation in 1..=5 {
        let data = format!("catalog native generation {generation}\n").repeat(512);
        let blob = git_text(&source, &["hash-object", "-w", "--stdin"], data.as_bytes());
        let tree = git_text(
            &source,
            &["mktree"],
            format!("100644 blob {blob}\tdata.txt\n").as_bytes(),
        );
        let mut args = vec!["commit-tree", tree.as_str()];
        if generation > 1 {
            args.extend(["-p", &previous]);
        }
        let tip = git_text(&source, &args, b"catalog native fixture\n");
        let revisions = if generation > 1 {
            format!("{tip}\n^{previous}\n")
        } else {
            format!("{tip}\n")
        };
        let pack = git(
            &source,
            &["pack-objects", "--stdout", "--revs"],
            revisions.as_bytes(),
        );
        let line = format!("{previous} {tip} refs/heads/main\0report-status atomic\n");
        let mut body = format!("{:04x}{line}", line.len() + 4).into_bytes();
        body.extend_from_slice(b"0000");
        body.extend(pack);
        if generation == 4 {
            worker.page_fault(fault);
        }
        let before = if generation == 5 && ambiguous {
            None
        } else {
            Some(worker.usage())
        };
        let response = host.handle(Request {
            method: "POST".into(),
            target: "/repo.git/git-receive-pack".into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-git-receive-pack-request".into(),
            )]),
            body,
        });
        if generation == 4 && ambiguous {
            fs::write(root.join("push-4-error.body"), &response.body).unwrap();
            fs::write(root.join("push-4-error.json"),serde_json::to_vec_pretty(&json!({"status":response.status,"fault":fault,"body":String::from_utf8_lossy(&response.body)})).unwrap()).unwrap();
            assert!(
                response.status >= 500,
                "fourth push must retain its ambiguous failure"
            );
            partial = before;
            previous = tip;
            // No test-side service request, inventory, GC or restart occurs
            // between this error and the fifth push on this same NativeHost.
            continue;
        }
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        assert!(String::from_utf8_lossy(&response.body).contains("ok refs/heads/main"));
        let usage = worker.usage();
        let delta = usage
            .checked_workload_delta(before.as_ref().or(partial.as_ref()).unwrap())
            .unwrap();
        let manifest = DurableStateStore::new(worker.services.clone())
            .load()
            .unwrap();
        assert_eq!(manifest.generation, generation);
        assert_eq!(manifest.refs["refs/heads/main"], tip);
        if generation == 1 {
            assert_eq!(
                delta.do_rows_written,
                Some(4),
                "two actual DDL rows + PUT page + CAS in first Native push"
            );
            assert!(schema(&worker.state(), "entries"));
        }
        if generation == 4 {
            assert_eq!(usage.operations["r2_delete"].attempts, 3);
            assert_eq!(usage.operations["r2_delete"].successes, 2);
            assert_eq!(usage.operations["r2_delete"].failures, 1);
            assert_eq!(worker.state()["certain"], false);
            partial = Some(usage.clone());
        }
        if generation == 5 {
            assert_eq!(
                usage
                    .checked_workload_delta(partial.as_ref().unwrap())
                    .unwrap()
                    .r2_class_a,
                if ambiguous { 3 } else { 2 },
                "authoritative GC LIST plus actual PUTs, without replaying the fourth upload"
            );
            assert_eq!(
                usage.operations["r2_delete"].failures,
                u64::from(!ambiguous)
            );
            if ambiguous {
                let previous = partial.as_ref().unwrap();
                assert_eq!(
                    usage.operations["manifest_cas"].successes
                        - previous.operations["manifest_cas"].successes,
                    2,
                    "committed fourth CAS is never republished"
                );
                assert_eq!(
                    usage.operations["r2_put"].attempts - previous.operations["r2_put"].attempts,
                    2
                );
            }
            assert!(manifest.garbage.is_empty());
            assert_eq!(worker.state()["certain"], true);
            git(
                &native_root.join("view/repo.git"),
                &["fsck", "--strict"],
                &[],
            );
            assert_eq!(
                git(
                    &native_root.join("view/repo.git"),
                    &["cat-file", "blob", &blob],
                    &[]
                ),
                data.as_bytes()
            );
        }
        reports.push(json!({"generation":generation,"delta":delta,"usage":worker.usage_json(),"state":worker.state()}));
        previous = tip;
    }
    drop(host);
    worker.services.unregister().unwrap();
    runtime.stop();
    fs::write(
        root.join("native.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("Native artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8833; existing explicit v1/v2 Rust controls on the common new Worker"]
fn actual_catalog_pages_preserve_existing_v1_v2_controls() {
    let root = artifacts("legacy-controls");
    let run = |test: &str, filter: &str| {
        let output = Command::new(env!("CARGO"))
            .args(["test", "--manifest-path"])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .args([
                "-p",
                "git-zero-host",
                "--test",
                test,
                filter,
                "-j2",
                "--target-dir",
                "/tmp/git-zero-catalog-pages-target",
                "--",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("GIT_ZERO_CLOUDFLARE_ORIGIN", "http://127.0.0.1:8833")
            .env("GIT_ZERO_CLOUDFLARE_TOKEN", "git-zero-local-services")
            .output()
            .unwrap();
        fs::write(root.join(format!("{filter}.stdout.log")), &output.stdout).unwrap();
        fs::write(root.join(format!("{filter}.stderr.log")), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{filter}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let mut runtime = Runtime::start(&root, 1);
    for filter in [
        "real_peak_capability_is_repo_scoped_and_migration_mutation_only",
        "real_peak_duplicate_and_last_or_alternate_witness_deletes",
        "real_peak_faults_block_ordinary_mutations_until_inventory_reconciliation",
    ] {
        run("peak_catalog", filter);
    }
    runtime.stop();
    let mut runtime = Runtime::start_config(&root, 2, "upload-fault");
    for filter in [
        "faulted_upload_barriers_catalog_partial_retirement_and_lost_receipt_preserve_authority",
        "real_upload_retirement_preserves_binding_sql_bytes_and_removes_one_callback",
    ] {
        run("cloudflare", filter);
    }
    runtime.stop();
    println!("legacy control artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8833; maximum existing 256-key mixed-mode retirement and witness SQL bound"]
fn actual_catalog_pages_maximum_retirement_bound() {
    let root = artifacts("maximum-debt");
    let mut runtime = Runtime::start(&root, 1);
    let worker = Worker::new("maximum-debt");
    worker.register(1);
    let keys: Vec<_> = (0..256)
        .map(|i| worker.put(format!("maximum bounded retirement {i}").as_bytes()))
        .collect();
    worker.register(2);
    worker.put(b"maximum bounded retirement 0");
    let peak = worker.usage().r2_peak_bytes;
    let mut rows = worker.state()["catalog"].as_array().unwrap().clone();
    for row in &mut rows {
        row["peak_bytes"] = json!(peak);
    }
    worker.seed(rows);
    worker.fault("");
    worker.page_fault("");
    let before = worker.usage();
    let (status, body) = worker.request(
        "POST",
        "/_services/manifest/retire",
        &serde_json::to_vec(&retirement_body(&keys, &[])).unwrap(),
        &[],
    );
    fs::write(root.join("response.json"), &body).unwrap();
    fs::write(
        root.join("state.json"),
        serde_json::to_vec_pretty(&worker.state()).unwrap(),
    )
    .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let response: Value = serde_json::from_slice(&body).unwrap();
    let outcomes = response["retirement"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 256);
    for (outcome, key) in outcomes.iter().zip(&keys) {
        assert_eq!(&outcome["key"], key);
        assert_eq!(outcome["deleted"], true);
    }
    let after = worker.usage();
    assert_eq!(
        after.checked_workload_delta(&before).unwrap().r2_deletes,
        256
    );
    assert_eq!(after.r2_live_objects, 0);
    assert_eq!(after.r2_peak_bytes, before.r2_peak_bytes);
    assert!(worker.state()["catalog"].as_array().unwrap().is_empty());
    let state = worker.state();
    let trace = state["page_fault"]["trace"].as_array().unwrap();
    let fallback = trace
        .iter()
        .position(|event| event == "fallback:write")
        .unwrap();
    let delete = trace.iter().position(|event| event == "r2:delete").unwrap();
    assert!(
        trace[fallback + 1..delete]
            .iter()
            .any(|event| event == "sync:durable")
    );
    fs::write(
        root.join("usage.json"),
        serde_json::to_vec_pretty(&worker.usage_json()).unwrap(),
    )
    .unwrap();
    runtime.stop();
    println!("maximum debt artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8833; malformed numeric SQL metadata, integer SUM overflow and authoritative reopen recovery"]
fn actual_catalog_pages_corrupt_numeric_reopen_gc() {
    let root = artifacts("numeric-recovery");
    let mut runtime = Runtime::start(&root, 1);
    let mut cases = Vec::new();
    let mut reports = Vec::new();
    let modes: Vec<_> = ["size", "peak_bytes", "fallback"]
        .into_iter()
        .flat_map(|field| {
            ["negative", "unsafe", "fractional"]
                .into_iter()
                .map(move |kind| (field, kind))
        })
        .chain([
            ("size", "sum-maxint"),
            ("size", "sum-safe"),
            ("peak_bytes", "lost-witness"),
            ("fallback", "empty-zero"),
        ])
        .collect();
    for (field, kind) in modes {
        let worker = Worker::new(&format!("numeric-{field}-{kind}"));
        worker.register(2);
        let mut live = Vec::new();
        let expected = if kind == "empty-zero" {
            worker
                .blobs()
                .delete(&format!("chunks/{}", hash(b"absent")))
                .unwrap();
            0
        } else {
            let a = worker.put(&[1; 100]);
            let old = worker.put(&[3; 400]);
            worker.blobs().delete(&old).unwrap();
            assert_eq!(
                worker.usage().r2_peak_bytes,
                500,
                "historical H comes from actual physical bytes"
            );
            live.push(a.clone());
            let mut rows = vec![encoded_page("", &[(a[7..].to_owned(), 100)], 500)];
            if kind != "lost-witness" {
                let legacy = worker.put(&[4; 100]);
                live.push(legacy.clone());
                rows.push(json!({"key":legacy,"size":100,"peak_bytes":500,"entries":null}));
            }
            if kind == "sum-maxint" {
                rows.push(json!({"key":format!("chunks/{}",hash(b"extra integer row")),"size":0,"peak_bytes":0,"entries":null}));
            }
            if kind == "sum-safe" {
                for i in 0..1025 {
                    rows.push(json!({"key":format!("chunks/{}",hash(format!("safe overflowing row {i}").as_bytes())),"size":9_007_199_254_740_991u64,"peak_bytes":0,"entries":null}));
                }
            }
            worker.seed(rows);
            worker.json(
                "POST",
                "/_services/test/orphan",
                &json!({"key":format!("chunks/{}",hash(&[2;70])),"bytes":vec![2;70]}),
            );
            if kind == "lost-witness" { 170 } else { 500 }
        };
        if kind != "sum-safe" {
            let mutation_kind = match kind {
                "sum-maxint" => "maxint",
                "lost-witness" => "unsafe",
                "empty-zero" => "negative",
                _ => kind,
            };
            worker.json("POST","/_services/test/corrupt-number",&json!({"field":field,"kind":mutation_kind,"key":"pages/","all":kind=="sum-maxint"}));
        }
        cases.push(json!({"repo":worker.repo,"field":field,"kind":kind,"live":live,"expected_floor":expected,"historical_peak_unknown":kind=="lost-witness"}));
    }
    runtime.stop();
    let mut runtime = Runtime::start(&root, 2);
    for case in &cases {
        let worker = Worker::named(case["repo"].as_str().unwrap().to_owned());
        let initial = worker.state();
        assert_eq!(initial["certain"], false, "{}", case);
        let expected = case["expected_floor"].as_u64().unwrap();
        if case["historical_peak_unknown"] == true {
            assert_eq!(
                initial["peak"], 0,
                "invalid sole H cannot fabricate history"
            );
        } else {
            assert_eq!(
                initial["peak"], expected,
                "valid alternate H survives invalid MAX/total"
            );
        }
        let before = worker.usage();
        assert!(
            worker
                .blobs()
                .put(
                    &format!("chunks/{}", hash(b"blocked numeric mutation")),
                    b"blocked numeric mutation"
                )
                .is_err()
        );
        assert_eq!(
            worker
                .request(
                    "POST",
                    "/_services/gc",
                    br#"{"expected_generation":1,"live_keys":[]}"#,
                    &[]
                )
                .0,
            409
        );
        let observed = worker.usage();
        assert_eq!(
            observed.calls.do_rows_written, before.calls.do_rows_written,
            "reads/stale recovery cannot repair metadata"
        );
        worker.fault("");
        worker.page_fault("");
        let live: Vec<String> = serde_json::from_value(case["live"].clone()).unwrap();
        worker.gc(0, &live);
        let state = worker.state();
        let usage = worker.usage();
        assert_eq!(state["certain"], true);
        assert_eq!(state["high_water"][0]["bytes"], expected);
        assert_eq!(usage.r2_peak_bytes, expected);
        assert_eq!(usage.r2_live_objects, live.len() as u64);
        let trace = state["page_fault"]["trace"].as_array().unwrap();
        let checkpoint = trace
            .iter()
            .position(|event| event == "fallback:write")
            .unwrap();
        let durable = trace[checkpoint + 1..]
            .iter()
            .position(|event| event == "sync:durable")
            .unwrap()
            + checkpoint
            + 1;
        if !live.is_empty() {
            assert!(durable < trace.iter().position(|event| event == "r2:delete").unwrap());
        }
        reports.push(json!({"case":case,"reopened_invalid":initial,"reconciled":state,"usage":worker.usage_json()}));
    }
    runtime.stop();
    let mut runtime = Runtime::start(&root, 3);
    for case in &cases {
        let worker = Worker::named(case["repo"].as_str().unwrap().to_owned());
        let usage = worker.usage();
        assert_eq!(
            usage.r2_peak_bytes,
            case["expected_floor"].as_u64().unwrap()
        );
        assert_eq!(worker.state()["certain"], true);
    }
    runtime.stop();
    fs::write(
        root.join("numeric-recovery.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
    println!("numeric recovery artifacts: {}", root.display());
}

#[test]
#[ignore = "owns 8833; actual schema/first-mutation/realistic page viability control, not a benchmark"]
fn actual_catalog_page_shape_gate() {
    let root = artifacts("shape");
    let mut runtime = Runtime::start(&root, 1);
    let mut report = Vec::new();
    let mut reopened = Vec::new();
    for rows in [11usize, 13] {
        for mode in 0..3 {
            let worker = Worker::new(&format!("shape-{rows}-v{mode}"));
            let initial = worker.usage();
            let before_register = initial.calls.do_requests;
            let ack = worker.register(mode);
            assert_eq!(ack.get("catalog_pages").is_some(), mode == 2);
            assert_eq!(
                worker.usage().calls.do_requests - before_register,
                1,
                "registration performs no DO dispatch"
            );
            assert!(!schema(&worker.state(), "peak_bytes"));
            assert!(!schema(&worker.state(), "entries"));
            let mut chunks = Vec::new();
            for i in 0..rows {
                let data = vec![i as u8; if rows == 11 { 4 } else { 5 } * 1024 * 1024 + i];
                chunks.push(json!({"key":worker.put(&data),"len":data.len()}));
            }
            let mut packs = Vec::new();
            let mut offset = 0;
            for i in 0..11 {
                let length = if i < rows - 11 { 2 } else { 1 };
                let bundle = chunks[offset..offset + length].to_vec();
                offset += length;
                let size: u64 = bundle
                    .iter()
                    .map(|entry| entry["len"].as_u64().unwrap())
                    .sum();
                packs.push(json!({"id":format!("{i:040x}"),"pack":[],"index":[],"bundle":bundle,"pack_bytes":size-20,"index_bytes":20,"level":i%3}));
            }
            let refs: serde_json::Map<String, Value> = (0..33)
                .map(|i| {
                    (
                        format!("refs/heads/backend-shape-reference-{i:02}"),
                        json!(format!("{i:040x}")),
                    )
                })
                .collect();
            worker.json("POST", "/_services/manifest", &json!({"expected":0,"next":{"generation":1,"refs":refs,"packs":packs,"garbage":[]}}));
            let after_json = worker.usage_json();
            let after: ServiceUsage = serde_json::from_value(after_json.clone()).unwrap();
            let delta = after.checked_workload_delta(&initial).unwrap();
            let state = worker.state();
            assert_eq!(schema(&state, "peak_bytes"), mode >= 1);
            assert_eq!(schema(&state, "entries"), mode == 2);
            assert_eq!(
                state["catalog"].as_array().unwrap().len(),
                if mode == 2 { 1 } else { rows }
            );
            assert!(after_json["manifest_bytes"].as_u64().unwrap() >= 4800);
            assert!(
                after_json["do_sqlite_bytes"].as_u64().unwrap() <= 24821,
                "another SQLite page fails"
            );
            if mode == 2 {
                assert_eq!(
                    state["catalog"][0]["entries"].as_array().unwrap().len(),
                    6 + 40 * rows
                );
            }
            let alters: Vec<_> = state["sql"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|cursor| {
                    cursor["statement"]
                        .as_str()
                        .unwrap()
                        .starts_with("ALTER TABLE")
                })
                .cloned()
                .collect();
            assert_eq!(alters.len(), mode);
            assert!(alters.iter().all(|cursor| cursor["rowsWritten"] == 1));
            report.push(json!({"physical_keys":rows,"mode":mode,"usage":after_json,"delta":delta,"state":state,"actual_alter_cursors":alters}));
            if mode == 2 {
                let keys: Vec<_> = chunks
                    .iter()
                    .map(|chunk| chunk["key"].as_str().unwrap().to_owned())
                    .collect();
                worker.gc(1, &keys);
                let rebuilt = worker.usage_json();
                assert!(rebuilt["do_sqlite_bytes"].as_u64().unwrap() <= 24821);
                assert_eq!(entries(&worker.state()), rows);
                report.push(json!({"physical_keys":rows,"mode":mode,"after_authoritative_gc":rebuilt,"state":worker.state()}));
            }
            reopened.push(worker.repo);
        }
    }
    runtime.stop();
    let mut runtime = Runtime::start(&root, 2);
    for repo in reopened {
        let worker = Worker::named(repo);
        let metrics = worker.usage_json();
        assert!(metrics["do_sqlite_bytes"].as_u64().unwrap() <= 24821);
        report.push(json!({"reopened_repo":worker.repo,"usage":metrics,"state":worker.state()}));
    }
    runtime.stop();
    fs::write(
        root.join("shape.json"),
        serde_json::to_vec_pretty(
            &json!({"kind":"actual_schema_shape_correctness_not_performance","cases":report}),
        )
        .unwrap(),
    )
    .unwrap();
    println!("shape artifacts: {}", root.display());
}
