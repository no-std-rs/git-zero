use super::advertisement_tests::{Fixture, discovery, git, packet, text};
use super::*;

fn commit(fixture: &Fixture, parent: &str, payload: &[u8]) -> String {
    let blob = text(&fixture.source, &["hash-object", "-w", "--stdin"], payload);
    let tree = text(
        &fixture.source,
        &["mktree"],
        format!("100644 blob {blob}\tpayload\n").as_bytes(),
    );
    text(
        &fixture.source,
        &[
            "-c",
            "user.name=Contract",
            "-c",
            "user.email=contract@example.invalid",
            "commit-tree",
            &tree,
            "-p",
            parent,
        ],
        b"next\n",
    )
}

fn fetch(id: &str, have: Option<&str>) -> Request {
    let mut body = packet(format!("want {id} ofs-delta\n").as_bytes());
    body.extend_from_slice(b"0000");
    if let Some(have) = have {
        body.extend(packet(format!("have {have}\n").as_bytes()));
    }
    body.extend(packet(b"done\n"));
    Request {
        method: "POST".into(),
        target: "/repo.git/git-upload-pack".into(),
        headers: BTreeMap::from([(
            "content-type".into(),
            "application/x-git-upload-pack-request".into(),
        )]),
        body,
    }
}

fn prepared() -> (Fixture, NativeHost, String) {
    let fixture = Fixture::new();
    let mut host = fixture.host();
    assert_eq!(host.handle(discovery()).status, 200);
    assert!(host.lineage.is_some());
    let generation = host.manifest.generation;
    let probe = Request {
        method: "POST".into(),
        target: "/repo.git/git-receive-pack".into(),
        headers: BTreeMap::from([(
            "content-type".into(),
            "application/x-git-receive-pack-request".into(),
        )]),
        body: b"0000".to_vec(),
    };
    assert_eq!(host.handle(probe).status, 200);
    assert!(host.lineage.is_some());
    assert_eq!(host.manifest.generation, generation);
    assert_eq!(
        host.handle(fixture.push(&"0".repeat(40), &fixture.tip, "refs/heads/old"))
            .status,
        200
    );
    let next = commit(&fixture, &fixture.tip, b"second revision\n");
    let pushed = host.handle(fixture.push(&fixture.tip, &next, "refs/heads/main"));
    assert_eq!(pushed.status, 200);
    assert!(host.lineage.is_some());
    assert_eq!(host.manifest.packs.len(), 2);
    (fixture, host, next)
}

#[test]
fn cold_selection_cache_hit_metrics_and_full_inventory_restore_keep_scope_honest() {
    let (fixture, mut host, latest) = prepared();
    let latest_request = fetch(&latest, None);
    assert_eq!(host.handle(latest_request.clone()).status, 200);
    let response = host.handle(fetch(&fixture.tip, Some(&fixture.tip)));
    assert_eq!(response.status, 200);
    assert!(matches!(host.scope, ViewScope::Projection { packs: 1, .. }));
    assert_eq!(
        refs(&host.view).unwrap(),
        BTreeMap::from([("refs/heads/old".into(), fixture.tip.clone())])
    );
    let rehydrates = host.metrics.cloud_view_rehydrates;
    assert_eq!(host.handle(latest_request).status, 200);
    assert_eq!(host.metrics.cloud_view_rehydrates, rehydrates);
    assert!(matches!(host.scope, ViewScope::Projection { .. }));
    let cloud = host.cloudflare.take();
    let metrics = host.handle(Request {
        method: "GET".into(),
        target: "/metrics".into(),
        headers: BTreeMap::new(),
        body: vec![],
    });
    assert_eq!(metrics.status, 200);
    let metrics: serde_json::Value = serde_json::from_slice(&metrics.body).unwrap();
    assert_eq!(metrics["native_view"]["scope"], "projection");
    assert_eq!(host.metrics.cloud_view_rehydrates, rehydrates);
    assert!(host.lineage_bytes() > 0);
    assert_eq!(
        metrics["cache_retained_bytes"].as_u64().unwrap(),
        (host.cache_bytes + host.metadata_bytes()) as u64
    );
    host.cloudflare = cloud;
    // Inventory's normal services observer cannot contact the fixture's fake
    // network endpoint, but full reconstruction and strict inventory run first.
    let stats = host.handle(Request {
        method: "GET".into(),
        target: "/_object-stats".into(),
        headers: BTreeMap::new(),
        body: vec![],
    });
    assert_eq!(stats.status, 503);
    assert!(matches!(host.scope, ViewScope::Full { .. }));
    assert_eq!(host.metrics.cloud_view_rehydrates, rehydrates + 1);
    assert_eq!(host.metrics.history_full_restores, 1);
    assert_eq!(refs(&host.view).unwrap(), host.manifest.refs);
    assert!(git(&host.view, &["fsck", "--strict", "--full"], &[]).is_empty());
}

#[test]
fn selected_missing_or_corrupt_chunks_fail_without_full_retry_and_omitted_metadata_is_validated() {
    for corrupt in [false, true] {
        let (fixture, mut host, _) = prepared();
        let key = host.manifest.packs[0].chunks().next().unwrap().key.clone();
        if corrupt {
            // Bypass the immutable public put contract to simulate disk damage.
            host.repository.blobs.delete(&key).unwrap();
            fs::write(host.root.join("services/blobs").join(&key), b"corrupt").unwrap();
        } else {
            host.repository.blobs.delete(&key).unwrap();
        }
        let before = host.metrics.cloud_view_rehydrates;
        assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 503);
        assert!(matches!(host.scope, ViewScope::Invalid));
        assert!(host.view_dirty);
        assert!(host.lineage.is_none());
        assert_eq!(host.metrics.cloud_view_rehydrates, before);
        assert!(!host.view.join("objects/pack").exists());
    }
    let (fixture, mut host, _) = prepared();
    let valid = host.manifest.clone();
    for variant in 0..7 {
        host.manifest = valid.clone();
        match variant {
            0 => host.manifest.packs[1].pack_bytes += 1,
            1 => host.manifest.packs[1].bundle[0].key = "chunks/../bad".into(),
            2 => host.manifest.packs[1].pack = host.manifest.packs[1].bundle.clone(),
            3 => host.manifest.packs[1].id = host.manifest.packs[0].id.clone(),
            4 => host.manifest.packs[1].bundle[0].key = host.manifest.packs[0].pack[0].key.clone(),
            5 => {
                host.manifest
                    .refs
                    .insert("refs/heads/bad..name".into(), fixture.tip.clone());
            }
            6 => host.manifest.packs[1].index_bytes = u64::MAX,
            _ => unreachable!(),
        }
        assert!(
            host.fetch_read_view(&fetch(&fixture.tip, None)).is_err(),
            "{variant}"
        );
    }
}

#[test]
fn partial_to_ref_only_push_unknown_rpc_and_native_discovery_establish_full_once() {
    let (fixture, mut host, latest) = prepared();
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    assert!(matches!(host.scope, ViewScope::Projection { .. }));
    let reads = host.metrics.cloud_view_rehydrates;
    assert_eq!(
        host.handle(fixture.push(&"0".repeat(40), &latest, "refs/heads/alias"))
            .status,
        200
    );
    assert_eq!(host.metrics.cloud_view_rehydrates, reads + 1);
    assert!(matches!(host.scope, ViewScope::Full { .. }));
    assert_eq!(host.cache_bytes, 0);
    assert!(host.lineage.is_some());
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    let reads = host.metrics.cloud_view_rehydrates;
    let unknown = Request {
        method: "GET".into(),
        target: "/repo.git/unknown".into(),
        headers: BTreeMap::new(),
        body: vec![],
    };
    assert_eq!(host.handle(unknown).status, 404);
    assert_eq!(host.metrics.cloud_view_rehydrates, reads + 1);
    assert!(matches!(host.scope, ViewScope::Full { .. }));
    assert_eq!(
        host.handle(fetch(&fixture.tip, Some(&fixture.tip))).status,
        200
    );
    let reads = host.metrics.cloud_view_rehydrates;
    let mut unsupported = discovery();
    unsupported
        .headers
        .insert("git-protocol".into(), "version=2".into());
    assert_eq!(host.handle(unsupported).status, 200);
    assert_eq!(host.metrics.cloud_view_rehydrates, reads + 1);
    assert!(matches!(host.scope, ViewScope::Full { .. }));
}

#[test]
fn full_reads_preserve_lineage_external_or_dirty_recovery_adopts_conservative_floors() {
    let (fixture, mut host, latest) = prepared();
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    assert!(matches!(host.scope, ViewScope::Projection { .. }));
    host.cloud_read_view().unwrap();
    host.cache.clear();
    host.cache_bytes = 0;
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    assert!(matches!(host.scope, ViewScope::Projection { .. }));
    let mut external = host.manifest.clone();
    external.generation += 1;
    external.refs.insert("refs/heads/external".into(), latest);
    assert!(
        host.repository
            .state
            .compare_and_swap(host.manifest.generation, &external)
            .unwrap()
    );
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    assert!(matches!(host.scope, ViewScope::Full { .. }));
    assert_eq!(host.metrics.history_projections, 2);
    host.view_dirty = true;
    assert_eq!(
        host.handle(fetch(&fixture.tip, Some(&fixture.tip))).status,
        200
    );
    assert!(matches!(host.scope, ViewScope::Full { .. }));
    // Restart is independently conservative and never invents old floors.
    host.cloudflare = None;
    let reopened = NativeHost::open(host.root.clone()).unwrap();
    assert!(reopened.lineage.is_none());
    assert!(matches!(reopened.scope, ViewScope::Full { .. }));
}

#[test]
fn real_compaction_propagates_minimum_across_duplicates_and_multilevel_plans() {
    let fixture = Fixture::new();
    let mut host = NativeHost::open(fixture.root.join("compaction")).unwrap();
    let pack = git(
        &fixture.source,
        &["pack-objects", "--stdout", "--revs"],
        format!("{}\n", fixture.tip).as_bytes(),
    );
    let stored = host
        .repository
        .store_pack("fixture".into(), &pack, &[], 0)
        .unwrap();
    let directory = fixture.root.join("merge");
    init(&directory).unwrap();
    let path = directory.join("objects/pack/input.pack");
    fs::write(&path, pack).unwrap();
    let mut process = Command::new("git");
    process.arg("index-pack").arg(&path);
    command(process, &[]).unwrap();
    let make = |level, floor| CandidatePack {
        stored: Some(stored.clone()),
        id: "same".into(),
        level,
        pack: path.clone(),
        index: path.with_extension("idx"),
        bytes: 100,
        floor,
    };
    let mut packs = vec![make(0, 9), make(0, 2), make(0, 8), make(0, 10)];
    host.compact(&directory, &mut packs).unwrap();
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].floor, 2);
    assert_eq!(packs[0].level, 1);
    // The second generation of compaction must preserve the earlier floor,
    // including a duplicate-only input whose output bytes need no new object.
    packs.extend([make(1, 20), make(1, 1), make(1, 24)]);
    host.compact(&directory, &mut packs).unwrap();
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].floor, 1);
    assert_eq!(packs[0].level, 2);
}

#[test]
fn joint_budget_charges_exact_retained_clones_capacities_and_pending_proofs() {
    let (_fixture, mut host, _) = prepared();
    host.cache = VecDeque::new();
    host.cache_bytes = 0;
    let mut header = String::with_capacity(64 * 1024);
    header.push_str("native");
    let response = Response {
        status: 200,
        headers: BTreeMap::from([("Content-Type".into(), header)]),
        body: vec![1; 14 * 1024 * 1024].into(),
    };
    for name in ["a", "b", "c"] {
        let mut key = String::with_capacity(4096);
        key.push_str(name);
        host.cache_insert(key, &response);
        assert!(host.cache_bytes + host.metadata_bytes() <= CACHE_BYTES);
        assert_eq!(
            host.cache_bytes,
            host.cache
                .iter()
                .map(|(key, value)| NativeHost::cache_weight(key.capacity(), value))
                .sum::<usize>()
        );
    }
    assert_eq!(host.cache.len(), 2);
    assert!(host.metrics.cache_evictions > 0);
    host.pending_lineage_bytes = history::Certificate::allocation_bound(4000, 1).unwrap();
    host.reserve_metadata(32 * 1024);
    assert!(host.cache_bytes + host.metadata_bytes() + 32 * 1024 <= CACHE_BYTES);
    host.pending_lineage_bytes = 0;
    host.trim_cache(CACHE_BYTES);
    assert!(host.cache.is_empty());
    assert_eq!(host.cache_bytes, 0);
    assert!(host.metadata_bytes() > host.lineage_bytes() + host.advertisement_bytes());
    assert!(host.lineage.is_some());
    let old: Metrics = serde_json::from_value(serde_json::json!({"cache_bytes":0})).unwrap();
    assert_eq!(old.history_lineage_bytes, 0);
    assert_eq!(old.history_full_restores, 0);
}

fn noop() -> Request {
    Request {
        method: "POST".into(),
        target: "/repo.git/git-receive-pack".into(),
        headers: BTreeMap::from([(
            "content-type".into(),
            "application/x-git-receive-pack-request".into(),
        )]),
        body: b"0000".to_vec(),
    }
}

#[test]
fn exact_noop_is_native_equivalent_for_empty_full_and_partial_views() {
    let fixture = Fixture::new();
    let empty = fixture.root.join("empty.git");
    init(&empty).unwrap();
    let request = noop();
    let expected = git_rpc(&empty, &request).unwrap();
    assert_eq!(expected.status, 200);
    assert!(expected.body.is_empty());
    assert_eq!(
        expected.headers,
        BTreeMap::from([(
            "Content-Type".into(),
            "application/x-git-receive-pack-result".into()
        )])
    );
    let mut unborn = NativeHost::open(fixture.root.join("unborn-host")).unwrap();
    unborn.cloudflare = Some(Arc::new(
        HttpServices::new("http://127.0.0.1:1", "demo", "token").unwrap(),
    ));
    unborn.cold_view = true;
    let response = unborn.handle(request.clone());
    assert_eq!(response.body, expected.body);
    assert_eq!(response.headers, expected.headers);
    assert_eq!(unborn.metrics.cloud_view_rehydrates, 0);
    assert_eq!(unborn.metrics.receive_noop_shortcuts, 1);
    let (fixture, mut host, _) = prepared();
    for projected in [false, true] {
        if projected {
            assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
        }
        let native = git_rpc(&host.view, &request).unwrap();
        let hydrated = host.metrics.cloud_view_rehydrates;
        let generation = host.manifest.generation;
        let lineage_bytes = host.lineage_bytes();
        let ad_bytes = host.advertisement_bytes();
        let cache_bytes = host.cache_bytes;
        let was_partial = matches!(host.scope, ViewScope::Projection { .. });
        let response = host.handle(request.clone());
        assert_eq!(response.status, native.status);
        assert_eq!(response.body, native.body);
        assert_eq!(response.headers, native.headers);
        assert_eq!(response.body, expected.body);
        assert_eq!(response.headers, expected.headers);
        assert_eq!(host.metrics.cloud_view_rehydrates, hydrated);
        assert_eq!(host.manifest.generation, generation);
        assert_eq!(host.lineage_bytes(), lineage_bytes);
        assert_eq!(host.advertisement_bytes(), ad_bytes);
        assert_eq!(host.cache_bytes, cache_bytes);
        assert_eq!(
            matches!(host.scope, ViewScope::Projection { .. }),
            was_partial
        );
    }
}

#[test]
fn noop_requires_complete_valid_metadata_and_unsupported_policy_keeps_full_native_path() {
    let (fixture, mut host, _) = prepared();
    assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
    let valid = host.manifest.clone();
    for variant in 0..5 {
        host.manifest = valid.clone();
        match variant {
            0 => host.manifest.packs[1].pack_bytes += 1,
            1 => host.manifest.packs[1].bundle[0].key = "chunks/../invalid".into(),
            2 => host.manifest.packs[1].pack = host.manifest.packs[1].bundle.clone(),
            3 => {
                host.manifest
                    .refs
                    .insert("refs/heads/bad..name".into(), fixture.tip.clone());
            }
            4 => host.manifest.packs[1].index_bytes = u64::MAX,
            _ => unreachable!(),
        }
        let shortcuts = host.metrics.receive_noop_shortcuts;
        let hydrated = host.metrics.cloud_view_rehydrates;
        assert!(host.receive(&noop()).is_err());
        assert_eq!(host.metrics.receive_noop_shortcuts, shortcuts);
        assert_eq!(host.metrics.cloud_view_rehydrates, hydrated);
    }
    host.manifest = valid;
    for protocol in ["version=1", "version=2", "version=0:extra"] {
        let mut request = noop();
        request
            .headers
            .insert("git-protocol".into(), protocol.into());
        let hydrated = host.metrics.cloud_view_rehydrates;
        let shortcuts = host.metrics.receive_noop_shortcuts;
        assert_eq!(host.handle(request).status, 200);
        assert_eq!(host.metrics.cloud_view_rehydrates, hydrated + 1);
        assert_eq!(host.metrics.receive_noop_shortcuts, shortcuts);
        assert!(matches!(host.scope, ViewScope::Full { .. }));
    }
    for (name, value) in [("transfer-encoding", "chunked"), ("content-length", "5")] {
        let mut request = noop();
        request.headers.insert(name.into(), value.into());
        let hydrated = host.metrics.cloud_view_rehydrates;
        let shortcuts = host.metrics.receive_noop_shortcuts;
        assert_eq!(host.handle(request).status, 200);
        assert_eq!(host.metrics.cloud_view_rehydrates, hydrated + 1);
        assert_eq!(host.metrics.receive_noop_shortcuts, shortcuts);
    }
    let hydrated = host.metrics.cloud_view_rehydrates;
    let shortcuts = host.metrics.receive_noop_shortcuts;
    fs::write(
        host.view.join("config"),
        [NATIVE_CONFIG, b"[receive]\ndenyNonFastForwards = false\n"].concat(),
    )
    .unwrap();
    assert_eq!(host.handle(noop()).status, 200);
    assert_eq!(host.metrics.cloud_view_rehydrates, hydrated + 1);
    assert_eq!(host.metrics.receive_noop_shortcuts, shortcuts);
}

#[test]
fn noop_identity_change_uses_full_fallback_and_discards_each_stale_certificate() {
    for stale_lineage in [false, true] {
        let (fixture, mut host, _) = prepared();
        assert_eq!(host.handle(fetch(&fixture.tip, None)).status, 200);
        assert!(matches!(host.scope, ViewScope::Projection { .. }));
        assert!(host.lineage.is_some());
        assert!(host.advertisement.is_some());
        // Construct a valid metadata certificate under a DIFFERENT private test
        // identity. This exercises identity drift without racing global env.
        if stale_lineage {
            host.lineage = history::Certificate::bootstrap(&host.manifest, [9; 32]);
        } else {
            host.advertisement = advertisement::Certificate::learn(
                &host.manifest,
                &fixture.native(&host.manifest),
                [9; 32],
            );
        }
        let hydrated = host.metrics.cloud_view_rehydrates;
        let shortcuts = host.metrics.receive_noop_shortcuts;
        let expected = git_rpc(&host.view, &noop()).unwrap();
        let response = host.handle(noop());
        assert_eq!(response.status, expected.status);
        assert_eq!(response.body, expected.body);
        assert_eq!(response.headers, expected.headers);
        assert_eq!(host.metrics.receive_noop_shortcuts, shortcuts);
        assert_eq!(host.metrics.cloud_view_rehydrates, hydrated + 1);
        assert!(matches!(host.scope, ViewScope::Full { .. }));
        if stale_lineage {
            assert!(host.lineage.is_none());
        } else {
            assert!(host.advertisement.is_none());
        }
        assert_eq!(host.pending_lineage_bytes, 0);
        assert_eq!(host.pending_advertisement_bytes, 0);
    }
}
