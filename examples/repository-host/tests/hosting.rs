use git_zero_host::http::{Request, Response};
use git_zero_host::native::{NativeHost, command, refs};
use git_zero_host::storage::{LocalServices, Manifest, StateStore};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const ZERO_ID: &str = "0000000000000000000000000000000000000000";
const MAIN: &str = "refs/heads/main";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "git-zero-host-integration-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(repository: &Path, arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(repository).args(arguments);
    command(process, input)
        .unwrap_or_else(|error| panic!("git {arguments:?} in {}: {error}", repository.display()))
        .stdout
}

fn git_text(repository: &Path, arguments: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(repository, arguments, input))
        .unwrap()
        .trim()
        .to_string()
}

fn source_repository(directory: &TestDirectory) -> PathBuf {
    let source = directory.0.join("source.git");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha1",
        ],
        &[],
    );
    git(
        &source,
        &["config", "user.name", "Hosting integration"],
        &[],
    );
    git(
        &source,
        &["config", "user.email", "hosting@example.invalid"],
        &[],
    );
    source
}

fn commit(source: &Path, parent: Option<&str>, revision: usize) -> String {
    let body = format!(
        "# Repository host fixture\n{}\nrevision {revision}\n",
        "stable content for compressed and delta packs\n".repeat(512)
    );
    let blob = git_text(source, &["hash-object", "-w", "--stdin"], body.as_bytes());
    let tree = git_text(
        source,
        &["mktree"],
        format!("100644 blob {blob}\tREADME.md\n").as_bytes(),
    );
    let mut arguments = vec!["commit-tree", &tree];
    if let Some(parent) = parent {
        arguments.extend(["-p", parent]);
    }
    git_text(
        source,
        &arguments,
        format!("Revision {revision}\n").as_bytes(),
    )
}

fn packet(payload: &str) -> Vec<u8> {
    let mut output = format!("{:04x}", payload.len() + 4).into_bytes();
    output.extend_from_slice(payload.as_bytes());
    output
}

fn request(method: &str, target: &str, body: Vec<u8>, content_type: Option<&str>) -> Request {
    Request {
        method: method.into(),
        target: target.into(),
        headers: content_type
            .map(|value| BTreeMap::from([("content-type".into(), value.into())]))
            .unwrap_or_default(),
        body,
    }
}

fn push_request(source: &Path, old: &str, new: &str) -> Request {
    push_ref_request(source, old, new, MAIN)
}

fn push_ref_request(source: &Path, old: &str, new: &str, name: &str) -> Request {
    let mut body = packet(&format!(
        "{old} {new} {name}\0report-status atomic ofs-delta\n"
    ));
    body.extend_from_slice(b"0000");
    if new != ZERO_ID {
        let mut revisions = format!("{new}\n");
        if old != ZERO_ID {
            revisions.push_str(&format!("^{old}\n"));
        }
        body.extend(git(
            source,
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
    request(
        "POST",
        "/repo.git/git-receive-pack",
        body,
        Some("application/x-git-receive-pack-request"),
    )
}

fn assert_push_ok(response: &Response) {
    let body = String::from_utf8_lossy(&response.body);
    assert_eq!(response.status, 200, "{body}");
    assert!(body.contains("unpack ok\n"), "{body}");
    assert!(body.contains(&format!("ok {MAIN}\n")), "{body}");
    assert!(!body.contains(&format!("ng {MAIN} ")), "{body}");
}

#[cfg(unix)]
#[test]
fn thin_pushes_support_non_utf8_and_newline_root_names() {
    use std::os::unix::ffi::OsStrExt;

    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory
        .0
        .join(std::ffi::OsStr::from_bytes(b"host-\xff\nrepository"));
    let mut host = NativeHost::open(root.clone()).unwrap();
    let first = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &first)));
    let second = commit(&source, Some(&first), 2);
    assert_push_ok(&host.handle(push_request(&source, &first, &second)));
    assert_eq!(manifest(&root).refs.get(MAIN), Some(&second));
    let view = root.join("view/repo.git");
    git(&view, &["fsck", "--strict", "--no-reflogs"], &[]);
    assert_eq!(git_text(&view, &["rev-list", "--count", MAIN], &[]), "2");
}

#[test]
fn master_only_publication_and_recovery_produce_a_checked_out_clone() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let tip = commit(&source, None, 1);
    let response = host.handle(push_ref_request(
        &source,
        ZERO_ID,
        &tip,
        "refs/heads/master",
    ));
    assert_eq!(response.status, 200);
    let body = String::from_utf8_lossy(&response.body);
    assert!(body.contains("ok refs/heads/master\n"), "{body}");
    let view = root.join("view/repo.git");
    let clone_and_check = |name: &str| {
        let cloned = directory.0.join(name);
        let mut process = Command::new("git");
        process
            .args(["clone", "--quiet", "--no-local"])
            .arg(&view)
            .arg(&cloned);
        command(process, &[]).unwrap();
        assert_eq!(git_text(&cloned, &["rev-parse", "HEAD"], &[]), tip);
        assert_eq!(
            git_text(&cloned, &["symbolic-ref", "HEAD"], &[]),
            "refs/heads/master"
        );
        assert!(cloned.join("README.md").is_file());
    };
    clone_and_check("before-restart");
    drop(host);
    let _recovered = NativeHost::open(root.clone()).unwrap();
    clone_and_check("after-restart");
}

#[test]
fn large_pack_checksum_overlap_preserves_authoritative_bytes_and_recovery() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let mut state = 0x9e3779b97f4a7c15u64;
    let bytes: Vec<_> = (0..1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let blob = git_text(&source, &["hash-object", "-w", "--stdin"], &bytes);
    let tree = git_text(
        &source,
        &["mktree"],
        format!("100644 blob {blob}\tdata.bin\n").as_bytes(),
    );
    let tip = git_text(&source, &["commit-tree", &tree], b"Large import\n");
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &tip)));
    assert!(host.metrics.receive_reused_pack_bytes >= bytes.len() as u64);
    assert_eq!(
        git(
            &root.join("view/repo.git"),
            &["cat-file", "blob", &blob],
            &[]
        ),
        bytes
    );
    drop(host);
    let _recovered = NativeHost::open(root.clone()).unwrap();
    assert_eq!(
        git(
            &root.join("view/repo.git"),
            &["cat-file", "blob", &blob],
            &[]
        ),
        bytes
    );
    git(
        &root.join("view/repo.git"),
        &["fsck", "--strict", "--no-reflogs"],
        &[],
    );
}

fn manifest(root: &Path) -> Manifest {
    LocalServices::new(root.join("services"))
        .unwrap()
        .state
        .load()
        .unwrap()
}

fn upload_request(id: &str) -> Request {
    let mut body = packet(&format!("want {id}\n"));
    body.extend_from_slice(b"0000");
    body.extend(packet("done\n"));
    request(
        "POST",
        "/repo.git/git-upload-pack",
        body,
        Some("application/x-git-upload-pack-request"),
    )
}

fn verify_uploaded_pack(directory: &Path, response: &Response, tip: &str) {
    assert_eq!(response.status, 200);
    // No sideband was requested: Git returns one NAK packet followed by a pack.
    assert!(response.body.starts_with(b"0008NAK\nPACK"));
    fs::create_dir(directory).unwrap();
    git(
        directory,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    git(
        directory,
        &["index-pack", "--stdin", "--strict"],
        &response.body[b"0008NAK\n".len()..],
    );
    git(directory, &["update-ref", MAIN, tip], &[]);
    git(directory, &["fsck", "--strict", "--no-reflogs"], &[]);
}

#[test]
fn incremental_pushes_compact_and_recover_from_authoritative_chunks() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let mut previous = ZERO_ID.to_string();
    let mut tips = Vec::new();
    for revision in 1..=4 {
        let tip = commit(
            &source,
            (previous != ZERO_ID).then_some(previous.as_str()),
            revision,
        );
        assert_push_ok(&host.handle(push_request(&source, &previous, &tip)));
        let published = manifest(&root);
        assert_eq!(published.generation, revision as u64);
        assert_eq!(published.refs.get(MAIN), Some(&tip));
        if revision == 3 {
            assert_eq!(host.metrics.compactions, 0);
            assert_eq!(published.packs.len(), 3);
        }
        previous = tip.clone();
        tips.push(tip);
    }
    let published = manifest(&root);
    assert_eq!(host.metrics.compactions, 1);
    assert!(host.metrics.compaction_input_bytes > 0);
    assert!(host.metrics.compaction_output_bytes > 0);
    assert!(host.metrics.inline_maintenance_seconds > 0.0);
    assert_eq!(published.packs.len(), 1);
    assert_eq!(published.packs[0].level, 1);
    assert!(published.packs[0].pack_bytes < 4096);
    let statistics = host.handle(request("GET", "/_object-stats", vec![], None));
    assert_eq!(statistics.status, 200);
    let statistics: serde_json::Value = serde_json::from_slice(&statistics.body).unwrap();
    let objects = &statistics["objects"];
    let ids = git(
        &source,
        &["rev-list", "--objects", "--no-object-names", &previous],
        &[],
    );
    let logical_sizes = git(&source, &["cat-file", "--batch-check=%(objectsize)"], &ids);
    let logical_bytes: u64 = String::from_utf8(logical_sizes)
        .unwrap()
        .lines()
        .map(|line| line.parse::<u64>().unwrap())
        .sum();
    assert_eq!(
        objects["logical_object_bytes"].as_u64(),
        Some(logical_bytes)
    );
    assert_eq!(
        objects["by_type"]["blob"]["unique_objects"].as_u64(),
        Some(4)
    );
    assert!(objects["delta_representations"].as_u64().unwrap() > 0);
    assert_eq!(
        objects["compressed_pack_bytes"].as_u64(),
        Some(published.packs[0].pack_bytes)
    );
    let discovery = host.handle(request(
        "GET",
        "/repo.git/info/refs?service=git-upload-pack",
        vec![],
        None,
    ));
    assert_eq!(discovery.status, 200);
    assert!(String::from_utf8_lossy(&discovery.body).contains(&previous));
    drop(host);

    // Lose every disposable pack and ref, as after restoring authoritative storage
    // onto a fresh machine. Recovery must not depend on leftover native objects.
    fs::remove_dir_all(root.join("view")).unwrap();
    let mut recovered = NativeHost::open(root.clone()).unwrap();
    assert_eq!(manifest(&root), published);
    let view = root.join("view/repo.git");
    assert_eq!(refs(&view).unwrap(), published.refs);
    git(&view, &["fsck", "--strict", "--no-reflogs"], &[]);
    assert_eq!(git_text(&view, &["rev-list", "--count", MAIN], &[]), "4");
    for tip in &tips {
        assert_eq!(git_text(&view, &["cat-file", "-t", tip], &[]), "commit");
    }
    let response = recovered.handle(upload_request(&previous));
    verify_uploaded_pack(&directory.0.join("download.git"), &response, &previous);
}

#[test]
fn discovery_uses_the_first_service_parameter_consistently() {
    let directory = TestDirectory::new();
    let mut host = NativeHost::open(directory.0.join("host")).unwrap();
    let response = host.handle(request(
        "GET",
        "/repo.git/info/refs?service=git-upload-pack&service=git-receive-pack",
        vec![],
        None,
    ));
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("Content-Type").map(String::as_str),
        Some("application/x-git-upload-pack-advertisement")
    );
    assert!(
        response
            .body
            .windows(b"# service=git-upload-pack\n".len())
            .any(|bytes| bytes == b"# service=git-upload-pack\n")
    );
}

#[test]
fn query_on_receive_route_cannot_bypass_durable_publication() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let tip = commit(&source, None, 1);
    let mut push = push_request(&source, ZERO_ID, &tip);
    push.target.push_str("?trace=1");
    assert_push_ok(&host.handle(push));
    let published = manifest(&root);
    assert_eq!(published.generation, 1);
    assert_eq!(published.refs.get(MAIN), Some(&tip));
    assert_eq!(published.packs.len(), 1);
    drop(host);
    fs::remove_dir_all(root.join("view")).unwrap();
    NativeHost::open(root.clone()).unwrap();
    assert_eq!(refs(&root.join("view/repo.git")).unwrap(), published.refs);
}

#[test]
fn stale_and_corrupt_pushes_leave_the_manifest_and_refs_unchanged() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let first = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &first)));
    let published = manifest(&root);
    let second = commit(&source, Some(&first), 2);

    let stale = host.handle(push_request(&source, ZERO_ID, &second));
    assert_eq!(stale.status, 200);
    assert!(String::from_utf8_lossy(&stale.body).contains(&format!("ng {MAIN} ")));
    assert_eq!(manifest(&root), published);
    assert_eq!(refs(&root.join("view/repo.git")).unwrap(), published.refs);

    let mut corrupt = push_request(&source, &first, &second);
    *corrupt.body.last_mut().unwrap() ^= 1;
    let rejected = host.handle(corrupt);
    assert_eq!(rejected.status, 200);
    let body = String::from_utf8_lossy(&rejected.body);
    assert!(body.contains(&format!("ng {MAIN} ")), "{body}");
    assert!(!body.contains("unpack ok\n"), "{body}");
    assert_eq!(manifest(&root), published);
    assert_eq!(refs(&root.join("view/repo.git")).unwrap(), published.refs);
    git(
        &root.join("view/repo.git"),
        &["fsck", "--strict", "--no-reflogs"],
        &[],
    );

    // A failed quarantine must not poison a subsequent valid retry.
    assert_push_ok(&host.handle(push_request(&source, &first, &second)));
    assert_eq!(manifest(&root).generation, published.generation + 1);
    assert_eq!(manifest(&root).refs.get(MAIN), Some(&second));
}

#[test]
fn deleting_the_last_ref_publishes_and_recovers_an_empty_ref_set() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let tip = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &tip)));
    let previous = manifest(&root);
    assert_push_ok(&host.handle(push_request(&source, &tip, ZERO_ID)));
    let deleted = manifest(&root);
    assert_eq!(deleted.generation, previous.generation + 1);
    assert!(deleted.refs.is_empty());
    assert!(refs(&root.join("view/repo.git")).unwrap().is_empty());
    drop(host);

    // Reintroduce a stale native ref; recovery must delete it using the manifest.
    git(
        &root.join("view/repo.git"),
        &["update-ref", MAIN, &tip],
        &[],
    );
    let _recovered = NativeHost::open(root.clone()).unwrap();
    assert_eq!(manifest(&root), deleted);
    assert!(refs(&root.join("view/repo.git")).unwrap().is_empty());
}

#[test]
fn upload_cache_reuses_responses_and_invalidates_after_publication() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let first = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &first)));
    let first_request = upload_request(&first);
    let cold = host.handle(first_request.clone());
    let warm = host.handle(first_request.clone());
    assert_eq!(cold.body, warm.body);
    assert_eq!(host.metrics.cache_misses, 1);
    assert_eq!(host.metrics.cache_hits, 1);
    verify_uploaded_pack(&directory.0.join("first-download.git"), &warm, &first);

    let second = commit(&source, Some(&first), 2);
    assert_push_ok(&host.handle(push_request(&source, &first, &second)));
    // The same request must go through the codec again after a new generation.
    let old_tip = host.handle(first_request);
    assert_eq!(old_tip.status, 200);
    assert_eq!(host.metrics.cache_misses, 2);
    assert_eq!(host.metrics.cache_hits, 1);
    let new_tip = host.handle(upload_request(&second));
    let new_tip_cached = host.handle(upload_request(&second));
    assert_eq!(new_tip.body, new_tip_cached.body);
    assert_eq!(host.metrics.cache_misses, 3);
    assert_eq!(host.metrics.cache_hits, 2);
    verify_uploaded_pack(
        &directory.0.join("second-download.git"),
        &new_tip_cached,
        &second,
    );
    assert_eq!(
        git_text(
            &directory.0.join("second-download.git"),
            &["rev-list", "--count", MAIN],
            &[],
        ),
        "2"
    );
}

#[test]
fn recovery_rejects_corrupted_authoritative_chunks() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let tip = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &tip)));
    let published = manifest(&root);
    let chunk = root
        .join("services/blobs")
        .join(&published.packs[0].pack[0].key);
    let mut bytes = fs::read(&chunk).unwrap();
    bytes[0] ^= 1;
    // Git's immutable pack is now shared with the blob. Explicitly override its
    // read-only mode for this deliberate corruption injection.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&chunk, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(chunk, bytes).unwrap();
    drop(host);
    fs::remove_dir_all(root.join("view")).unwrap();

    let error = match NativeHost::open(root.clone()) {
        Ok(_) => panic!("recovery exposed a repository with a corrupt pack chunk"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("corrupt chunk"), "{error}");
    assert_eq!(manifest(&root), published);
    assert!(refs(&root.join("view/repo.git")).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn recovery_retries_failed_cleanup_before_advertising_durable_refs() {
    use std::os::unix::ffi::OsStrExt;

    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let view = root.join("view/repo.git");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let first = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &first)));
    let discovery = || {
        request(
            "GET",
            "/repo.git/info/refs?service=git-receive-pack",
            vec![],
            None,
        )
    };
    let advertised = host.handle(discovery());
    assert_eq!(advertised.status, 200);
    assert!(String::from_utf8_lossy(&advertised.body).contains(&first));

    // Publish the next generation, then fail its disposable ref update. The
    // host must recover the durable tip before trusting its old manifest/cache.
    fs::create_dir(view.join("packed-refs.next")).unwrap();
    let second = commit(&source, Some(&first), 2);
    assert_eq!(
        host.handle(push_request(&source, &first, &second)).status,
        503
    );
    let published = manifest(&root);
    assert_eq!(published.generation, 2);
    assert_eq!(published.refs.get(MAIN), Some(&second));

    // Rebuilding the view succeeds, but an invalid orphan filename makes the
    // subsequent recovery scan fail before in-memory state can be replaced.
    let orphan = root
        .join("services/blobs")
        .join(std::ffi::OsStr::from_bytes(b"orphan-\xff"));
    fs::write(&orphan, b"unpublished").unwrap();
    assert_eq!(host.handle(discovery()).status, 503);
    assert_eq!(refs(&view).unwrap(), published.refs);
    fs::remove_file(orphan).unwrap();

    let recovered = host.handle(discovery());
    assert_eq!(recovered.status, 200);
    let body = String::from_utf8_lossy(&recovered.body);
    assert!(body.contains(&second), "{body}");
    assert!(!body.contains(&first), "{body}");
    assert_eq!(manifest(&root), published);
}

#[test]
fn recovery_discards_unpublished_packs_before_validating_later_pushes() {
    let directory = TestDirectory::new();
    let source = source_repository(&directory);
    let root = directory.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let first = commit(&source, None, 1);
    assert_push_ok(&host.handle(push_request(&source, ZERO_ID, &first)));
    let published = manifest(&root);
    let orphan = commit(&source, Some(&first), 2);
    let view = root.join("view/repo.git");

    // A crash after native pack installation but before manifest CAS can leave
    // valid object bytes that were never published. Install that exact shape.
    let pack = git(
        &source,
        &["pack-objects", "--stdout", "--revs"],
        format!("{orphan}\n").as_bytes(),
    );
    git(&view, &["index-pack", "--stdin", "--strict"], &pack);
    assert_eq!(git_text(&view, &["cat-file", "-t", &orphan], &[]), "commit");
    assert_eq!(manifest(&root), published);
    drop(host);

    let mut recovered = NativeHost::open(root.clone()).unwrap();
    let mut probe = Command::new("git");
    probe.arg("-C").arg(&view).args(["cat-file", "-e", &orphan]);
    assert!(
        command(probe, &[]).is_err(),
        "unpublished object survived recovery"
    );

    // Send a valid empty pack, claiming the target already exists server-side.
    // Without scrubbing, Git finds the orphan through alternates and approves
    // a manifest whose reachable object is missing from authoritative storage.
    let mut body = packet(&format!(
        "{first} {orphan} {MAIN}\0report-status atomic ofs-delta\n"
    ));
    body.extend_from_slice(b"0000");
    body.extend(git(&source, &["pack-objects", "--stdout"], &[]));
    let rejected = recovered.handle(request(
        "POST",
        "/repo.git/git-receive-pack",
        body,
        Some("application/x-git-receive-pack-request"),
    ));
    assert_eq!(rejected.status, 200);
    assert!(
        String::from_utf8_lossy(&rejected.body).contains(&format!("ng {MAIN} ")),
        "{}",
        String::from_utf8_lossy(&rejected.body)
    );
    assert_eq!(manifest(&root), published);
    assert_eq!(refs(&view).unwrap(), published.refs);
    git(&view, &["fsck", "--strict", "--no-reflogs"], &[]);
}
