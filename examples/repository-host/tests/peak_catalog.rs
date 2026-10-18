//! Opt-in Rust contracts against the actual local Worker/R2/SQLite backend.
//! Use cloudflare/test/peak-catalog.toml and a fresh persist directory.
use git_zero_host::{
    Repository,
    accounting::ServiceUsage,
    cloudflare::HttpServices,
    hash,
    storage::{BlobStore, DurableStateStore, Manifest, R2BlobStore, RetiringCas},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Worker {
    origin: String,
    repo: String,
    services: Arc<HttpServices>,
}

struct Wrangler {
    child: Child,
}

impl Wrangler {
    fn start(root: &std::path::Path, phase: usize) -> Self {
        let log = fs::File::create(root.join(format!("wrangler-{phase}.log"))).unwrap();
        let cli = std::env::var("GIT_ZERO_PEAK_WRANGLER").unwrap_or_else(|_|
            "/tmp/git-zero-hosting-fused-retirement/examples/repository-host/cloudflare/node_modules/wrangler/bin/wrangler.js".into());
        let config =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/test/peak-catalog.toml");
        let child = Command::new("setsid")
            .arg("node")
            .arg(cli)
            .args(["dev", "--config"])
            .arg(config)
            .args([
                "--ip",
                "127.0.0.1",
                "--port",
                "8832",
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
        let mut running = Self { child };
        for _ in 0..100 {
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "Wrangler exited; inspect {}",
                root.display()
            );
            if fs::read_to_string(root.join(format!("wrangler-{phase}.log")))
                .unwrap()
                .contains("Ready on http://127.0.0.1:8832")
            {
                return running;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Wrangler did not start; inspect {}", root.display());
    }

    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let status = Command::new("kill")
                .args(["-TERM", "--", &format!("-{}", self.child.id())])
                .status()
                .unwrap();
            assert!(status.success());
            self.child.wait().unwrap();
        }
    }
}

impl Drop for Wrangler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
#[ignore = "owns port 8832: Rust starts/stops three real Wrangler processes with one persisted state"]
fn process_peak_restart_contract() {
    let reservation = std::net::TcpListener::bind("127.0.0.1:8832")
        .expect("stop only the previous owned 8832 Wrangler first");
    assert_eq!(
        std::env::var("GIT_ZERO_CLOUDFLARE_ORIGIN").unwrap(),
        "http://127.0.0.1:8832"
    );
    let root = std::env::var("GIT_ZERO_PEAK_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("git-zero-peak-restart-{}", std::process::id()))
        });
    fs::create_dir_all(&root).unwrap();
    assert!(
        !root.join("persist").exists(),
        "restart proof requires fresh persisted state"
    );
    drop(reservation);
    let mut runtime = Wrangler::start(&root, 1);
    let mut cases = Vec::new();
    for label in [
        "carrier",
        "empty",
        "legacy",
        "put-gap",
        "put-gap-metrics",
        "gc-reset",
        "gc-checkpoint",
    ] {
        let worker = Worker::new(label);
        worker.register(label != "legacy");
        let a = worker.put(&[1; 100]);
        let mut keys = vec![a.clone()];
        let expected;
        match label {
            "carrier" => {
                let b = worker.put(&[2; 40]);
                let c = worker.put(&[3; 10]);
                worker.put(&[1; 100]);
                worker.services_blob().delete(&a).unwrap();
                keys = vec![b, c];
                expected = 150;
                assert!(worker.state()["high_water"].as_array().unwrap().is_empty());
            }
            "empty" => {
                worker.services_blob().delete(&a).unwrap();
                keys.clear();
                expected = 100;
            }
            "legacy" => {
                worker.services_blob().delete(&a).unwrap();
                keys = vec![worker.put(b"legacy-small")];
                expected = 100;
                assert!(!v2(&worker.state()));
            }
            "put-gap" | "put-gap-metrics" => {
                worker.fault("catalog-put");
                assert!(
                    worker
                        .services_blob()
                        .put(&format!("chunks/{}", hash(&[2; 70])), &[2; 70])
                        .is_err()
                );
                expected = 170;
            }
            "gc-reset" | "gc-checkpoint" => {
                worker.put(&[2; 70]);
                worker.fault(if label == "gc-reset" {
                    "gc-reset"
                } else {
                    "sync"
                });
                assert_eq!(worker.gc(0, &keys).0, 500);
                assert_eq!(worker.state()["high_water"][0]["bytes"], 170);
                let trace = worker.state()["trace"].as_array().unwrap().clone();
                let barrier = trace
                    .iter()
                    .position(|event| event == "sync:durable")
                    .unwrap();
                if label == "gc-checkpoint" {
                    assert!(!trace.iter().any(|event| event == "r2:delete"));
                } else {
                    assert!(barrier < trace.iter().position(|event| event == "r2:delete").unwrap());
                }
                expected = 170;
            }
            _ => unreachable!(),
        }
        cases.push(
            json!({"label":label,"repo":worker.repo,"keys":keys,"expected_peak":expected,
            "seed_state":worker.state(),"seed_epoch":worker.usage().counter_epoch}),
        );
    }
    runtime.stop();
    let mut runtime = Wrangler::start(&root, 2);
    for case in &mut cases {
        let worker = Worker::named(case["repo"].as_str().unwrap().to_owned());
        let initial = worker.state();
        if case["label"] == "legacy" {
            assert!(!v2(&initial), "read-only reopen cannot ALTER v1");
            let before = worker.usage();
            worker.register(true);
            let extra = worker.put(b"migration-after-real-restart");
            let after = worker.usage();
            assert_eq!(
                after
                    .checked_workload_delta(&before)
                    .unwrap()
                    .do_rows_written,
                Some(2),
                "ALTER schema row remains in first PUT"
            );
            case["keys"].as_array_mut().unwrap().push(json!(extra));
            assert!(v2(&worker.state()));
        } else {
            assert!(
                v2(&initial),
                "schema survives complete Worker/DO process restart"
            );
            if case["label"] == "put-gap" || case["label"] == "put-gap-metrics" {
                assert_eq!(
                    initial["peak"], 100,
                    "SQL alone cannot reconstruct unrecorded physical PUT"
                );
                if case["label"] == "put-gap" {
                    let orphan = format!("chunks/{}", hash(&[2; 70]));
                    assert!(
                        worker.services_blob().delete(&orphan).is_err(),
                        "HEAD detects uncataloged physical object before deletion"
                    );
                } else {
                    worker.usage();
                }
                assert_eq!(worker.state()["certain"], false);
                assert!(
                    worker
                        .services_blob()
                        .put(
                            &format!("chunks/{}", hash(b"blocked-after-observation")),
                            b"blocked-after-observation"
                        )
                        .is_err()
                );
            } else {
                assert_eq!(initial["peak"], case["expected_peak"]);
            }
        }
        // Lost registry and an old Native registration both leave v2 safe.
        worker.register(false);
        let keys: Vec<String> = serde_json::from_value(case["keys"].clone()).unwrap();
        assert_eq!(worker.gc(0, &keys).0, 200);
        let usage = worker.usage();
        assert_ne!(
            serde_json::to_value(&usage.counter_epoch).unwrap(),
            case["seed_epoch"]
        );
        assert_eq!(usage.r2_peak_bytes, case["expected_peak"].as_u64().unwrap());
        assert_eq!(usage.r2_live_objects, keys.len() as u64);
        assert!(v2(&worker.state()));
        worker.services.unregister().unwrap();
        let old_client_key = worker.put(b"old-client-v2");
        let old_client_state = worker.state();
        assert!(
            old_client_state["catalog"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["key"] == old_client_key
                    && row["peak_bytes"] == case["expected_peak"])
        );
        worker.services_blob().delete(&old_client_key).unwrap();
        case["recovered_state"] = worker.state();
    }
    runtime.stop();
    let mut runtime = Wrangler::start(&root, 3);
    for case in &mut cases {
        let worker = Worker::named(case["repo"].as_str().unwrap().to_owned());
        let state = worker.state();
        assert!(v2(&state));
        assert_eq!(state["peak"], case["expected_peak"], "{}", case["label"]);
        assert_eq!(
            worker.usage().r2_peak_bytes,
            case["expected_peak"].as_u64().unwrap()
        );
        case["second_restart_state"] = state;
    }
    runtime.stop();
    fs::write(
        root.join("restart.json"),
        serde_json::to_vec_pretty(
            &json!({"kind":"actual_three_process_restart_correctness","cases":cases}),
        )
        .unwrap(),
    )
    .unwrap();
    println!(
        "Actual three-process restart receipts: {}",
        root.join("restart.json").display()
    );
}

impl Worker {
    fn new(label: &str) -> Self {
        let repo = format!(
            "peak-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        Self::named(repo)
    }

    fn named(repo: String) -> Self {
        let origin =
            std::env::var("GIT_ZERO_CLOUDFLARE_ORIGIN").expect("explicit actual Worker origin");
        let services =
            Arc::new(HttpServices::new(&origin, &repo, "git-zero-local-services").unwrap());
        Self {
            origin,
            repo,
            services,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> (u16, Vec<u8>) {
        let mut process = Command::new("curl");
        process.args([
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--request",
            method,
            "--write-out",
            "\n%{http_code}",
        ]);
        for (name, value) in [
            ("x-hosting-token", "git-zero-local-services"),
            ("x-hosting-repo", self.repo.as_str()),
            ("x-hosting-phase", "push"),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            process.arg("--header").arg(format!("{name}: {value}"));
        }
        if !body.is_empty() {
            process.args(["--data-binary", "@-"]);
        }
        let mut child = process
            .arg(format!("{}{path}", self.origin))
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
        assert_eq!(output.stdout[split], b'\n');
        let status = std::str::from_utf8(&output.stdout[split + 1..])
            .unwrap()
            .parse()
            .unwrap();
        output.stdout.truncate(split);
        (status, output.stdout)
    }

    fn json(&self, method: &str, path: &str, value: &Value) -> Value {
        let (status, body) = self.request(method, path, &serde_json::to_vec(value).unwrap(), &[]);
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    fn state(&self) -> Value {
        let (status, body) = self.request("GET", "/_services/test/state", &[], &[]);
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }
    fn fault(&self, mode: &str) {
        self.json("POST", "/_services/test/fault", &json!({"mode":mode}));
    }
    fn usage(&self) -> ServiceUsage {
        serde_json::from_value(self.services.usage().unwrap()).unwrap()
    }
    fn register(&self, peak: bool) -> Value {
        self.json(
            "POST",
            "/_services/register",
            &json!({"codec_origin":"http://127.0.0.1:1",
            "manifest_retire":1,"manifest_upload_retire":1,"catalog_peak":if peak {1} else {0}}),
        )
    }
    fn put(&self, data: &[u8]) -> String {
        let key = format!("chunks/{}", hash(data));
        self.services_blob().put(&key, data).unwrap();
        key
    }
    fn services_blob(&self) -> R2BlobStore<HttpServices> {
        R2BlobStore::new(self.services.clone())
    }
    fn gc(&self, generation: u64, keys: &[String]) -> (u16, Vec<u8>) {
        self.request(
            "POST",
            "/_services/gc",
            &serde_json::to_vec(&json!({
            "expected_generation":generation,"live_keys":keys}))
            .unwrap(),
            &[],
        )
    }
}

fn v2(state: &Value) -> bool {
    state["schema"]
        .as_array()
        .unwrap()
        .iter()
        .any(|column| column["name"] == "peak_bytes")
}

#[test]
#[ignore = "requires actual test Worker; verifies authentication and mutation-only migration"]
fn real_peak_capability_is_repo_scoped_and_migration_mutation_only() {
    let worker = Worker::new("gate");
    let legacy = worker.register(false);
    assert!(legacy.get("catalog_peak").is_none());
    let forged = b"legacy-forged-capability";
    let key = format!("chunks/{}", hash(forged));
    assert_eq!(
        worker
            .request(
                "PUT",
                &format!("/_services/blob/{key}"),
                forged,
                &[("x-git-zero-internal-catalog-peak", "1")]
            )
            .0,
        204
    );
    assert!(!v2(&worker.state()), "caller cannot mint capability");
    worker.services.unregister().unwrap();
    assert_eq!(
        worker
            .request(
                "PUT",
                &format!("/_services/blob/{key}"),
                forged,
                &[("x-git-zero-internal-catalog-peak", "1")]
            )
            .0,
        204
    );
    assert!(!v2(&worker.state()), "unregistered caller cannot migrate");
    assert_eq!(
        worker
            .request(
                "POST",
                "/_services/register",
                b"{\"codec_origin\":\"http://127.0.0.1:1\",\"catalog_peak\":1}",
                &[("x-hosting-token", "wrong")]
            )
            .0,
        401
    );
    let ack = worker.json(
        "POST",
        "/_services/register",
        &json!({"codec_origin":"http://127.0.0.1:1","catalog_peak":"1"}),
    );
    assert!(ack.get("catalog_peak").is_none());
    worker.register(true);
    let before = worker.usage();
    assert_eq!(
        worker
            .request("GET", &format!("/_services/blob/{key}"), &[], &[])
            .0,
        200
    );
    assert_eq!(
        worker
            .request(
                "POST",
                "/_services/blobs/read",
                &serde_json::to_vec(&json!({"chunks":[{"key":key,"len":forged.len()}]})).unwrap(),
                &[]
            )
            .0,
        200
    );
    worker.json(
        "POST",
        "/_services/manifest",
        &json!({"expected":0,"next":{"generation":1,"refs":{},"packs":[]}}),
    );
    assert_eq!(worker.gc(0, &[]).0, 409);
    // RegExp.test coerces an array containing one key to that valid string.
    // GC must reject the actual JSON type before capability migration or Set
    // membership could misclassify the physical live object as garbage.
    assert_eq!(
        worker
            .request(
                "POST",
                "/_services/gc",
                &serde_json::to_vec(&json!({"expected_generation":1,"live_keys":[[key]]})).unwrap(),
                &[]
            )
            .0,
        400
    );
    assert!(!v2(&worker.state()));
    assert_eq!(
        worker
            .request("GET", &format!("/_services/blob/{key}"), &[], &[])
            .0,
        200
    );
    assert_eq!(
        worker
            .request("PUT", &format!("/_services/blob/{key}"), b"wrong hash", &[])
            .0,
        409
    );
    assert_eq!(
        worker
            .request("POST", "/_services/manifest/upload-retire", b"invalid", &[])
            .0,
        400
    );
    worker.json(
        "POST",
        "/_services/manifest/retire",
        &json!({"version":1,"expected":0,
        "next":{"generation":1,"refs":{},"packs":[],"garbage":[key]}}),
    );
    assert!(!v2(&worker.state()));
    let after = worker.usage();
    assert_eq!(after.calls.r2_deletes, before.calls.r2_deletes);
    assert_eq!(
        after.calls.do_rows_written.unwrap() - before.calls.do_rows_written.unwrap(),
        1,
        "only the valid plain manifest CAS may write; reads/invalid/stale requests never ALTER"
    );
    let peak_before = after.r2_peak_bytes;
    let candidate = worker.put(b"candidate");
    assert!(v2(&worker.state()));
    worker.services.unregister().unwrap();
    worker.put(b"old-native-on-v2");
    assert!(v2(&worker.state()));
    assert!(worker.usage().r2_peak_bytes >= peak_before);
    assert_eq!(worker.gc(1, &[key, candidate]).0, 200);
}

#[test]
#[ignore = "requires actual test Worker; verifies duplicate PUTs and last/alternate witnesses"]
fn real_peak_duplicate_and_last_or_alternate_witness_deletes() {
    let worker = Worker::new("witness");
    worker.register(true);
    let a = worker.put(&[1; 100]);
    let b = worker.put(&[2; 40]);
    let c = worker.put(&[3; 10]);
    let before = worker.usage();
    assert_eq!(worker.put(&[1; 100]), a);
    let duplicate = worker.usage();
    assert_eq!(duplicate.r2_live_bytes, 150);
    assert_eq!(duplicate.r2_peak_bytes, 150);
    assert_eq!(
        duplicate
            .checked_workload_delta(&before)
            .unwrap()
            .do_rows_written,
        Some(1)
    );
    worker.services_blob().delete(&a).unwrap();
    let alternate = worker.state();
    assert!(
        alternate["high_water"].as_array().unwrap().is_empty(),
        "equal surviving row avoids fallback"
    );
    assert_eq!(alternate["owner"]["key"], c);
    worker.services_blob().delete(&c).unwrap();
    let last = worker.state();
    assert_eq!(last["high_water"][0]["bytes"], 150);
    let trace = last["trace"].as_array().unwrap();
    let fallback = trace
        .iter()
        .position(|event| {
            event
                .as_str()
                .is_some_and(|s| s.starts_with("INSERT INTO high_water"))
        })
        .unwrap();
    assert!(
        trace[fallback + 1..]
            .iter()
            .position(|event| event == "sync:durable")
            .unwrap()
            < trace[fallback + 1..]
                .iter()
                .position(|event| event == "r2:delete")
                .unwrap()
    );
    worker.services_blob().delete(&b).unwrap();
    worker.services_blob().delete(&b).unwrap();
    let empty = worker.usage();
    assert_eq!(empty.r2_live_bytes, 0);
    assert_eq!(empty.r2_peak_bytes, 150);
    worker.put(b"below-historical-peak");
    let lower = worker.usage();
    assert_eq!(lower.r2_peak_bytes, 150);
    assert_eq!(
        lower
            .checked_workload_delta(&empty)
            .unwrap()
            .do_rows_written,
        Some(1)
    );
}

#[test]
#[ignore = "requires actual test Worker; real binding/catalog/barrier faults must require GC"]
fn real_peak_faults_block_ordinary_mutations_until_inventory_reconciliation() {
    for mode in [
        "put",
        "catalog-put",
        "sync",
        "delete",
        "catalog-delete",
        "fallback-sql",
        "fallback-sync",
        "gc-reset",
        "migration",
        "migration-after",
    ] {
        let worker = Worker::new(mode);
        worker.register(true);
        if mode.starts_with("migration") {
            worker.fault(mode);
            assert!(
                worker
                    .services_blob()
                    .put(&format!("chunks/{}", hash(b"migration")), b"migration")
                    .is_err()
            );
            assert!(!worker.state()["certain"].as_bool().unwrap());
            assert_eq!(worker.gc(0, &[]).0, 200);
            assert!(v2(&worker.state()));
            worker.put(b"after-recovery");
            continue;
        }
        let a = worker.put(&[1; 100]);
        let b = worker.put(&[2; 40]);
        let before = worker.usage();
        worker.fault(if mode == "fallback-sync" {
            "sync"
        } else {
            mode
        });
        let result = if ["put", "catalog-put", "sync"].contains(&mode) {
            worker
                .services_blob()
                .put(&format!("chunks/{}", hash(&[3; 70])), &[3; 70])
        } else if mode == "gc-reset" {
            assert_eq!(worker.gc(0, std::slice::from_ref(&a)).0, 500);
            // The response must be a real failure after its durable checkpoint.
            assert_eq!(worker.state()["fired"], true);
            Err(std::io::Error::other("assert dirty below"))
        } else {
            worker.services_blob().delete(&b)
        };
        assert!(result.is_err(), "{mode}");
        assert_eq!(worker.state()["certain"], false, "{mode}");
        let failed = worker.usage();
        let attempts = |usage: &ServiceUsage, name: &str| {
            usage.operations.get(name).map_or(0, |op| op.attempts)
        };
        if matches!(mode, "fallback-sql" | "fallback-sync") {
            assert_eq!(
                attempts(&failed, "r2_delete"),
                attempts(&before, "r2_delete"),
                "fallback failure prevents delete"
            );
        }
        assert!(
            worker
                .services_blob()
                .put(&format!("chunks/{}", hash(b"blocked")), b"blocked")
                .is_err()
        );
        assert!(worker.services_blob().delete(&a).is_err());
        let blocked = worker.usage();
        assert_eq!(attempts(&blocked, "r2_put"), attempts(&failed, "r2_put"));
        assert_eq!(
            attempts(&blocked, "r2_delete"),
            attempts(&failed, "r2_delete")
        );
        let peak = if matches!(mode, "catalog-put" | "sync") {
            210
        } else {
            140
        };
        let live = if matches!(mode, "catalog-delete" | "gc-reset") {
            vec![a]
        } else {
            vec![a, b]
        };
        let (status, body) = worker.gc(0, &live);
        assert_eq!(status, 200, "{mode}: {}", String::from_utf8_lossy(&body));
        let state = worker.state();
        assert_eq!(state["certain"], true, "{mode}");
        assert_eq!(state["high_water"][0]["bytes"], peak, "{mode}");
        assert_eq!(worker.usage().r2_peak_bytes, peak);
        worker.put(b"after-recovery");
        if matches!(
            mode,
            "catalog-put" | "catalog-delete" | "fallback-sql" | "gc-reset"
        ) {
            assert_eq!(
                worker.usage().calls.do_rows_written,
                None,
                "failed cursor row counts cannot become invented zero"
            );
        }
    }
}

#[test]
#[ignore = "requires real local Wrangler with test/peak-catalog.toml and fresh state"]
fn real_peak_capability_normal_writes_and_realistic_sqlite_pages() {
    let mut normal = Vec::new();
    for peak in [false, true] {
        let worker = Worker::new(if peak { "normal-v2" } else { "normal-v1" });
        let before_registration = worker.usage();
        let ack = worker.register(peak);
        assert_eq!(ack.get("catalog_peak").is_some(), peak);
        let after_registration = worker.usage();
        assert_eq!(
            after_registration.calls.do_requests - before_registration.calls.do_requests,
            1
        );
        assert!(!v2(&worker.state()), "registration/read must not migrate");
        // Exercise the actual new Native registration body/negotiation too.
        worker.services.register("http://127.0.0.1:1").unwrap();
        if !peak {
            worker.register(false);
        }
        let repository = Repository::new(
            worker.services_blob(),
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
        let before = worker.usage();
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
        let result = repository.publish_staged_retiring(&previous, next.clone(), Some(pending));
        // Raw legacy registration deliberately lacks the SDK capability flags.
        // Both current paths still negotiate upload fusion through registration.
        let RetiringCas::Published(receipt) = result.unwrap() else {
            panic!("not published");
        };
        assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
        let after = worker.usage();
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(after.r2_peak_bytes, 47);
        assert_eq!(after.r2_live_bytes, 17);
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_class_b, 0);
        assert_eq!(delta.r2_deletes, 3);
        let state = worker.state();
        assert_eq!(v2(&state), peak);
        if peak {
            assert!(state["high_water"].as_array().unwrap().is_empty());
            assert!(state["sql"].as_array().unwrap().iter().any(|cursor| {
                cursor["statement"]
                    .as_str()
                    .unwrap()
                    .starts_with("ALTER TABLE")
                    && cursor["rowsWritten"] == 1
            }));
        }
        normal.push(delta);
    }
    assert_eq!(normal[0].r2_written_bytes, normal[1].r2_written_bytes);
    assert_eq!(normal[0].do_requests, normal[1].do_requests);
    assert_eq!(
        normal[0].do_rows_written.unwrap(),
        normal[1].do_rows_written.unwrap() + 1
    );

    let mut shapes = Vec::new();
    for rows in [11, 13] {
        let mut pair = Vec::new();
        for peak in [false, true] {
            let worker = Worker::new(&format!("shape-{rows}-{peak}"));
            worker.register(peak);
            let initial = worker.usage();
            let mut chunks = Vec::new();
            for i in 0..rows {
                let data = vec![i as u8; if rows == 11 { 4 } else { 5 } * 1024 * 1024 + i];
                chunks.push(json!({"key":worker.put(&data),"len":data.len()}));
            }
            let mut packs = Vec::new();
            let mut offset = 0;
            for i in 0..11 {
                let count = if i < rows - 11 { 2 } else { 1 };
                let bundle = chunks[offset..offset + count].to_vec();
                offset += count;
                let size: u64 = bundle
                    .iter()
                    .map(|chunk| chunk["len"].as_u64().unwrap())
                    .sum();
                packs.push(
                    json!({"id":format!("{i:040x}"),"pack":[],"index":[],"bundle":bundle,
                    "pack_bytes":size-20,"index_bytes":20,"level":i%3}),
                );
            }
            let refs: serde_json::Map<String, Value> = (0..33)
                .map(|i| {
                    (
                        format!("refs/heads/backend-shape-reference-{i:02}"),
                        json!(format!("{i:040x}")),
                    )
                })
                .collect();
            worker.json(
                "POST",
                "/_services/manifest",
                &json!({"expected":0,
                "next":{"generation":1,"refs":refs,"packs":packs,"garbage":[]}}),
            );
            let after_json = worker.services.usage().unwrap();
            let after: ServiceUsage = serde_json::from_value(after_json.clone()).unwrap();
            let delta = after.checked_workload_delta(&initial).unwrap();
            assert!(after_json["manifest_bytes"].as_u64().unwrap() >= 4800);
            assert!(after.r2_peak_bytes > 39_000_000);
            let catalog_state = worker.state();
            assert_eq!(catalog_state["catalog"].as_array().unwrap().len(), rows);
            let schema_cursor = catalog_state["sql"]
                .as_array()
                .unwrap()
                .iter()
                .find(|cursor| cursor["statement"] == "PRAGMA table_info(blob_catalog)")
                .unwrap();
            let migration_cursors: Vec<_> = catalog_state["sql"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|cursor| {
                    cursor["statement"]
                        .as_str()
                        .unwrap()
                        .starts_with("ALTER TABLE")
                })
                .collect();
            pair.push(json!({"peak_capability":peak,"catalog_rows":rows,"manifest_bytes":after_json["manifest_bytes"],
                "peak_bytes":after.r2_peak_bytes,"sqlite_bytes":after_json["do_sqlite_bytes"],
                "sql_reads":delta.do_rows_read,"sql_writes":delta.do_rows_written,
                "constructor_schema_cursor":schema_cursor,"migration_cursors":migration_cursors,
                "initial_observer_counters":initial.calls}));
        }
        assert_eq!(
            pair[0]["sqlite_bytes"], pair[1]["sqlite_bytes"],
            "new SQLite page rejects candidate"
        );
        // Actual local ALTER writes one SQLite schema row in the first PUT.
        // It stays charged to that mutation, reducing this fresh saving by one.
        assert_eq!(
            pair[0]["sql_writes"].as_u64().unwrap(),
            pair[1]["sql_writes"].as_u64().unwrap() + rows as u64 - 1
        );
        shapes.push(pair);
    }
    let report = json!({"kind":"actual_backend_correctness_shape_control_not_benchmark",
        "normal_retirement_deltas":normal,"shapes":shapes});
    println!("{report}");
    if let Ok(path) = std::env::var("GIT_ZERO_PEAK_ARTIFACTS") {
        fs::create_dir_all(&path).unwrap();
        fs::write(
            PathBuf::from(path).join("shape.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}
