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
    storage::{
        BlobStore, Chunk, DurableStateStore, Manifest, R2BlobStore, RetiringCas, StateStore,
        StoredPack,
    },
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

fn fixture_data(mut state: u32) -> Vec<u8> {
    (0..9 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

struct NativeFixture {
    data: Vec<u8>,
    tip: String,
    blob: String,
    tree: String,
    id: String,
    pack: Vec<u8>,
    index: Vec<u8>,
}

fn native_fixture(source: &std::path::Path, seed: u32) -> NativeFixture {
    let data = fixture_data(seed);
    let output = |arguments: &[&str], input: &[u8]| {
        String::from_utf8(git(source, arguments, input))
            .unwrap()
            .trim()
            .to_string()
    };
    let blob = output(&["hash-object", "-w", "--stdin"], &data);
    let tree = output(
        &["mktree"],
        format!("100644 blob {blob}\tpayload.bin\n").as_bytes(),
    );
    let tip = output(
        &[
            "-c",
            "user.name=Contract",
            "-c",
            "user.email=contract@example.invalid",
            "commit-tree",
            &tree,
        ],
        b"bounded cloud layout fixture\n",
    );
    let pack = git(
        source,
        &["pack-objects", "--stdout", "--revs"],
        format!("{tip}\n").as_bytes(),
    );
    let indexed = output(&["index-pack", "--stdin", "--strict"], &pack);
    let id = indexed.split_whitespace().last().unwrap().to_string();
    let index = fs::read(source.join(format!("objects/pack/pack-{id}.idx"))).unwrap();
    assert!(pack.len() > 8 * 1024 * 1024);
    assert!(pack.len() + index.len() < 16 * 1024 * 1024);
    NativeFixture {
        data,
        tip,
        blob,
        tree,
        id,
        pack,
        index,
    }
}

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

#[test]
#[ignore = "requires local workerd with cloudflare/test/upload-fault.toml"]
fn faulted_upload_barriers_catalog_partial_retirement_and_lost_receipt_preserve_authority() {
    for catalog_peak in [false, true] {
        for mode in [
            "put",
            "catalog",
            "uploadsync",
            "cassync",
            "delete",
            "lost",
            "corrupt",
        ] {
            let worker = RealWorker::new(&format!("upload-{mode}-peak{catalog_peak}"));
            let services = worker.services();
            services.register("http://127.0.0.1:1").unwrap();
            if !catalog_peak {
                register_legacy_worker(&worker);
            }
            let repository = Repository::new(
                R2BlobStore::new(services.clone()),
                DurableStateStore::new(services.clone()),
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
            let (stored, pending) = repository
                .stage_bundled_pack("new".into(), b"new-pack", b"new-index", 1)
                .unwrap()
                .into_parts();
            let key = pending.chunk().key.clone();
            let mut next = Manifest {
                generation: 2,
                packs: vec![stored],
                ..Manifest::default()
            };
            next.retire_replaced(&previous);
            let before = usage(&services);
            let result = repository.publish_staged_retiring(&previous, next.clone(), Some(pending));
            if mode == "delete" {
                let RetiringCas::Published(receipt) = result.unwrap() else {
                    panic!("partial retirement failed publication");
                };
                assert_eq!(
                    receipt
                        .outcomes
                        .iter()
                        .map(|o| o.deleted)
                        .collect::<Vec<_>>(),
                    [true, false, !catalog_peak]
                );
            } else {
                assert!(result.is_err(), "fault {mode} did not fire");
            }
            let after = usage(&services);
            let delta = after.checked_workload_delta(&before).unwrap();
            assert_eq!(delta.worker_service_requests, 1, "{mode}");
            assert_eq!(delta.do_requests, 1, "{mode}");
            let puts = operation(&after, "r2_put");
            let prior_puts = operation(&before, "r2_put");
            assert_eq!(puts.attempts - prior_puts.attempts, 1, "{mode}");
            assert_eq!(
                puts.failures - prior_puts.failures,
                u64::from(mode == "put")
            );
            let published = !matches!(mode, "put" | "catalog" | "uploadsync");
            assert_eq!(
                repository.state.load().unwrap(),
                if published {
                    next.clone()
                } else {
                    previous.clone()
                },
                "{mode}"
            );
            let cas = operation(&after, "manifest_cas");
            let prior_cas = operation(&before, "manifest_cas");
            assert_eq!(cas.attempts - prior_cas.attempts, u64::from(published));
            assert_eq!(
                cas.failures - prior_cas.failures,
                u64::from(mode == "cassync")
            );
            // CAS errors finish before retirement, including an ambiguous reply to
            // its own sync; no fallback PUT/CAS or blind orphan delete is issued.
            assert_eq!(
                delta.r2_deletes,
                if mode == "delete" && catalog_peak {
                    2
                } else if matches!(mode, "delete" | "lost" | "corrupt") {
                    3
                } else {
                    0
                }
            );
            assert_eq!(
                operation(&after, "r2_delete").failures - operation(&before, "r2_delete").failures,
                u64::from(mode == "delete")
            );
            if mode != "put" {
                assert!(repository.blobs.get(&key).is_ok());
            }
            let authoritative = repository.state.load().unwrap();
            // This authoritative listing also reconciles a PUT whose catalog
            // write was interrupted. Live shared chunks remain untouched.
            let(status,body)=local_request(&worker.origin,"POST","/_services/gc",&[("x-hosting-token",&worker.token),("x-hosting-repo",&worker.namespace)],
            &serde_json::to_vec(&serde_json::json!({"expected_generation":authoritative.generation,"live_keys":authoritative.packs.iter().flat_map(StoredPack::chunks).map(|c|&c.key).collect::<Vec<_>>()})).unwrap());
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            for pack in &authoritative.packs {
                repository.read_pack_files(pack).unwrap();
            }
            let reconciled = usage(&services);
            assert_eq!(reconciled.r2_live_objects, authoritative.packs.len() as u64);
            assert_eq!(reconciled.accounting_complete, Some(true));
            assert_eq!(
                reconciled.r2_peak_bytes,
                if mode == "put" { 30 } else { 47 }
            );
        }
    }
}

#[test]
#[ignore = "requires local workerd with cloudflare/test/upload-fault.toml"]
fn faulted_native_upload_lost_response_recovers_before_serving_or_accepting_more_git() {
    let worker = RealWorker::new("upload-lost");
    let services = worker.services();
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let source = directory.0.join("source.git");
    fs::create_dir_all(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    let fixture = native_fixture(&source, 19);
    let mut host = NativeHost::open_cloudflare(
        directory.0.join("host"),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        false,
    )
    .unwrap();
    host.cloudflare_services()
        .unwrap()
        .register("http://127.0.0.1:1")
        .unwrap();
    let mut body = packet(&format!(
        "{} {} refs/heads/main\0report-status atomic\n",
        "0".repeat(40),
        fixture.tip
    ));
    body.extend_from_slice(b"0000");
    body.extend_from_slice(&fixture.pack);
    let before = usage(&services);
    let response = host.handle(rpc("receive", body));
    assert_eq!(response.status, 503);
    let committed = usage(&services);
    assert_eq!(
        operation(&committed, "r2_put").attempts - operation(&before, "r2_put").attempts,
        1
    );
    assert_eq!(
        operation(&committed, "manifest_cas").attempts
            - operation(&before, "manifest_cas").attempts,
        1
    );
    let request = Request {
        method: "GET".into(),
        target: "/repo.git/info/refs?service=git-upload-pack".into(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    };
    let response = host.handle(request);
    assert_eq!(response.status, 200);
    assert!(String::from_utf8_lossy(&response.body).contains(&fixture.tip));
    let recovered = usage(&services);
    assert_eq!(
        operation(&recovered, "r2_put"),
        operation(&committed, "r2_put")
    );
    assert_eq!(
        operation(&recovered, "manifest_cas"),
        operation(&committed, "manifest_cas")
    );
    let view = directory.0.join("host/view/repo.git");
    git(&view, &["fsck", "--strict"], &[]);
    assert_eq!(
        git(&view, &["cat-file", "blob", &fixture.blob], &[]),
        fixture.data
    );
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_upload_frame_overflow_falls_back_to_exactly_one_durable_put_then_publication() {
    let worker = RealWorker::new("upload-overflow");
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
    let mut repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    repository.chunk_bytes = 16 * 1024 * 1024;
    let data = vec![0x75; 16 * 1024 * 1024];
    let (stored, pending) = repository
        .stage_bundled_pack("x".repeat(3000), &data, b"", 0)
        .unwrap()
        .into_parts();
    let before = usage(&services);
    let next = Manifest {
        generation: 1,
        packs: vec![stored],
        ..Manifest::default()
    };
    assert!(matches!(
        repository
            .publish_staged_retiring(&Manifest::default(), next.clone(), Some(pending))
            .unwrap(),
        RetiringCas::Published(_)
    ));
    let after = usage(&services);
    let delta = after.checked_workload_delta(&before).unwrap();
    assert_eq!(delta.worker_service_requests, 2);
    assert_eq!(delta.do_requests, 2);
    assert_eq!(delta.r2_class_a, 1);
    assert_eq!(delta.r2_written_bytes, data.len() as u64);
    assert_eq!(
        operation(&after, "manifest_cas").attempts - operation(&before, "manifest_cas").attempts,
        1
    );
    assert_eq!(repository.state.load().unwrap(), next);
    assert_eq!(repository.read_pack_files(&next.packs[0]).unwrap().0, data);
}

#[test]
#[ignore = "two-phase test-only upload-fault runtime: seed catalog interruption, restart, then verify"]
fn seed_upload_catalog_before_workerd_restart() {
    let worker = restart_fixture();
    assert!(worker.namespace.contains("-upload-catalog-"));
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    assert_eq!(repository.state.load().unwrap(), Manifest::default());
    let (stored, pending) = repository
        .stage_bundled_pack("catalog-orphan".into(), b"catalog-orphan", b"index", 0)
        .unwrap()
        .into_parts();
    let key = pending.chunk().key.clone();
    let before = usage(&services);
    assert!(
        repository
            .publish_staged_retiring(
                &Manifest::default(),
                Manifest {
                    packs: vec![stored],
                    ..Manifest::default()
                },
                Some(pending)
            )
            .is_err()
    );
    let after = usage(&services);
    assert_eq!(
        operation(&after, "r2_put").successes - operation(&before, "r2_put").successes,
        1
    );
    assert_eq!(
        operation(&after, "manifest_cas"),
        operation(&before, "manifest_cas")
    );
    assert_eq!(repository.blobs.get(&key).unwrap(), b"catalog-orphanindex");
    fs::write(
        restart_marker(&worker),
        serde_json::to_vec(&serde_json::json!({"key":key,"counter_epoch":after.counter_epoch}))
            .unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "two-phase test-only upload-fault runtime: seed and restart same persist-dir first"]
fn verify_upload_catalog_after_workerd_restart() {
    let worker = restart_fixture();
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(restart_marker(&worker)).unwrap()).unwrap();
    let services = worker.services();
    let before = usage(&services);
    assert_ne!(
        serde_json::to_value(&before.counter_epoch).unwrap(),
        marker["counter_epoch"]
    );
    let directory =
        Directory(std::env::temp_dir().join(format!("{}-catalog-recovery", worker.namespace)));
    let host = NativeHost::open_cloudflare(
        directory.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        false,
    )
    .unwrap();
    let after = usage(&services);
    assert_eq!(after.r2_live_objects, 0);
    assert_eq!(after.r2_live_bytes, 0);
    assert_eq!(after.accounting_complete, Some(true));
    assert_eq!(
        operation(&after, "r2_delete").successes - operation(&before, "r2_delete").successes,
        1
    );
    assert_eq!(
        operation(&after, "manifest_cas"),
        operation(&before, "manifest_cas")
    );
    assert_eq!(
        DurableStateStore::new(services.clone()).load().unwrap(),
        Manifest::default()
    );
    assert_eq!(
        R2BlobStore::new(services)
            .get(marker["key"].as_str().unwrap())
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    git(
        &directory.0.join("view/repo.git"),
        &["fsck", "--strict"],
        &[],
    );
    drop(host);
    fs::remove_file(restart_marker(&worker)).unwrap();
}

fn upload_frame_bytes(metadata: &[u8], data: &[u8]) -> Vec<u8> {
    let mut frame = b"GZUP".to_vec();
    frame.extend_from_slice(&1u32.to_le_bytes());
    frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    frame.extend_from_slice(metadata);
    frame.extend_from_slice(data);
    frame
}

fn upload_request(worker: &RealWorker, frame: &[u8]) -> (u16, Vec<u8>) {
    local_request(
        &worker.origin,
        "POST",
        "/_services/manifest/upload-retire",
        &[
            ("x-hosting-token", &worker.token),
            ("x-hosting-repo", &worker.namespace),
            ("x-hosting-phase", "push"),
        ],
        frame,
    )
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_upload_retirement_preserves_binding_sql_bytes_and_removes_one_callback() {
    let mut deltas = Vec::new();
    for fused in [false, true] {
        let worker = RealWorker::new(if fused {
            "upload-fused"
        } else {
            "upload-legacy"
        });
        let services = worker.services();
        // Both paths negotiate ordinary fused retirement. The baseline keeps
        // the eager import contract and therefore takes two service calls.
        services.register("http://127.0.0.1:1").unwrap();
        let repository = Repository::new(
            R2BlobStore::new(services.clone()),
            DurableStateStore::new(services.clone()),
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
        let before = usage(&services);
        let (stored, pending) = if fused {
            let (stored, pending) = repository
                .stage_bundled_pack("new".into(), b"new-pack", b"new-index", 1)
                .unwrap()
                .into_parts();
            (stored, Some(pending))
        } else {
            (
                repository
                    .store_bundled_pack("new".into(), b"new-pack", b"new-index", 1)
                    .unwrap(),
                None,
            )
        };
        let mut next = Manifest {
            generation: 2,
            packs: vec![stored],
            ..Manifest::default()
        };
        next.retire_replaced(&previous);
        let RetiringCas::Published(receipt) = repository
            .publish_staged_retiring(&previous, next.clone(), pending)
            .unwrap()
        else {
            panic!("not published");
        };
        assert_eq!(receipt.outcomes.len(), 3);
        assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
        let after = usage(&services);
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.worker_service_requests, if fused { 1 } else { 2 });
        assert_eq!(delta.do_requests, if fused { 1 } else { 2 });
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_class_b, 0);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.r2_written_bytes, 17);
        assert_eq!(
            operation(&after, "r2_put").successes - operation(&before, "r2_put").successes,
            1
        );
        assert_eq!(
            operation(&after, "manifest_cas").successes
                - operation(&before, "manifest_cas").successes,
            1
        );
        assert_eq!(repository.state.load().unwrap(), next);
        repository.read_pack_files(&next.packs[0]).unwrap();
        deltas.push(delta);
    }
    assert_eq!(deltas[0].do_rows_read, deltas[1].do_rows_read);
    assert_eq!(deltas[0].do_rows_written, deltas[1].do_rows_written);
    assert_eq!(deltas[0].r2_read_bytes, deltas[1].r2_read_bytes);
    assert_eq!(deltas[0].r2_written_bytes, deltas[1].r2_written_bytes);
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_upload_stale_shared_winner_and_orphan_recovery_never_delete_live_data() {
    let worker = RealWorker::new("upload-stale");
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    let winner = repository
        .store_bundled_pack("winner".into(), b"shared", b"index", 0)
        .unwrap();
    let next = Manifest {
        generation: 1,
        packs: vec![winner.clone()],
        ..Manifest::default()
    };
    assert!(
        repository
            .publish(&Manifest::default(), next.clone())
            .unwrap()
    );
    for shared in [true, false] {
        let (stored, pending) = repository
            .stage_bundled_pack(
                "loser".into(),
                if shared { b"shared" } else { b"orphan" },
                b"index",
                0,
            )
            .unwrap()
            .into_parts();
        let key = pending.chunk().key.clone();
        let before = usage(&services);
        assert_eq!(
            repository
                .publish_staged_retiring(
                    &Manifest::default(),
                    Manifest {
                        packs: vec![stored],
                        garbage: winner.chunks().map(|c| c.key.clone()).collect(),
                        ..Manifest::default()
                    },
                    Some(pending)
                )
                .unwrap(),
            RetiringCas::Stale
        );
        let after = usage(&services);
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(delta.r2_class_a, 1);
        assert_eq!(delta.r2_deletes, 0);
        assert_eq!(delta.worker_service_requests, 1);
        assert!(repository.blobs.get(&key).is_ok());
    }
    assert_eq!(repository.state.load().unwrap(), next);
    let (status, body) = local_request(&worker.origin, "POST", "/_services/gc", &[("x-hosting-token", &worker.token), ("x-hosting-repo", &worker.namespace)],
        &serde_json::to_vec(&serde_json::json!({"expected_generation":1,"live_keys":winner.chunks().map(|c| &c.key).collect::<Vec<_>>()})).unwrap());
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["deleted"],
        1
    );
    assert_eq!(
        repository.read_pack_files(&winner).unwrap(),
        (b"shared".to_vec(), b"index".to_vec())
    );
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_upload_frame_rejects_every_invalid_shape_before_mutation_and_accepts_exact_bounds() {
    use serde_json::json;
    let worker = RealWorker::new("upload-frame");
    let services = worker.services();
    let key = format!("chunks/{}", hash(b"x"));
    let next = Manifest {
        generation: 1,
        packs: vec![StoredPack {
            id: "one".into(),
            bundle: vec![Chunk {
                key: key.clone(),
                len: 1,
            }],
            pack_bytes: 1,
            ..StoredPack::default()
        }],
        ..Manifest::default()
    };
    let base = json!({"version":1,"expected":0,"next":next,"chunk":{"key":key,"len":1}});
    let metadata = serde_json::to_vec(&base).unwrap();
    let valid = upload_frame_bytes(&metadata, b"x");
    let mut invalid = Vec::new();
    for len in [0, 1, 4, 8, 11, 12, valid.len() - 1] {
        invalid.push(valid[..len].to_vec());
    }
    let mut bad = valid.clone();
    bad[0] = 0;
    invalid.push(bad);
    let mut bad = valid.clone();
    bad[4..8].copy_from_slice(&2u32.to_le_bytes());
    invalid.push(bad);
    for len in [0, u32::MAX, 1_500_001] {
        let mut bad = valid.clone();
        bad[8..12].copy_from_slice(&len.to_le_bytes());
        invalid.push(bad);
    }
    let mut bad = valid.clone();
    bad.push(0);
    invalid.push(bad);
    invalid.push(upload_frame_bytes(&metadata, b"y"));
    invalid.push(upload_frame_bytes(b"{malformed", b"x"));
    invalid.push(upload_frame_bytes(&[0xff], b"x"));
    invalid.push(upload_frame_bytes(b"[]", b"x"));
    invalid.push(upload_frame_bytes(b"null", b"x"));
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/expected", json!(9_007_199_254_740_992u64)),
        ("/next/generation", json!(2)),
        ("/chunk/key", json!(format!("chunks/{}", hash(b"other")))),
        ("/chunk/len", json!(0)),
        ("/chunk/len", json!(16 * 1024 * 1024 + 1)),
        ("/chunk/len", json!(u64::MAX)),
        ("/next/packs/0/bundle/0/len", json!(2)),
        ("/next/packs/0/pack", json!([{"key":key,"len":1}])),
        ("/next/packs", json!([])),
        ("/next/refs", json!({"refs/../bad":"a".repeat(40)})),
        ("/next/refs", json!({"refs/heads/main":"0".repeat(40)})),
    ] {
        let mut value2 = base.clone();
        *value2.pointer_mut(pointer).unwrap() = value;
        invalid.push(upload_frame_bytes(
            &serde_json::to_vec(&value2).unwrap(),
            b"x",
        ));
    }
    for location in [
        "",
        "/next",
        "/chunk",
        "/next/packs/0",
        "/next/packs/0/bundle/0",
    ] {
        let mut bad = base.clone();
        bad.pointer_mut(location).unwrap()["extra"] = json!(1);
        invalid.push(upload_frame_bytes(&serde_json::to_vec(&bad).unwrap(), b"x"));
    }
    for field in ["version", "expected", "next", "chunk"] {
        let mut bad = base.clone();
        bad.as_object_mut().unwrap().remove(field);
        invalid.push(upload_frame_bytes(&serde_json::to_vec(&bad).unwrap(), b"x"));
    }
    let text = String::from_utf8(metadata.clone()).unwrap();
    for text in [
        text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
        text.replacen("\"len\":1", "\"len\":1,\"l\\u0065n\":1", 1),
    ] {
        invalid.push(upload_frame_bytes(text.as_bytes(), b"x"));
    }
    let mut debt = base.clone();
    debt["next"]["garbage"] = json!(
        (0..257)
            .map(|i| format!("chunks/{i:064x}"))
            .collect::<Vec<_>>()
    );
    invalid.push(upload_frame_bytes(
        &serde_json::to_vec(&debt).unwrap(),
        b"x",
    ));
    let mut too_big = metadata.clone();
    too_big.resize(1_500_001, b' ');
    invalid.push(upload_frame_bytes(&too_big, b"x"));
    let before = usage(&services);
    for (i, bad) in invalid.iter().enumerate() {
        let (status, body) = upload_request(&worker, bad);
        assert!(
            matches!(status, 400 | 409 | 413),
            "case {i}, status {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let after = usage(&services);
    for name in ["r2_put", "manifest_cas", "r2_delete"] {
        assert_eq!(operation(&before, name), operation(&after, name));
    }
    assert_eq!(after.r2_live_objects, 0);
    // Metadata itself can reach its independent ceiling, with exact EOF.
    let mut exact = metadata;
    exact.resize(1_500_000, b' ');
    let (status, body) = upload_request(&worker, &upload_frame_bytes(&exact, b"x"));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["published"],
        true
    );
    // A complete 16 MiB payload plus exactly 2,060 bytes framing is legal.
    let data = vec![0x39; 16 * 1024 * 1024];
    let key = format!("chunks/{}", hash(&data));
    let mut full = base;
    full["expected"] = json!(1);
    full["next"]["generation"] = json!(2);
    full["chunk"] = json!({"key":key,"len":data.len()});
    full["next"]["packs"][0]["bundle"] = json!([full["chunk"].clone()]);
    full["next"]["packs"][0]["pack_bytes"] = json!(data.len());
    let mut metadata = serde_json::to_vec(&full).unwrap();
    metadata.resize(2060 - 12, b' ');
    let frame = upload_frame_bytes(&metadata, &data);
    assert_eq!(frame.len(), 16 * 1024 * 1024 + 2060);
    let mut excessive = frame.clone();
    excessive.push(0);
    assert_eq!(upload_request(&worker, &excessive).0, 413);
    let (status, body) = upload_request(&worker, &frame);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(usage(&services).r2_live_objects, 2);
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_fused_retirement_matches_scalar_work_and_removes_three_callbacks() {
    let mut deltas = Vec::new();
    for fused in [false, true] {
        let worker = RealWorker::new(if fused {
            "retire-fused"
        } else {
            "retire-scalar"
        });
        let services = worker.services();
        if fused {
            services.register("http://127.0.0.1:1").unwrap();
            register_legacy_worker(&worker);
        }
        let repository = Repository::new(
            R2BlobStore::new(services.clone()),
            DurableStateStore::new(services.clone()),
        );
        let mut first = Manifest {
            generation: 1,
            ..Manifest::default()
        };
        for i in 0..3 {
            first.packs.push(
                repository
                    .store_bundled_pack(
                        format!("pack-{i}"),
                        format!("pack-{i}").as_bytes(),
                        b"idx",
                        0,
                    )
                    .unwrap(),
            );
        }
        assert!(
            repository
                .publish(&Manifest::default(), first.clone())
                .unwrap()
        );
        let mut next = Manifest {
            generation: 2,
            ..Manifest::default()
        };
        next.retire_replaced(&first);
        assert_eq!(next.garbage.len(), 3);
        let before = usage(&services);
        let RetiringCas::Published(receipt) =
            repository.publish_retiring(&first, next.clone()).unwrap()
        else {
            panic!("CAS did not publish");
        };
        let after = usage(&services);
        let delta = after.checked_workload_delta(&before).unwrap();
        assert_eq!(receipt.generation, 2);
        assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
        assert_eq!(
            receipt
                .outcomes
                .iter()
                .map(|outcome| &outcome.key)
                .collect::<Vec<_>>(),
            next.garbage.iter().collect::<Vec<_>>()
        );
        assert_eq!(after.r2_live_objects, 0);
        assert_eq!(delta.r2_deletes, 3);
        assert_eq!(delta.r2_class_a, 0);
        assert_eq!(delta.r2_class_b, 0);
        assert_eq!(delta.worker_requests, if fused { 1 } else { 4 });
        assert_eq!(delta.do_requests, if fused { 1 } else { 4 });
        assert_eq!(
            operation(&after, "manifest_cas").successes
                - operation(&before, "manifest_cas").successes,
            1
        );
        assert_eq!(
            operation(&after, "r2_delete").successes - operation(&before, "r2_delete").successes,
            3
        );
        assert_eq!(repository.state.load().unwrap(), next);
        deltas.push(delta);
        // A stale CAS reports no retirement and never touches any R2 binding.
        let before = usage(&services);
        assert_eq!(
            repository.publish_retiring(&first, next).unwrap(),
            RetiringCas::Stale
        );
        let after = usage(&services);
        let stale = after.checked_workload_delta(&before).unwrap();
        assert_eq!(stale.r2_deletes, 0);
        assert_eq!(stale.r2_class_a + stale.r2_class_b, 0);
    }
    assert_eq!(deltas[0].do_rows_read, deltas[1].do_rows_read);
    assert_eq!(deltas[0].do_rows_written, deltas[1].do_rows_written);
}

#[test]
#[ignore = "requires local workerd with cloudflare/test/retirement-fault.toml, on a fresh persist-dir"]
fn faulted_fused_retirement_preserves_cas_and_failed_r2_accounting() {
    let worker = RealWorker::new("retire-fault");
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
    register_legacy_worker(&worker);
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    let mut next = Manifest {
        generation: 1,
        ..Manifest::default()
    };
    for data in [
        b"retirement-before".as_slice(),
        b"retirement-failure",
        b"retirement-after",
    ] {
        let key = format!("chunks/{}", hash(data));
        repository.blobs.put(&key, data).unwrap();
        next.garbage.push(key);
    }
    let before = usage(&services);
    let RetiringCas::Published(receipt) = repository
        .publish_retiring(&Manifest::default(), next.clone())
        .unwrap()
    else {
        panic!("CAS did not publish");
    };
    let after = usage(&services);
    assert_eq!(receipt.generation, 1);
    assert_eq!(
        receipt
            .outcomes
            .iter()
            .map(|outcome| outcome.deleted)
            .collect::<Vec<_>>(),
        vec![true, false, true]
    );
    let cas = operation(&after, "manifest_cas");
    assert_eq!(
        cas.successes - operation(&before, "manifest_cas").successes,
        1
    );
    assert_eq!(cas.failures, 0);
    let deletes = operation(&after, "r2_delete");
    assert_eq!(
        deletes.attempts - operation(&before, "r2_delete").attempts,
        3
    );
    assert_eq!(
        deletes.successes - operation(&before, "r2_delete").successes,
        2
    );
    assert_eq!(
        deletes.failures - operation(&before, "r2_delete").failures,
        1
    );
    assert_eq!(after.accounting_complete, Some(true));
    assert_eq!(after.r2_live_objects, 1);
    assert_eq!(after.r2_live_bytes, b"retirement-failure".len() as u64);
    let delta = after.checked_workload_delta(&before).unwrap();
    assert_eq!(delta.worker_requests, 1);
    assert_eq!(delta.do_requests, 1);
    assert_eq!(delta.r2_deletes, 3);
    assert_eq!(repository.state.load().unwrap(), next);
    assert_eq!(
        repository.blobs.get(&next.garbage[1]).unwrap(),
        b"retirement-failure"
    );
    // The real failed counter above is deliberately left in place; existing
    // benchmark validation rejects any failed service operation in a trace.
    assert!(next.collect_garbage(&repository.blobs).is_empty());
    assert_eq!(usage(&services).r2_live_objects, 0);
}

#[test]
#[ignore = "requires real local Wrangler R2 and Durable Object bindings"]
fn real_fused_retirement_validates_before_cas_and_preserves_all_live_representations() {
    let worker = RealWorker::new("retire-validation");
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    let split = repository
        .store_pack("split".into(), b"pack", b"index", 0)
        .unwrap();
    let bundle = repository
        .store_bundled_pack("bundle".into(), b"bundled", b"idx", 0)
        .unwrap();
    let mut next = Manifest {
        generation: 1,
        packs: vec![split, bundle],
        ..Manifest::default()
    };
    next.garbage = next
        .packs
        .iter()
        .flat_map(StoredPack::chunks)
        .map(|c| c.key.clone())
        .collect();
    let base = serde_json::json!({"version":1,"expected":0,"next":next});
    let mut invalid = Vec::new();
    invalid.extend([b"null".to_vec(), b"[]".to_vec()]);
    for (pointer, value) in [
        ("/version", serde_json::json!(2)),
        (
            "/next/garbage",
            serde_json::json!([next.garbage[0], next.garbage[0]]),
        ),
        ("/next/garbage", serde_json::json!(["chunks/../invalid"])),
        (
            "/next/garbage",
            serde_json::json!(
                (0..257)
                    .map(|i| format!("chunks/{i:064x}"))
                    .collect::<Vec<_>>()
            ),
        ),
        ("/next/packs/0/pack/0/len", serde_json::json!(0)),
        ("/next/packs/1/id", serde_json::json!("split")),
        (
            "/next/packs/1/pack",
            serde_json::json!([next.packs[0].pack[0]]),
        ),
    ] {
        let mut changed = base.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        invalid.push(serde_json::to_vec(&changed).unwrap());
    }
    invalid.push(b"{malformed".to_vec());
    invalid.push(vec![b' '; 1_500_001]);
    let before = usage(&services);
    for body in invalid {
        let (status, _) = local_request(
            &worker.origin,
            "POST",
            "/_services/manifest/retire",
            &[
                ("x-hosting-token", &worker.token),
                ("x-hosting-repo", &worker.namespace),
                ("x-hosting-phase", "push"),
            ],
            &body,
        );
        assert!(matches!(status, 400 | 413), "status {status}");
    }
    let after = usage(&services);
    assert_eq!(
        operation(&after, "manifest_cas"),
        operation(&before, "manifest_cas")
    );
    assert_eq!(
        operation(&after, "r2_delete"),
        operation(&before, "r2_delete")
    );
    assert_eq!(repository.state.load().unwrap(), Manifest::default());
    let RetiringCas::Published(receipt) = repository
        .publish_retiring(&Manifest::default(), next.clone())
        .unwrap()
    else {
        panic!("CAS did not publish");
    };
    assert!(receipt.outcomes.is_empty());
    for pack in &next.packs {
        repository.read_pack_files(pack).unwrap();
    }
    let after = usage(&services);
    assert_eq!(after.r2_live_objects, 3);
    assert_eq!(
        operation(&after, "r2_delete"),
        operation(&before, "r2_delete")
    );
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

fn register_legacy_worker(worker: &RealWorker) {
    let (status, body) = local_request(
        &worker.origin,
        "POST",
        "/_services/register",
        &[
            ("x-hosting-token", &worker.token),
            ("x-hosting-repo", &worker.namespace),
        ],
        br#"{"codec_origin":"http://127.0.0.1:1","manifest_retire":1,"manifest_upload_retire":1}"#,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
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
    for generation in 1..=4 {
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
        if generation == 4 {
            assert_eq!(metrics["compactions"], 1);
            assert_eq!(metrics["retired_chunk_deletes"], 3);
            let published = DurableStateStore::new(worker.services()).load().unwrap();
            assert_eq!(published.packs.len(), 1);
            assert_eq!(published.garbage.len(), 3);
            let accounted = usage(&worker.services());
            assert_eq!(operation(&accounted, "r2_delete").successes, 3);
            assert_eq!(operation(&accounted, "r2_delete").failures, 0);
        }
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
        "gateway generation 4\n".repeat(512).as_bytes()
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
fn real_full_cloud_chunk_put_and_bulk_read_charge_one_binding_each() {
    let worker = RealWorker::new("bulk-16mib");
    let services = worker.services();
    let blobs = R2BlobStore::new(services.clone());
    let data = vec![0x63; 16 * 1024 * 1024];
    let chunk = Chunk {
        key: format!("chunks/{}", hash(&data)),
        len: data.len() as u64,
    };
    let before = usage(&services);
    blobs.put(&chunk.key, &data).unwrap();
    let written = usage(&services);
    let put = written
        .workload_counters()
        .unwrap()
        .checked_sub(&before.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            put.worker_service_requests,
            put.do_requests,
            put.r2_class_a,
            put.r2_written_bytes
        ),
        (1, 1, 1, 16 * 1024 * 1024)
    );
    assert_eq!(blobs.get_many(&[chunk]).unwrap(), vec![data]);
    let after = usage(&services);
    let read = after
        .workload_counters()
        .unwrap()
        .checked_sub(&written.workload_counters().unwrap())
        .unwrap();
    assert_eq!(
        (
            read.worker_service_requests,
            read.do_requests,
            read.r2_class_b,
            read.r2_read_bytes
        ),
        (1, 1, 1, 16 * 1024 * 1024)
    );
    assert_eq!(
        (read.r2_class_a, read.r2_deletes, read.r2_written_bytes),
        (0, 0, 0)
    );
    assert_eq!(
        operation(&after, "r2_get").successes - operation(&written, "r2_get").successes,
        1
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
        serde_json::json!({"chunks": [{"key": key, "len": 16 * 1024 * 1024 + 1}]}),
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
fn real_manifest_advertisement_skips_only_certified_discovery_reads() {
    let worker = RealWorker::new("advertisement");
    let services = worker.services();
    let directory = Directory(std::env::temp_dir().join(&worker.namespace));
    let source = directory.0.join("source.git");
    fs::create_dir_all(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    let fixture = native_fixture(&source, 29);
    let root = directory.0.join("host");
    let mut host = NativeHost::open_cloudflare(
        root.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    let mut push = packet(&format!(
        "{} {} refs/heads/main\0report-status atomic\n",
        "0".repeat(40),
        fixture.tip
    ));
    push.extend_from_slice(b"0000");
    push.extend_from_slice(&fixture.pack);
    assert_eq!(host.handle(rpc("receive", push)).status, 200);
    let state = DurableStateStore::new(services.clone());
    let published = state.load().unwrap();
    let expected_gets = published.packs.iter().flat_map(StoredPack::chunks).count() as u64;
    let expected_bytes: u64 = published
        .packs
        .iter()
        .map(|pack| pack.pack_bytes + pack.index_bytes)
        .sum();
    let discovery = Request {
        method: "GET".into(),
        target: "/repo.git/info/refs?service=git-upload-pack".into(),
        headers: BTreeMap::new(),
        body: vec![],
    };
    let before = usage(&services);
    let hydration = host.metrics.cloud_view_rehydrates;
    let native = host.handle(discovery.clone());
    assert_eq!(native.status, 200);
    assert_eq!(host.metrics.cloud_view_rehydrates, hydration + 1);
    assert_eq!(host.metrics.upload_advertisement_type_checks, 1);
    assert_eq!(host.metrics.upload_advertisement_certificates, 1);
    let bootstrap_storage = services.usage().unwrap();
    let bootstrapped: ServiceUsage = serde_json::from_value(bootstrap_storage.clone()).unwrap();
    let delta = bootstrapped.checked_workload_delta(&before).unwrap();
    assert_eq!(
        (delta.r2_class_b, delta.r2_read_bytes),
        (expected_gets, expected_bytes)
    );
    let mut expected = packet("# service=git-upload-pack\n");
    expected.extend_from_slice(b"0000");
    expected.extend(git(
        &root.join("view/repo.git"),
        &["upload-pack", "--stateless-rpc", "--advertise-refs", "."],
        &[],
    ));
    assert_eq!(native.body.as_ref(), expected);
    let mut version_zero = discovery.clone();
    version_zero
        .headers
        .insert("git-protocol".into(), "version=0".into());
    let shortcut = host.handle(version_zero);
    assert_eq!(shortcut.body, native.body);
    assert_eq!(shortcut.headers, native.headers);
    assert_eq!(host.metrics.cloud_view_rehydrates, hydration + 1);
    assert_eq!(host.metrics.upload_advertisement_shortcuts, 1);
    let shortcut_storage = services.usage().unwrap();
    let cheaper: ServiceUsage = serde_json::from_value(shortcut_storage.clone()).unwrap();
    assert!(bootstrap_storage["do_sqlite_bytes"].as_u64().unwrap() > 0);
    for field in ["do_sqlite_bytes", "manifest_bytes", "manifest_generation"] {
        assert_eq!(shortcut_storage[field], bootstrap_storage[field], "{field}");
    }
    let delta = cheaper.checked_workload_delta(&bootstrapped).unwrap();
    assert_eq!(
        (
            delta.r2_class_b,
            delta.r2_read_bytes,
            delta.r2_class_a,
            delta.r2_deletes,
            delta.do_rows_written
        ),
        (0, 0, 0, 0, Some(0))
    );
    assert_eq!(cheaper.r2_live_objects, bootstrapped.r2_live_objects);
    assert_eq!(cheaper.r2_live_bytes, bootstrapped.r2_live_bytes);
    assert_eq!(cheaper.r2_peak_bytes, bootstrapped.r2_peak_bytes);
    // Observers consume the existing full view and shift no removed read.
    for target in ["/metrics", "/_object-stats"] {
        let response = host.handle(Request {
            target: target.into(),
            ..discovery.clone()
        });
        assert_eq!(response.status, 200);
        let metrics: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert!(
            metrics["upload_advertisement_template_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            metrics["cache_retained_bytes"].as_u64().unwrap()
                <= metrics["cache_budget_bytes"].as_u64().unwrap()
        );
    }
    let observed = usage(&services);
    assert_eq!(observed.calls.r2_class_b, cheaper.calls.r2_class_b);
    assert_eq!(observed.calls.r2_read_bytes, cheaper.calls.r2_read_bytes);
    let mut fetch = packet(&format!("want {}\n", fixture.tip));
    fetch.extend_from_slice(b"0000");
    fetch.extend(packet("done\n"));
    let fetched = host.handle(rpc("upload", fetch));
    assert_eq!(fetched.status, 200);
    let offset = fetched
        .body
        .windows(4)
        .position(|bytes| bytes == b"PACK")
        .unwrap();
    assert_eq!(host.metrics.cloud_view_rehydrates, hydration + 2);
    let fetched_usage = usage(&services);
    let delta = fetched_usage.checked_workload_delta(&observed).unwrap();
    assert_eq!(
        (delta.r2_class_b, delta.r2_read_bytes),
        (expected_gets, expected_bytes)
    );
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
        &["update-ref", "refs/heads/main", &fixture.tip],
        &[],
    );
    git(&destination, &["fsck", "--strict"], &[]);
    assert_eq!(
        git(&destination, &["cat-file", "blob", &fixture.blob], &[]),
        fixture.data
    );
    assert_eq!(
        git(&destination, &["archive", &fixture.tip], &[]),
        git(&source, &["archive", &fixture.tip], &[])
    );
    assert_eq!(state.load().unwrap(), published);
    // A validated native branch transaction advances the certificate, clearing
    // response entries while retaining only the bounded policy template.
    let mut alias = packet(&format!(
        "{} {} refs/heads/alias\0report-status atomic\n",
        "0".repeat(40),
        fixture.tip
    ));
    alias.extend_from_slice(b"0000");
    alias.extend_from_slice(&fixture.pack);
    let pushed = host.handle(rpc("receive", alias));
    assert_eq!(pushed.status, 200);
    assert!(
        String::from_utf8_lossy(&pushed.body).contains("ok refs/heads/alias\n"),
        "{}",
        String::from_utf8_lossy(&pushed.body)
    );
    let metrics = host.handle(Request {
        target: "/metrics".into(),
        ..discovery.clone()
    });
    assert_eq!(metrics.status, 200);
    let metrics: serde_json::Value = serde_json::from_slice(&metrics.body).unwrap();
    assert_eq!(metrics["cache_bytes"], 0);
    assert_eq!(metrics["cache_entries"], 0);
    assert_eq!(
        metrics["cache_retained_bytes"],
        metrics["upload_advertisement_template_bytes"]
    );
    let hydrated = host.metrics.cloud_view_rehydrates;
    let after_push = usage(&services);
    let advertisement = host.handle(discovery.clone());
    assert!(String::from_utf8_lossy(&advertisement.body).contains("refs/heads/alias\n"));
    assert_eq!(host.metrics.cloud_view_rehydrates, hydrated);
    assert_eq!(host.metrics.upload_advertisement_certificates, 1);
    let after_read = usage(&services);
    let delta = after_read.checked_workload_delta(&after_push).unwrap();
    assert_eq!(
        (
            delta.r2_class_b,
            delta.r2_read_bytes,
            delta.r2_class_a,
            delta.r2_deletes,
            delta.do_rows_written
        ),
        (0, 0, 0, 0, Some(0))
    );
    let mut external = state.load().unwrap();
    let previous_generation = external.generation;
    external.generation += 1;
    external.refs.remove("refs/heads/alias");
    assert!(
        state
            .compare_and_swap(previous_generation, &external)
            .unwrap()
    );
    let response = host.handle(discovery);
    assert_eq!(response.body, native.body);
    assert_eq!(host.metrics.upload_advertisement_native_bootstraps, 2);
    assert_eq!(host.metrics.upload_advertisement_certificates, 2);
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
    let fixture = native_fixture(&source, 7);
    let object_id = &fixture.tip;
    let packed = &fixture.pack;
    // The old default splits this exact pack/index stream into two chunks.
    let legacy_services =
        git_zero_host::storage::LocalServices::new(directory.0.join("legacy")).unwrap();
    let legacy = Repository::new(legacy_services.blobs, legacy_services.state)
        .store_bundled_pack(fixture.id.clone(), packed, &fixture.index, 0)
        .unwrap();
    assert_eq!(legacy.bundle.len(), 2);
    assert_eq!(legacy.bundle[0].len, 8 * 1024 * 1024);
    let mut body = packet(&format!(
        "0000000000000000000000000000000000000000 {object_id} refs/tags/bundle-fixture\0report-status atomic\n"
    ));
    body.extend_from_slice(b"0000");
    body.extend_from_slice(packed);
    let root = directory.0.join("host");
    let mut host = NativeHost::open_cloudflare(
        root.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    host.cloudflare_services()
        .unwrap()
        .register("http://127.0.0.1:1")
        .unwrap();
    let before: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    let response = host.handle(rpc("receive", body));
    assert_eq!(response.status, 200);
    assert!(String::from_utf8_lossy(&response.body).contains("ok refs/tags/bundle-fixture\n"));
    assert_eq!(host.metrics.blob_puts, 1);
    let uploaded = usage(&services);
    let callbacks = uploaded.checked_workload_delta(&before).unwrap();
    // Direct native dispatch needs one manifest read, then exactly one fused
    // final-upload/publication call. The actual PUT is still present.
    assert_eq!(callbacks.worker_service_requests, 2);
    assert_eq!(callbacks.do_requests, 2);
    let state = DurableStateStore::new(services.clone());
    let published = state.load().unwrap();
    assert_eq!(published.packs.len(), 1);
    let stored = &published.packs[0];
    assert_eq!(stored.bundle.len(), 1);
    assert!(stored.bundle[0].len > 8 * 1024 * 1024);
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
    let view = root.join("view/repo.git");
    assert_eq!(
        fs::read(view.join(format!("objects/pack/pack-{}.pack", stored.id))).unwrap(),
        fixture.pack
    );
    assert_eq!(
        fs::read(view.join(format!("objects/pack/pack-{}.idx", stored.id))).unwrap(),
        fixture.index
    );
    assert_eq!(
        git(&view, &["rev-parse", &format!("{object_id}^{{tree}}")], &[]),
        format!("{}\n", fixture.tree).as_bytes()
    );
    git(&view, &["fsck", "--strict"], &[]);
    assert_eq!(
        git(
            &root.join("view/repo.git"),
            &["cat-file", "blob", &fixture.blob],
            &[]
        ),
        fixture.data
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
        &["update-ref", "refs/tags/bundle-fixture", object_id],
        &[],
    );
    git(&destination, &["fsck", "--strict"], &[]);
    assert_eq!(
        git(&destination, &["cat-file", "blob", &fixture.blob], &[]),
        fixture.data
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
    let legacy = native_fixture(&source.0, 11);
    let current = native_fixture(&source.0, 13);
    let mut repository = repository;
    let old = repository
        .store_bundled_pack(legacy.id.clone(), &legacy.pack, &legacy.index, 0)
        .unwrap();
    assert_eq!(old.bundle.len(), 2);
    assert_eq!(old.bundle[0].len, 8 * 1024 * 1024);
    repository.chunk_bytes = 16 * 1024 * 1024;
    let new = repository
        .store_bundled_pack(current.id.clone(), &current.pack, &current.index, 0)
        .unwrap();
    assert_eq!(new.bundle.len(), 1);
    assert!(new.bundle[0].len > 8 * 1024 * 1024);
    let mut next = Manifest {
        generation: 1,
        refs: BTreeMap::from([
            ("refs/tags/restart-legacy".into(), legacy.tip.clone()),
            ("refs/tags/restart-current".into(), current.tip.clone()),
        ]),
        packs: vec![old, new],
        ..Manifest::default()
    };
    // Simulate interruption after publication and the first scalar retirement.
    // Keep the complete durable debt across a real workerd process restart.
    for data in [
        b"restart-retired-a".as_slice(),
        b"restart-retired-b",
        b"restart-retired-c",
    ] {
        let key = format!("chunks/{}", hash(data));
        repository.blobs.put(&key, data).unwrap();
        next.garbage.push(key);
    }
    assert!(repository.publish(&previous, next.clone()).unwrap());
    repository.blobs.delete(&next.garbage[0]).unwrap();
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 5);
    assert_eq!(
        usage.r2_live_bytes,
        next.packs
            .iter()
            .map(|pack| pack.pack_bytes + pack.index_bytes)
            .sum::<u64>()
            + b"restart-retired-b".len() as u64
            + b"restart-retired-c".len() as u64
    );
    let metadata = |fixture: &NativeFixture, seed| {
        serde_json::json!({
            "seed":seed, "tip":fixture.tip, "blob":fixture.blob, "tree":fixture.tree,
            "pack_hash":hash(&fixture.pack), "index_hash":hash(&fixture.index),
        })
    };
    let marker = serde_json::json!({
        "manifest":next, "fixtures":[metadata(&legacy, 11), metadata(&current, 13)],
        "counter_epoch":usage.counter_epoch,
    });
    fs::write(
        restart_marker(&worker),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "two-phase upload-fusion process-restart proof; run seed, restart Wrangler, then verify"]
fn seed_upload_before_workerd_restart() {
    let worker = restart_fixture();
    let services = worker.services();
    services.register("http://127.0.0.1:1").unwrap();
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
    let legacy = native_fixture(&source.0, 11);
    let current = native_fixture(&source.0, 13);
    let mut repository = repository;
    let old = repository
        .store_bundled_pack(legacy.id.clone(), &legacy.pack, &legacy.index, 0)
        .unwrap();
    assert_eq!(old.bundle.len(), 2);
    assert_eq!(old.bundle[0].len, 8 * 1024 * 1024);
    repository.chunk_bytes = 16 * 1024 * 1024;
    let (new, pending) = repository
        .stage_bundled_pack(current.id.clone(), &current.pack, &current.index, 0)
        .unwrap()
        .into_parts();
    assert_eq!(new.bundle.len(), 1);
    assert!(new.bundle[0].len > 8 * 1024 * 1024);
    let mut next = Manifest {
        generation: 1,
        refs: BTreeMap::from([
            ("refs/tags/restart-legacy".into(), legacy.tip.clone()),
            ("refs/tags/restart-current".into(), current.tip.clone()),
        ]),
        packs: vec![old, new],
        ..Manifest::default()
    };
    let RetiringCas::Published(_) = repository
        .publish_staged_retiring(&previous, next.clone(), Some(pending))
        .unwrap()
    else {
        panic!("upload not published");
    };
    let uploaded = repository.state.load().unwrap();
    next.generation = 2;
    // Simulate interruption after publication and the first scalar retirement.
    // Keep the complete durable debt across a real workerd process restart.
    for data in [
        b"restart-retired-a".as_slice(),
        b"restart-retired-b",
        b"restart-retired-c",
    ] {
        let key = format!("chunks/{}", hash(data));
        repository.blobs.put(&key, data).unwrap();
        next.garbage.push(key);
    }
    next.garbage.push(next.packs[1].bundle[0].key.clone());
    assert!(repository.publish(&uploaded, next.clone()).unwrap());
    let (orphan, pending) = repository
        .stage_bundled_pack("pending-orphan".into(), b"unpublished-tail", b"index", 0)
        .unwrap()
        .into_parts();
    assert_eq!(
        repository
            .publish_staged_retiring(
                &Manifest::default(),
                Manifest {
                    packs: vec![orphan],
                    ..Manifest::default()
                },
                Some(pending)
            )
            .unwrap(),
        RetiringCas::Stale
    );
    repository.blobs.delete(&next.garbage[0]).unwrap();
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 6);
    assert_eq!(
        usage.r2_live_bytes,
        next.packs
            .iter()
            .map(|pack| pack.pack_bytes + pack.index_bytes)
            .sum::<u64>()
            + b"restart-retired-b".len() as u64
            + b"restart-retired-c".len() as u64
            + b"unpublished-tailindex".len() as u64
    );
    let metadata = |fixture: &NativeFixture, seed| {
        serde_json::json!({
            "seed":seed, "tip":fixture.tip, "blob":fixture.blob, "tree":fixture.tree,
            "pack_hash":hash(&fixture.pack), "index_hash":hash(&fixture.index),
        })
    };
    let marker = serde_json::json!({
        "manifest":next, "fixtures":[metadata(&legacy, 11), metadata(&current, 13)],
        "counter_epoch":usage.counter_epoch,
    });
    fs::write(
        restart_marker(&worker),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "two-phase upload-fusion process-restart proof; seed and restart same persist-dir first"]
fn verify_upload_after_workerd_restart() {
    verify_after_workerd_restart();
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
    let services = worker.services();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services.clone()),
    );
    assert_eq!(repository.state.load().unwrap(), expected);
    let restarted: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_ne!(
        serde_json::to_value(&restarted.counter_epoch).unwrap(),
        marker["counter_epoch"],
        "Worker/DO runtime did not restart between the two test phases"
    );
    for (pack, fixture) in expected
        .packs
        .iter()
        .zip(marker["fixtures"].as_array().unwrap())
    {
        let (data, index) = repository.read_pack_files(pack).unwrap();
        assert_eq!(data.len() as u64, pack.pack_bytes);
        assert_eq!(index.len() as u64, pack.index_bytes);
        assert_eq!(hash(&data), fixture["pack_hash"].as_str().unwrap());
        assert_eq!(hash(&index), fixture["index_hash"].as_str().unwrap());
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
    let native = view.0.join("view/repo.git");
    git(&native, &["fsck", "--strict"], &[]);
    for fixture in marker["fixtures"].as_array().unwrap() {
        let tip = fixture["tip"].as_str().unwrap();
        assert_eq!(
            git(
                &native,
                &["cat-file", "blob", fixture["blob"].as_str().unwrap()],
                &[]
            ),
            fixture_data(fixture["seed"].as_u64().unwrap() as u32)
        );
        assert_eq!(
            git(&native, &["rev-parse", &format!("{tip}^{{tree}}")], &[]),
            format!("{}\n", fixture["tree"].as_str().unwrap()).as_bytes()
        );
    }
    drop(host);
    // The new layout's chunk is shared by consecutive manifest generations.
    // Retiring the legacy layout must preserve that live chunk and its objects.
    let mut retained = expected.clone();
    retained.packs.remove(0);
    retained.refs.remove("refs/tags/restart-legacy");
    retained.retire_replaced(&expected);
    // Startup GC reconciled the interrupted debt; durable debt remains complete
    // and the fused request idempotently confirms its already-absent keys too.
    assert_eq!(retained.garbage.len(), 5);
    let shared = retained.packs[0].bundle[0].clone();
    assert!(!retained.garbage.contains(&shared.key));
    services.register("http://127.0.0.1:1").unwrap();
    let RetiringCas::Published(receipt) = repository.publish_retiring(&expected, retained).unwrap()
    else {
        panic!("fused retirement failed after restart");
    };
    assert_eq!(receipt.outcomes.len(), 5);
    assert!(receipt.outcomes.iter().all(|outcome| outcome.deleted));
    let retained = repository.state.load().unwrap();
    let remaining = usage(&services);
    assert_eq!(remaining.r2_live_objects, 1);
    assert_eq!(remaining.r2_live_bytes, shared.len);
    let host = NativeHost::open_cloudflare(
        view.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        true,
    )
    .unwrap();
    let current = &marker["fixtures"][1];
    assert_eq!(
        git(
            &native,
            &["cat-file", "blob", current["blob"].as_str().unwrap()],
            &[]
        ),
        fixture_data(13)
    );
    git(&native, &["fsck", "--strict"], &[]);
    drop(host);
    let mut empty = Manifest::default();
    empty.retire_replaced(&retained);
    assert!(repository.publish(&retained, empty).unwrap());
    let retired = repository.state.load().unwrap();
    assert!(retired.collect_garbage(&repository.blobs).is_empty());
    assert_eq!(usage(&services).r2_live_objects, 0);
    fs::remove_file(restart_marker(&worker)).unwrap();
}
