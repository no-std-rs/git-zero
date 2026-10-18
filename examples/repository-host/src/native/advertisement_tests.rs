use super::*;
use git_zero_core::pktline::{self, Packet};

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) source: PathBuf,
    pub(super) tip: String,
    pub(super) blob: String,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "git-zero-advertisement-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source.git");
        init(&source).unwrap();
        let blob = text(
            &source,
            &["hash-object", "-w", "--stdin"],
            b"advertisement fixture\n",
        );
        let tree = text(
            &source,
            &["mktree"],
            format!("100644 blob {blob}\tpayload\n").as_bytes(),
        );
        let tip = text(
            &source,
            &[
                "-c",
                "user.name=Contract",
                "-c",
                "user.email=contract@example.invalid",
                "commit-tree",
                &tree,
            ],
            b"fixture\n",
        );
        Self {
            root,
            source,
            tip,
            blob,
        }
    }

    pub(super) fn manifest(&self) -> Manifest {
        Manifest {
            generation: 1,
            // main is deliberately neither first nor last; aliases share OIDs.
            refs: ["z", "main", "A", "nested/alias"]
                .into_iter()
                .map(|name| (format!("refs/heads/{name}"), self.tip.to_ascii_uppercase()))
                .collect(),
            ..Manifest::default()
        }
    }

    pub(super) fn native(&self, manifest: &Manifest) -> Response {
        write_refs(&self.source, &manifest.refs).unwrap();
        git_rpc(&self.source, &discovery()).unwrap()
    }

    pub(super) fn push(&self, old: &str, new: &str, name: &str) -> Request {
        let mut body =
            packet(format!("{old} {new} {name}\0report-status atomic ofs-delta\n").as_bytes());
        body.extend_from_slice(b"0000");
        if new != "0".repeat(40) {
            let mut revisions = format!("{new}\n");
            if old != "0".repeat(40) {
                revisions.push_str(&format!("^{old}\n"));
            }
            body.extend(git(
                &self.source,
                &[
                    "pack-objects",
                    "--stdout",
                    "--revs",
                    "--thin",
                    "--delta-base-offset",
                ],
                revisions.as_bytes(),
            ));
        }
        Request {
            method: "POST".into(),
            target: "/repo.git/git-receive-pack".into(),
            headers: BTreeMap::from([(
                "content-type".into(),
                "application/x-git-receive-pack-request".into(),
            )]),
            body,
        }
    }

    pub(super) fn host(&self) -> NativeHost {
        let mut host = NativeHost::open(self.root.join("host")).unwrap();
        assert_eq!(
            host.handle(self.push(&"0".repeat(40), &self.tip, "refs/heads/main"))
                .status,
            200
        );
        // Retain real local authoritative storage, but exercise cold dispatch.
        // This service is never called: phase/witness checks are process-local.
        host.cloudflare = Some(Arc::new(
            HttpServices::new("http://127.0.0.1:1", "demo", "token").unwrap(),
        ));
        host.cold_view = true;
        host
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn git(source: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(source).args(args);
    command(process, input).unwrap().stdout
}

pub(super) fn text(source: &Path, args: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(source, args, input))
        .unwrap()
        .trim()
        .into()
}

pub(super) fn discovery() -> Request {
    Request {
        method: "GET".into(),
        target: "/repo.git/info/refs?service=git-upload-pack".into(),
        headers: BTreeMap::new(),
        body: vec![],
    }
}

pub(super) fn packet(bytes: &[u8]) -> Vec<u8> {
    let mut body = format!("{:04x}", bytes.len() + 4).into_bytes();
    body.extend_from_slice(bytes);
    body
}

fn frames(response: &Response) -> Vec<Vec<u8>> {
    let mut input = response.body.as_ref();
    let mut frames = Vec::new();
    while let Some(frame) = pktline::read(&mut input).unwrap() {
        frames.push(match frame {
            Packet::Data(data) => data.to_vec(),
            Packet::Flush => vec![],
        });
    }
    frames
}

fn framed(response: &Response, frames: &[Vec<u8>]) -> Response {
    let body: Vec<_> = frames
        .iter()
        .flat_map(|frame| {
            if frame.is_empty() {
                b"0000".to_vec()
            } else {
                packet(frame)
            }
        })
        .collect();
    Response {
        body: body.into(),
        ..response.clone()
    }
}

#[test]
fn native_advertisement_matches_exact_branch_order_aliases_and_lowercase_ids() {
    let fixture = Fixture::new();
    for names in [vec!["main"], vec!["z", "main", "A", "nested/alias"]] {
        let mut manifest = fixture.manifest();
        manifest.refs.retain(|name, _| {
            names
                .iter()
                .any(|short| name == &format!("refs/heads/{short}"))
        });
        let native = fixture.native(&manifest);
        let certificate = advertisement::Certificate::learn(&manifest, &native, [1; 32]).unwrap();
        let encoded = certificate.encode(&manifest).unwrap();
        assert_eq!(encoded.body, native.body);
        assert_eq!(encoded.headers, native.headers);
        assert!(advertisement::branch_commits(&fixture.source, &manifest));
        assert!(certificate.weight() <= 1280);
    }
}

#[test]
fn actual_native_agent_is_learned_and_policy_extensions_are_rejected() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    let native = fixture.native(&manifest);
    let mut process = Command::new("git");
    process
        .args(["upload-pack", "--stateless-rpc", "--advertise-refs"])
        .arg(&fixture.source)
        .env("GIT_USER_AGENT", "custom-native-agent/contract");
    let mut body = packet(b"# service=git-upload-pack\n");
    body.extend_from_slice(b"0000");
    body.extend(command(process, &[]).unwrap().stdout);
    let custom = Response {
        body: body.into(),
        ..native.clone()
    };
    let certificate = advertisement::Certificate::learn(&manifest, &custom, [1; 32]).unwrap();
    assert_eq!(certificate.encode(&manifest).unwrap().body, custom.body);
    assert!(String::from_utf8_lossy(&custom.body).contains("agent=custom-native-agent/contract"));
    let original = frames(&native);
    for capability in [
        "session-id=fresh",
        "filter",
        "allow-tip-sha1-in-want",
        "allow-any-sha1-in-want",
        "unknown",
    ] {
        let mut changed = original.clone();
        let head = &mut changed[2];
        head.pop();
        head.extend_from_slice(format!(" {capability}\n").as_bytes());
        assert!(
            advertisement::Certificate::learn(&manifest, &framed(&native, &changed), [1; 32])
                .is_none()
        );
    }
    let mut changed = original;
    let head = String::from_utf8(changed[2].clone())
        .unwrap()
        .replace("multi_ack thin-pack", "thin-pack multi_ack");
    changed[2] = head.into_bytes();
    assert!(
        advertisement::Certificate::learn(&manifest, &framed(&native, &changed), [1; 32]).is_none()
    );
}

#[test]
fn entire_native_framing_ref_set_and_response_headers_must_match() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    let native = fixture.native(&manifest);
    let original = frames(&native);
    for variant in 0..10 {
        let mut changed = original.clone();
        match variant {
            0 => {
                changed.remove(3);
            }
            1 => {
                changed.insert(3, changed[3].clone());
            }
            2 => changed.swap(3, 4),
            3 => changed[2][0] = b'0',
            4 => {
                changed[2] = changed[2]
                    .iter()
                    .copied()
                    .filter(|byte| *byte != 0)
                    .collect()
            }
            5 => changed[0] = b"# service=git-receive-pack\n".to_vec(),
            6 => {
                changed.insert(
                    3,
                    format!("{} refs/heads/main^{{}}\n", fixture.tip).into_bytes(),
                );
            }
            7 => {
                changed.insert(3, format!("shallow {}\n", fixture.tip).into_bytes());
            }
            8 => {
                changed.push(vec![]);
            }
            9 => {
                changed.pop();
            }
            _ => unreachable!(),
        }
        assert!(
            advertisement::Certificate::learn(&manifest, &framed(&native, &changed), [1; 32])
                .is_none(),
            "variant {variant}"
        );
    }
    for body in [
        b"zzzz".to_vec(),
        native.body[..native.body.len() - 1].to_vec(),
        [native.body.as_ref(), b"trailing"].concat(),
    ] {
        assert!(
            advertisement::Certificate::learn(
                &manifest,
                &Response {
                    body: body.into(),
                    ..native.clone()
                },
                [1; 32]
            )
            .is_none()
        );
    }
    let mut headers = native.clone();
    headers.headers.insert("Extra".into(), "policy".into());
    assert!(advertisement::Certificate::learn(&manifest, &headers, [1; 32]).is_none());
}

#[test]
fn unsupported_requests_refs_and_policy_identity_use_native_path() {
    let fixture = Fixture::new();
    let manifest = fixture.manifest();
    let native = fixture.native(&manifest);
    for refs in [
        BTreeMap::new(),
        BTreeMap::from([("refs/heads/other".into(), fixture.tip.clone())]),
        BTreeMap::from([
            ("refs/heads/main".into(), fixture.tip.clone()),
            ("refs/tags/legacy".into(), fixture.tip.clone()),
        ]),
    ] {
        assert!(
            advertisement::Certificate::learn(
                &Manifest {
                    refs,
                    ..manifest.clone()
                },
                &native,
                [1; 32]
            )
            .is_none()
        );
    }
    assert!(advertisement::eligible_request(&discovery()));
    for (header, value) in [
        ("git-protocol", "version=1"),
        ("git-protocol", "version=2"),
        ("git-protocol", "version=0:extra"),
        ("content-type", "application/x-git-upload-pack-request"),
        ("content-encoding", "gzip"),
        ("transfer-encoding", "chunked"),
        ("content-length", "1"),
    ] {
        let mut request = discovery();
        request.headers.insert(header.into(), value.into());
        assert!(!advertisement::eligible_request(&request));
    }
    for target in [
        "/repo.git/info/refs?service=git-upload-pack&extra=1",
        "/repo.git/info/refs?service=git-receive-pack",
    ] {
        let mut request = discovery();
        request.target = target.into();
        assert!(!advertisement::eligible_request(&request));
    }
    let identity = advertisement::identity(&fixture.source).unwrap();
    fs::write(fixture.source.join("HEAD"), b"ref: refs/heads/A\n").unwrap();
    assert!(advertisement::identity(&fixture.source).is_none());
    fs::write(fixture.source.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(
        fixture.source.join("config"),
        [
            NATIVE_CONFIG,
            b"[uploadpack]\nhideRefs = refs/heads/future\n",
        ]
        .concat(),
    )
    .unwrap();
    assert!(advertisement::identity(&fixture.source).is_none());
    assert_ne!(identity, [0; 32]);
}

#[test]
fn cold_discovery_bootstraps_once_then_receive_advances_and_external_recovery_invalidates() {
    let fixture = Fixture::new();
    let mut host = fixture.host();
    let first = host.handle(discovery());
    assert_eq!(first.status, 200);
    assert_eq!(host.metrics.upload_advertisement_native_bootstraps, 1);
    assert_eq!(host.metrics.upload_advertisement_type_checks, 1);
    assert_eq!(host.metrics.cloud_view_rehydrates, 1);
    host.cache.clear();
    host.cache_bytes = 0;
    let second = host.handle(discovery());
    assert_eq!(second.body, first.body);
    assert_eq!(host.metrics.upload_advertisement_shortcuts, 1);
    assert_eq!(host.metrics.cloud_view_rehydrates, 1);
    for (header, value) in [
        ("git-protocol", "version=1"),
        ("git-protocol", "version=2"),
        ("content-type", "nonstandard"),
        ("content-encoding", "gzip"),
    ] {
        let mut request = discovery();
        request.headers.insert(header.into(), value.into());
        let before = host.metrics.cloud_view_rehydrates;
        let shortcuts = host.metrics.upload_advertisement_shortcuts;
        assert_eq!(host.handle(request).status, 200);
        assert_eq!(host.metrics.cloud_view_rehydrates, before + 1);
        assert_eq!(host.metrics.upload_advertisement_shortcuts, shortcuts);
    }
    // A real fetch POST still reconstructs objects and leaves certification valid.
    let mut body = packet(format!("want {}\n", fixture.tip).as_bytes());
    body.extend_from_slice(b"0000");
    body.extend(packet(b"done\n"));
    let response = host.handle(Request {
        method: "POST".into(),
        target: "/repo.git/git-upload-pack".into(),
        headers: BTreeMap::from([(
            "content-type".into(),
            "application/x-git-upload-pack-request".into(),
        )]),
        body,
    });
    assert_eq!(response.status, 200);
    assert!(response.body.windows(4).any(|bytes| bytes == b"PACK"));
    assert_eq!(host.metrics.cloud_view_rehydrates, 6);
    assert!(host.advertisement.is_some());
    let push = fixture.push(&"0".repeat(40), &fixture.tip, "refs/heads/alias");
    let pushed = host.handle(push);
    assert_eq!(pushed.status, 200);
    assert!(String::from_utf8_lossy(&pushed.body).contains("ok refs/heads/alias\n"));
    assert_eq!(host.manifest.generation, 2);
    assert!(host.cache.is_empty());
    assert_eq!(host.cache_bytes, 0);
    let hydrated = host.metrics.cloud_view_rehydrates;
    let updated = host.handle(discovery());
    assert!(String::from_utf8_lossy(&updated.body).contains("refs/heads/alias\n"));
    assert_eq!(host.metrics.cloud_view_rehydrates, hydrated);
    assert_eq!(
        updated.body,
        git_rpc(&host.view, &discovery()).unwrap().body
    );
    // An external authoritative generation must clear even a cached response.
    let mut next = host.manifest.clone();
    next.generation += 1;
    next.refs.remove("refs/heads/alias");
    assert!(
        host.repository
            .state
            .compare_and_swap(host.manifest.generation, &next)
            .unwrap()
    );
    let external = host.handle(discovery());
    assert_eq!(external.body, first.body);
    assert_eq!(host.metrics.upload_advertisement_native_bootstraps, 2);
    host.view_dirty = true;
    assert_eq!(host.handle(discovery()).status, 200);
    assert_eq!(host.metrics.upload_advertisement_native_bootstraps, 3);
    assert!(host.advertisement_bytes() > 0);
    host.trim_cache(CACHE_BYTES - host.advertisement_bytes());
    assert!(host.cache.is_empty());
    assert!(host.cache_bytes + host.advertisement_bytes() <= CACHE_BYTES);
}

#[test]
fn legacy_noncommit_targets_never_certify_and_all_unread_metadata_is_checked() {
    let fixture = Fixture::new();
    let mut manifest = fixture.manifest();
    manifest
        .refs
        .insert("refs/heads/main".into(), fixture.blob.clone());
    let native = fixture.native(&manifest);
    // Matching unpeeled bytes still need the independent first type check.
    assert!(advertisement::Certificate::learn(&manifest, &native, [1; 32]).is_some());
    assert!(!advertisement::branch_commits(&fixture.source, &manifest));
    let mut host = fixture.host();
    assert_eq!(host.handle(discovery()).status, 200);
    let valid = host.manifest.clone();
    for variant in 0..7 {
        host.manifest = valid.clone();
        host.cache.clear();
        host.cache_bytes = 0;
        let mut pack = host.manifest.packs[0].clone();
        match variant {
            0 => pack.pack_bytes = MAX_REPOSITORY_BYTES + 1,
            1 => pack.pack[0].len += 1,
            2 => pack.pack[0].key = "chunks/../invalid".into(),
            3 => pack.bundle = pack.pack.clone(),
            4 => {
                pack.index[0].key = pack.pack[0].key.clone();
            }
            5 => {
                host.manifest
                    .refs
                    .insert("refs/heads/invalid..name".into(), fixture.tip.clone());
            }
            6 => {
                host.manifest
                    .refs
                    .insert("refs/heads/bad-id".into(), "0".repeat(40));
            }
            _ => unreachable!(),
        }
        host.manifest.packs = vec![pack];
        assert!(
            host.upload_advertisement(&discovery()).is_err(),
            "variant {variant}"
        );
    }
    host.manifest = valid;
    host.cache.clear();
    host.cache_bytes = 0;
    // No hidden byte read: valid metadata can advertise even with its bytes
    // temporarily unavailable; any following full consumer still rejects them.
    let key = host.manifest.packs[0].pack[0].key.clone();
    host.repository.blobs.delete(&key).unwrap();
    assert_eq!(host.upload_advertisement(&discovery()).unwrap().status, 200);
    assert!(host.cloud_read_view().is_err());
}

#[test]
fn metrics_added_fields_default_when_reading_old_binaries() {
    let metrics: Metrics = serde_json::from_value(serde_json::json!({"requests": 7})).unwrap();
    assert_eq!(metrics.requests, 7);
    assert_eq!(metrics.upload_advertisement_shortcuts, 0);
    assert_eq!(metrics.cache_retained_bytes, 0);
}

#[test]
fn workflow_git_environment_certifies_only_without_config_injection() {
    const CASE: &str = "GIT_ZERO_ADVERTISEMENT_ENVIRONMENT_CHILD";
    const TEST: &str = "native::advertisement_tests::workflow_git_environment_certifies_only_without_config_injection";
    if let Ok(case) = std::env::var(CASE) {
        if case == "workflow" {
            let fixture = Fixture::new();
            let mut host = fixture.host();
            let first = host.handle(discovery());
            assert_eq!(first.status, 200);
            assert_eq!(host.metrics.upload_advertisement_certificates, 1);
            let mut request = discovery();
            request
                .headers
                .insert("git-protocol".into(), "version=0".into());
            assert_eq!(host.handle(request).body, first.body);
            assert_eq!(host.metrics.upload_advertisement_shortcuts, 1);
            assert_eq!(host.metrics.cloud_view_rehydrates, 1);
        } else {
            // Invalid counted configuration can make Git itself refuse to run.
            // Test policy recognition with only the fixed filesystem view.
            let root = std::env::temp_dir()
                .join(format!("git-zero-advertisement-env-{}", std::process::id()));
            init(&root).unwrap();
            assert!(advertisement::identity(&root).is_none(), "{case}");
            fs::remove_dir_all(root).unwrap();
            if matches!(
                case.as_str(),
                "injected-counted-policy" | "keys-with-zero-count" | "injected-parameters"
            ) {
                let fixture = Fixture::new();
                let mut host = fixture.host();
                let first = host.handle(discovery());
                assert_eq!(first.status, 200);
                let mut request = discovery();
                request
                    .headers
                    .insert("git-protocol".into(), "version=0".into());
                assert_eq!(host.handle(request).body, first.body);
                assert_eq!(host.metrics.upload_advertisement_certificates, 0);
                assert_eq!(host.metrics.upload_advertisement_shortcuts, 0);
                assert_eq!(host.metrics.cloud_view_rehydrates, 2);
            }
        }
        return;
    }
    for (case, count, injection) in [
        ("workflow", "0", None),
        ("nonzero-count", "1", None),
        ("noncanonical-count", "00", None),
        ("negative-count", "-1", None),
        ("injected-counted-policy", "1", Some("counted")),
        ("keys-with-zero-count", "0", Some("counted")),
        ("injected-parameters", "0", Some("parameters")),
        ("unknown-policy", "0", Some("unknown")),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (name, _) in std::env::vars_os() {
            if name.as_encoded_bytes().starts_with(b"GIT_") {
                child.env_remove(name);
            }
        }
        child
            .env("GIT_CONFIG_COUNT", count)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env(CASE, case);
        match injection {
            Some("counted") => {
                child
                    .env("GIT_CONFIG_KEY_0", "uploadpack.hideRefs")
                    .env("GIT_CONFIG_VALUE_0", "refs/heads/future");
            }
            Some("parameters") => {
                child.env(
                    "GIT_CONFIG_PARAMETERS",
                    "'uploadpack.hideRefs=refs/heads/future'",
                );
            }
            Some("unknown") => {
                child.env("GIT_NAMESPACE", "future");
            }
            None => {}
            _ => unreachable!(),
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn legacy_noncommit_unborn_missing_main_tags_and_unknown_objects_keep_full_fallback() {
    let fixture = Fixture::new();
    let mut host = fixture.host();
    for refs in [
        BTreeMap::from([("refs/heads/main".into(), fixture.blob.clone())]),
        BTreeMap::new(),
        BTreeMap::from([("refs/heads/other".into(), fixture.tip.clone())]),
        BTreeMap::from([
            ("refs/heads/main".into(), fixture.tip.clone()),
            ("refs/tags/tag".into(), fixture.tip.clone()),
        ]),
        BTreeMap::from([("refs/heads/main".into(), "a".repeat(40))]),
    ] {
        let mut next = host.manifest.clone();
        next.generation += 1;
        next.refs = refs;
        assert!(
            host.repository
                .state
                .compare_and_swap(host.manifest.generation, &next)
                .unwrap()
        );
        let before = host.metrics.cloud_view_rehydrates;
        assert_eq!(host.handle(discovery()).status, 200);
        assert_eq!(host.metrics.cloud_view_rehydrates, before + 1);
        assert!(host.advertisement.is_none());
        assert_eq!(host.metrics.upload_advertisement_shortcuts, 0);
    }
}
