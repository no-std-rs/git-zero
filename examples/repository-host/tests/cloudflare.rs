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
    let too_large = serde_json::json!({"chunks": [{"key": key, "len": 8 * 1024 * 1024}, {"key": key, "len": 8 * 1024 * 1024}]});
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
    // Invalid chunks within the cap and an oversized response budget reach it.
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
