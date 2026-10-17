#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../examples/support/fixture.rs"]
mod fixture;
#[path = "../../../tests/common/selected_http.rs"]
mod protocol;
#[path = "../examples/support/selected_http.rs"]
mod selected_http;
#[path = "common/server.rs"]
mod server;
#[path = "common/validated.rs"]
mod validated;

use git_zero::{
    ValidatedRepository,
    cache::selected::{FixedCache, Outcome, PackCache},
    http::WantOptions,
};
use git_zero_rustix::{accept, cache::DiskCache, listen, local_port};
use rustix::{
    fd::AsFd,
    fs::{self, Mode, OFlags},
};
use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Cleanup(PathBuf);
impl Cleanup {
    fn prepare(format: &str, version: u8) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "git-zero-http-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let cleanup = Self(root);
        python(&cleanup.0, "prepare", format, version, 0);
        cleanup
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained HTTP cache fixture/traces: {}", self.0.display());
        } else {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn python(root: &Path, phase: &str, format: &str, version: u8, port: u16) {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/server_cached.py"
        ))
        .arg(phase)
        .arg("--root")
        .arg(root)
        .arg("--object-format")
        .arg(format)
        .arg("--version")
        .arg(version.to_string())
        .arg("--zero-url")
        .arg(format!("http://127.0.0.1:{port}/repo.git"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{phase}/{format}/v{version}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn phase(
    root: &Path,
    name: &str,
    format: &str,
    version: u8,
    listener: &(impl AsFd + Sync),
    cache: &mut (impl PackCache + Send),
) -> Vec<Outcome> {
    let directory = root.join(if name == "return" { "A" } else { name });
    validated::with_fixture(&directory, |repository| {
        Ok(serve_phase(
            root, name, format, version, listener, cache, repository,
        ))
    })
    .unwrap()
}

fn serve_phase(
    root: &Path,
    name: &str,
    format: &str,
    version: u8,
    listener: &(impl AsFd + Sync),
    cache: &mut (impl PackCache + Send),
    repository: &ValidatedRepository<'_, '_, impl git_zero::Repository + Sync>,
) -> Vec<Outcome> {
    let port = local_port(listener).unwrap();
    let mut storage = selected_http::Storage::new(262_144).unwrap();
    let mut outcomes = Vec::new();
    server::while_serving(
        port,
        || accept(listener).unwrap(),
        |socket| record_outcome(socket, repository, cache, &mut storage, &mut outcomes),
        |port| python(root, name, format, version, port),
    );
    outcomes
}

fn record_outcome(
    socket: &impl AsFd,
    repository: &ValidatedRepository<'_, '_, impl git_zero::Repository>,
    cache: &mut impl PackCache,
    storage: &mut selected_http::Storage,
    outcomes: &mut Vec<Outcome>,
) {
    let (status, outcome) = allocations::without_allocations(|| {
        storage.respond(socket, repository, cache, WantOptions::STRICT)
    })
    .unwrap();
    assert_eq!(status, 200);
    if let Some(outcome) = outcome {
        outcomes.push(outcome);
    }
}

fn compare(disk: bool) {
    for format in ["sha1", "sha256"] {
        for version in 0..=2 {
            compare_case(disk, format, version);
        }
    }
}

fn compare_case(disk: bool, format: &str, version: u8) {
    let cleanup = Cleanup::prepare(format, version);
    let root = &cleanup.0;
    let cache_directory = root.join("cache");
    std::fs::create_dir(&cache_directory).unwrap();
    let directory = fs::openat(
        fs::CWD,
        cache_directory.as_os_str().as_encoded_bytes(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    let listener = listen(0).unwrap();
    let mut resident = vec![0; 262_144];
    let mut memory = FixedCache::new(&mut resident);
    for (generation, name) in ["A", "B", "return"].into_iter().enumerate() {
        let outcomes = if disk {
            // Reopening the backend proves files, not in-process state,
            // retain A while another immutable generation is served.
            let mut disk_resident = vec![0; 262_144];
            let mut nonce = [0; 16];
            nonce[..8].copy_from_slice(&(generation as u64).to_be_bytes());
            let mut cache = DiskCache::new(directory.as_fd(), &mut disk_resident, nonce);
            phase(root, name, format, version, &listener, &mut cache)
        } else {
            phase(root, name, format, version, &listener, &mut memory)
        };
        assert!(
            outcomes.contains(&Outcome::Hit),
            "no actual hit: {outcomes:?}"
        );
        let first = if disk && name == "return" {
            Outcome::Hit
        } else {
            Outcome::Miss
        };
        assert_eq!(
            outcomes[0], first,
            "retained A should hit after backend reopen"
        );
        std::fs::write(
            root.join(format!("cache-{name}.txt")),
            format!("{outcomes:?}\n"),
        )
        .unwrap();
    }
}

#[test]
fn real_git_memory_misses_hits_haves_and_eviction() {
    compare(false);
}

#[test]
fn real_git_retained_disk_hits_after_a_b_a_and_backend_reopen() {
    compare(true);
}

#[derive(Clone, Copy)]
enum Failure {
    Lookup,
    Put,
    Corrupt,
    Capacity,
}

impl PackCache for Failure {
    fn get(&mut self, _: git_zero::cache::selected::Key) -> git_zero::Result<Option<&[u8]>> {
        match self {
            Self::Lookup => Err(git_zero::Error::Backend),
            Self::Corrupt => Ok(Some(b"not a pack")),
            _ => Ok(None),
        }
    }
    fn put(&mut self, _: git_zero::cache::selected::Key, _: &[u8]) -> git_zero::Result<()> {
        Err(git_zero::Error::Backend)
    }
}

#[test]
fn buffered_pack_stage_errors_replace_controls_before_any_http_success() {
    let (mut tree, mut commit) = ([0; 128], [0; 512]);
    let source = git_zero::Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let (mut metadata, mut by_id) = ([None; 2], [0; 2]);
    let repository = ValidatedRepository::new(
        &source,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let want = format!("want {}\n", git_zero::Repository::head(&source));
    let body = format!("{:04x}{want}00000009done\n", want.len() + 4);
    let request = protocol::wire(body.as_bytes(), 0);
    for (failure, capacity, status) in [
        (Failure::Lookup, 8192, 500),
        (Failure::Put, 8192, 500),
        (Failure::Corrupt, 8192, 400),
        (Failure::Capacity, 0, 413),
    ] {
        let mut storage = selected_http::Storage::new(capacity).unwrap();
        let reply = exchange(
            &repository,
            &mut failure.clone(),
            &mut storage,
            WantOptions::STRICT,
            &request,
        );
        reply.assert((status, None));
    }
}

fn exchange<R: git_zero::Repository + Sync>(
    repository: &ValidatedRepository<'_, '_, R>,
    cache: &mut (impl PackCache + Send),
    storage: &mut selected_http::Storage,
    options: WantOptions,
    request: &[u8],
) -> Reply {
    use std::io::{Read, Write};
    let listener = listen(0).unwrap();
    let mut client = TcpStream::connect(("127.0.0.1", local_port(&listener).unwrap())).unwrap();
    let server = accept(&listener).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    std::thread::scope(|scope| {
        let serving = scope.spawn(move || {
            allocations::without_allocations(|| {
                storage.respond(&server, repository, cache, options)
            })
            .unwrap()
        });
        client.write_all(request).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut wire = Vec::new();
        client.take(65536).read_to_end(&mut wire).unwrap();
        let (status, outcome) = serving.join().unwrap();
        Reply {
            status,
            outcome,
            wire,
        }
    })
}

struct Reply {
    status: u16,
    outcome: Option<Outcome>,
    wire: Vec<u8>,
}

impl Reply {
    fn assert(self, expected: (u16, Option<Outcome>)) {
        assert_eq!((self.status, self.outcome), expected);
        assert!(
            self.wire
                .starts_with(format!("HTTP/1.0 {} ", expected.0).as_bytes())
        );
        if expected.0 != 200 {
            assert_error_body(&self.wire);
        }
    }
}

fn assert_error_body(wire: &[u8]) {
    let body = &wire[wire
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4..];
    let mut packet = body;
    assert!(
        matches!(git_zero::pktline::read(&mut packet).unwrap(), Some(git_zero::pktline::Packet::Data(bytes)) if bytes.starts_with(b"ERR "))
    );
    assert!(
        packet.is_empty(),
        "provisional ACK/pack leaked into failure response"
    );
}

#[test]
fn buffered_example_rechecks_explicit_grant_and_revocation_on_one_warmed_cache() {
    for format in [git_zero::ObjectFormat::Sha1, git_zero::ObjectFormat::Sha256] {
        compare_grants(format);
    }
}

fn compare_grants(format: git_zero::ObjectFormat) {
    use git_zero::{File, Repository, Snapshot};
    let files = [File {
        name: "secret-to-strict-wants",
        contents: b"visible authorized bytes\0\xff",
    }];
    let (mut tree, mut commit) = ([0; 256], [0; 512]);
    let snapshot = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
    let (mut metadata, mut by_id) = ([None; 3], [0; 3]);
    let repository = ValidatedRepository::new(
        &snapshot,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let want = snapshot.object_id(2).unwrap();
    for version in 0..=2 {
        compare_grant_version(&repository, want, format, version);
    }
}

fn compare_grant_version(
    repository: &ValidatedRepository<'_, '_, impl git_zero::Repository + Sync>,
    want: git_zero::ObjectId,
    format: git_zero::ObjectFormat,
    version: u8,
) {
    let body = protocol::body(format, version, &[want], &[], &[], true, "");
    let request = protocol::wire(&body, version);
    let mut storage = selected_http::Storage::new(8192).unwrap();
    let mut resident = [0; 8192];
    let mut cache = FixedCache::new(&mut resident);
    let grant = WantOptions::all_visible_reachable(1);
    for (options, expected) in [
        (WantOptions::STRICT, (400, None)),
        (grant, (200, Some(Outcome::Miss))),
        (grant, (200, Some(Outcome::Hit))),
        (WantOptions::STRICT, (400, None)),
        (WantOptions::all_visible_reachable(0), (413, None)),
        (grant, (200, Some(Outcome::Hit))),
    ] {
        let before = cache.statistics();
        exchange(repository, &mut cache, &mut storage, options, &request).assert(expected);
        if expected.0 != 200 {
            assert_eq!(
                cache.statistics(),
                before,
                "denied request consulted the cache"
            );
        }
    }
}
