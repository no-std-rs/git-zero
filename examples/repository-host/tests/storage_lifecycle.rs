use git_zero_host::http::{Request, Response};
use git_zero_host::native::{NativeHost, command};
use git_zero_host::storage::Manifest;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const ZERO: &str = "0000000000000000000000000000000000000000";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        loop {
            let root = std::env::temp_dir().join(format!(
                "git-zero-storage-lifecycle-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fixture: {error}"),
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(repository: &Path, arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut process = Command::new("git");
    process.arg("-C").arg(repository).args(arguments);
    command(process, input)
        .unwrap_or_else(|error| panic!("git {arguments:?}: {error}"))
        .stdout
}

fn git_id(repository: &Path, arguments: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(repository, arguments, input))
        .unwrap()
        .trim()
        .into()
}

fn commit(source: &Path, parent: &str, revision: usize) -> String {
    let content = format!("{}revision {revision}\n", "append fixture\n".repeat(512));
    let blob = git_id(
        source,
        &["hash-object", "-w", "--stdin"],
        content.as_bytes(),
    );
    let tree = git_id(
        source,
        &["mktree"],
        format!("100644 blob {blob}\tfixture.txt\n").as_bytes(),
    );
    let mut arguments = vec!["commit-tree", &tree];
    if parent != ZERO {
        arguments.extend(["-p", parent]);
    }
    git_id(
        source,
        &arguments,
        format!("revision {revision}\n").as_bytes(),
    )
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

fn push(source: &Path, previous: &str, next: &str) -> Request {
    let mut body = packet(&format!(
        "{previous} {next} refs/heads/main\0report-status atomic ofs-delta\n"
    ));
    body.extend_from_slice(b"0000");
    let revisions = if previous == ZERO {
        format!("{next}\n")
    } else {
        format!("{next}\n^{previous}\n")
    };
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
    rpc("receive", body)
}

fn assert_push(response: &Response) {
    assert_eq!(response.status, 200);
    let body = String::from_utf8_lossy(&response.body);
    assert!(body.contains("unpack ok\n"), "{body}");
    assert!(body.contains("ok refs/heads/main\n"), "{body}");
}

fn manifest(root: &Path) -> Manifest {
    serde_json::from_slice(&fs::read(root.join("services/manifest.json")).unwrap()).unwrap()
}

fn files(root: &Path) -> BTreeSet<String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                );
            }
        }
    }
    files
}

fn assert_live_storage(root: &Path, published: &Manifest) {
    let live: BTreeSet<_> = published
        .packs
        .iter()
        .flat_map(|pack| pack.pack.iter().chain(&pack.index))
        .map(|chunk| chunk.key.clone())
        .collect();
    let blob_root = root.join("services/blobs");
    assert_eq!(
        files(&blob_root),
        live,
        "unreferenced chunks survived a push"
    );
    let native_packs = files(&root.join("view/repo.git/objects/pack"));
    assert_eq!(
        native_packs
            .iter()
            .filter(|name| name.ends_with(".pack"))
            .count(),
        published.packs.len(),
        "native view retained retired packs"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for pack in &published.packs {
            for (extension, chunks) in [("pack", &pack.pack), ("idx", &pack.index)] {
                if chunks.len() == 1 {
                    let blob = fs::metadata(blob_root.join(&chunks[0].key)).unwrap();
                    let view = fs::metadata(root.join(format!(
                        "view/repo.git/objects/pack/pack-{}.{extension}",
                        pack.id
                    )))
                    .unwrap();
                    assert_eq!(
                        (blob.dev(), blob.ino()),
                        (view.dev(), view.ino()),
                        "single-chunk {extension} duplicated its physical allocation"
                    );
                }
            }
        }
    }
}

#[test]
fn repeated_pushes_recovery_and_varied_fetches_keep_storage_bounded() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.git");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--quiet", "--object-format=sha1"],
        &[],
    );
    git(&source, &["config", "user.name", "Storage lifecycle"], &[]);
    git(
        &source,
        &["config", "user.email", "storage@example.invalid"],
        &[],
    );
    let root = fixture.0.join("host");
    let mut host = NativeHost::open(root.clone()).unwrap();
    let mut previous = ZERO.to_string();
    let mut tips = Vec::new();
    for revision in 1..=20 {
        let tip = commit(&source, &previous, revision);
        assert_push(&host.handle(push(&source, &previous, &tip)));
        let published = manifest(&root);
        assert_eq!(published.generation, revision as u64);
        assert_live_storage(&root, &published);
        previous = tip.clone();
        tips.push(tip);
    }
    assert!(host.metrics.compactions >= 5);
    assert!(host.metrics.retired_chunk_deletes > 0);
    drop(host);

    let mut host = NativeHost::open(root.clone()).unwrap();
    let published = manifest(&root);
    assert_live_storage(&root, &published);
    let persisted_files = files(&root);
    let blob_puts = host.metrics.blob_puts;
    for offset in 0..19 {
        let have = &tips[(offset * 7) % 19];
        let mut body = packet(&format!("want {previous} ofs-delta\n"));
        body.extend_from_slice(b"0000");
        body.extend(packet(&format!("have {have}\n")));
        body.extend(packet("done\n"));
        let response = host.handle(rpc("upload", body));
        assert_eq!(response.status, 200);
        assert!(response.body.windows(4).any(|bytes| bytes == b"PACK"));
        assert_live_storage(&root, &published);
    }
    let metrics_request = || Request {
        method: "GET".into(),
        target: "/metrics".into(),
        headers: BTreeMap::new(),
        body: vec![],
    };
    let before_empty = host.handle(metrics_request());
    let before_empty: serde_json::Value = serde_json::from_slice(&before_empty.body).unwrap();
    for nonce in 0..300 {
        // upload-pack accepts an empty negotiation and ignores trailing data.
        // Unique bodies must not create an unlimited cache of zero-byte RPCs.
        let response = host.handle(rpc("upload", format!("0000ignored-{nonce}").into_bytes()));
        assert_eq!(response.status, 200);
        assert!(response.body.is_empty());
    }
    let after_empty = host.handle(metrics_request());
    let after_empty: serde_json::Value = serde_json::from_slice(&after_empty.body).unwrap();
    assert_eq!(after_empty["cache_entries"], before_empty["cache_entries"]);
    for nonce in 0..300 {
        let response = host.handle(Request {
            method: "GET".into(),
            target: format!("/repo.git/info/refs?service=git-upload-pack&nonce={nonce}"),
            headers: BTreeMap::new(),
            body: vec![],
        });
        assert_eq!(response.status, 200);
        assert!(String::from_utf8_lossy(&response.body).contains(&previous));
    }
    assert_eq!(manifest(&root), published);
    assert_eq!(
        files(&root),
        persisted_files,
        "fetches wrote persistent cache files"
    );
    assert_eq!(host.metrics.blob_puts, blob_puts);
    let metrics = host.handle(metrics_request());
    let metrics: serde_json::Value = serde_json::from_slice(&metrics.body).unwrap();
    assert!(
        metrics["cache_entries"].as_u64().unwrap()
            <= metrics["cache_entry_limit"].as_u64().unwrap()
    );
    assert!(
        metrics["cache_bytes"].as_u64().unwrap() <= metrics["cache_budget_bytes"].as_u64().unwrap()
    );
}
