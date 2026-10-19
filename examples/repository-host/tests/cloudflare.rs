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
    accounting::ServiceUsage,
    cloudflare::HttpServices,
    hash,
    native::{NativeHost, command},
    storage::{BlobStore, DurableStateStore, Manifest, R2BlobStore, StateStore},
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
#[ignore = "requires real local Wrangler/workerd; see module-level invocation"]
fn real_large_ref_snapshot_matches_native_limits() {
    let worker = RealWorker::new("large-refs");
    let services = worker.services();
    let repository = Repository::new(
        R2BlobStore::new(services.clone()),
        DurableStateStore::new(services),
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
        b"large ref snapshot fixture\n",
    ))
    .unwrap()
    .trim()
    .to_owned();
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
    let stored = repository.store_pack(id.into(), &data, &index, 0).unwrap();
    let next = Manifest {
        generation: 1,
        refs: (0..16_000)
            .map(|number| {
                (
                    format!("refs/tags/{number:05}-{}", "r".repeat(44)),
                    object_id.clone(),
                )
            })
            .collect(),
        packs: vec![stored],
        ..Manifest::default()
    };
    assert!(serde_json::to_vec(&next).unwrap().len() > 1_500_000);
    assert!(
        repository
            .publish(&Manifest::default(), next.clone())
            .unwrap()
    );
    assert!(repository.state.load().unwrap() == next);

    // Reopening through the native host also validates its ref count/byte
    // limits and reconstructs the real pack behind all of these tag refs.
    let root = Directory(std::env::temp_dir().join(format!("{}-host", worker.namespace)));
    let host = NativeHost::open_cloudflare(
        root.0.clone(),
        &worker.origin,
        &worker.namespace,
        &worker.token,
        false,
    )
    .unwrap();
    assert!(git_zero_host::native::refs(&root.0.join("view/repo.git")).unwrap() == next.refs);
    drop(host);
    let mut empty = Manifest::default();
    empty.retire_replaced(&next);
    assert!(repository.publish(&next, empty.clone()).unwrap());
    assert!(empty.collect_garbage(&repository.blobs).is_empty());
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
    let stored = repository.store_pack(id.into(), &data, &index, 0).unwrap();
    let next = Manifest {
        generation: 1,
        refs: BTreeMap::from([("refs/tags/restart-fixture".into(), object_id.clone())]),
        packs: vec![stored],
        ..Manifest::default()
    };
    assert!(repository.publish(&previous, next.clone()).unwrap());
    let usage: ServiceUsage = serde_json::from_value(services.usage().unwrap()).unwrap();
    assert_eq!(usage.r2_live_objects, 2);
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
        assert_eq!(
            repository
                .read_chunks(&pack.pack, pack.pack_bytes)
                .unwrap()
                .len() as u64,
            pack.pack_bytes
        );
        assert_eq!(
            repository
                .read_chunks(&pack.index, pack.index_bytes)
                .unwrap()
                .len() as u64,
            pack.index_bytes
        );
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
