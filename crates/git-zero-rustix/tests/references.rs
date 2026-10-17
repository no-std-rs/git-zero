#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../examples/support/fixture.rs"]
mod fixture;

use git_zero::{
    Error, ObjectId, RefRepository, Repository, ValidatedRepository,
    http::{self, Response, SelectionStorage, stream::ResponseEncoder},
    io::SliceWriter,
};
use git_zero_rustix::{
    AcceptedSocketTimeouts, FileRepository, RequestLimits, accept_with_timeouts, listen,
    local_port, read_request_with_limits, respond,
};
use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use fixture::{Fixture, OBJECTS as OBJECT_CAPACITY};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained ref fixtures and traces: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

struct Stop<'a> {
    stopped: &'a AtomicBool,
    port: u16,
}
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn python(root: &Path, phase: &str, state: &str, object_format: &str, url: Option<&str>) {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/server_references.py"
        ))
        .arg(phase)
        .arg("--root")
        .arg(root)
        .arg("--state")
        .arg(state)
        .arg("--object-format")
        .arg(object_format);
    if let Some(url) = url {
        command.arg("--zero-url").arg(url);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{phase} {state}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn serve<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    listener: &impl rustix::fd::AsFd,
    stopped: &AtomicBool,
    prepared: &AtomicUsize,
) {
    let mut request_storage = [0; 4096];
    let mut response_storage = [0; 8192];
    let mut staging = [0; 37];
    let mut wants = [ObjectId::Sha1([0; 20]); 16];
    let mut metadata = [None; OBJECT_CAPACITY];
    let mut indices = [0; OBJECT_CAPACITY];
    let mut read = [0; 17];
    let mut parse = [0; 128];
    let mut selection = SelectionStorage {
        wants: &mut wants,
        objects: &mut metadata,
        indices: &mut indices,
        read: &mut read,
        parse: &mut parse,
        graph_read_limit: 1_048_576,
    };
    let timeouts = AcceptedSocketTimeouts {
        read: Some(Duration::from_secs(2)),
        write: Some(Duration::from_secs(2)),
    };
    let limits = RequestLimits {
        max_wire_bytes: Some(4096),
        max_body_bytes: Some(2048),
    };
    loop {
        let socket =
            allocations::without_allocations(|| accept_with_timeouts(listener, timeouts)).unwrap();
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        match prepare_response(
            repository,
            &socket,
            prepared,
            limits,
            &mut request_storage,
            &mut selection,
            &mut response_storage,
        ) {
            Ok(mut encoder) => {
                git_zero_rustix::respond_stream(&socket, repository, &mut encoder, &mut staging)
                    .unwrap()
            }
            Err(error) => respond_failure(&socket, error, &mut response_storage),
        }
    }
}

fn prepare_response<'a, R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    socket: &impl rustix::fd::AsFd,
    prepared: &AtomicUsize,
    limits: RequestLimits,
    request_storage: &mut [u8],
    selection: &'a mut SelectionStorage<'_>,
    response_storage: &'a mut [u8],
) -> Result<ResponseEncoder<'a>, Error> {
    let mut request = allocations::without_allocations(|| {
        read_request_with_limits(socket, request_storage, limits)
    })?;
    prepared.fetch_add(1, Ordering::SeqCst);
    request.path = request.path.strip_prefix("/repo.git/").unwrap();
    allocations::without_allocations(|| {
        ResponseEncoder::with_selection(repository, request, selection, response_storage)
    })
}

fn respond_failure(socket: &impl rustix::fd::AsFd, error: Error, storage: &mut [u8]) {
    let status = match error {
        Error::BufferTooSmall | Error::LimitExceeded => 413,
        Error::Backend => 500,
        _ => 400,
    };
    let mut failure = SliceWriter::new(storage);
    http::write_upload_error(error, &mut failure).unwrap();
    respond(
        socket,
        Response {
            status,
            content_type: "application/x-git-upload-pack-result",
        },
        failure.as_bytes(),
    )
    .unwrap();
}

fn compare(state: &str) {
    compare_format(state, "sha1");
}

fn compare_format(state: &str, object_format: &str) {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "git-zero-refs-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    python(&root, "prepare", state, object_format, None);
    let fixture = Fixture::load(&root).unwrap();
    let (id, head) = fixture.head().unwrap();
    let refs = fixture.references().unwrap();
    let files = FileRepository::new(id, fixture.objects());
    let repository = RefRepository::new(&files, head, &refs).unwrap();
    let mut metadata = [None; OBJECT_CAPACITY];
    let mut by_id = [0; OBJECT_CAPACITY];
    let repository = allocations::without_allocations(|| {
        ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 17],
            &mut [0; 128],
        )
        .unwrap()
    });
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let stopped = AtomicBool::new(false);
    let prepared = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| serve(&repository, &listener, &stopped, &prepared));
        let _stop = Stop {
            stopped: &stopped,
            port,
        };
        // Declared over-quota bodies are rejected before protocol/repository
        // preparation; the unchanged Git oracle then proves host recovery.
        reject_admission(port);
        assert_eq!(prepared.load(Ordering::SeqCst), 0);
        python(
            &root,
            "compare",
            state,
            object_format,
            Some(&format!("http://127.0.0.1:{port}/repo.git")),
        );
    });
}

fn reject_admission(port: u16) {
    use std::io::{Read, Write};
    for length in [2049, 4096] {
        let mut peer = TcpStream::connect(("127.0.0.1", port)).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(
            peer,
            "POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: {length}\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        peer.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.0 413 "), "{response}");
        assert!(!response.contains("PACK"));
    }
}

const EMPTY_BLOB_ID: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

// Resource parsing is tested independently of downstream object-ID/body and
// graph validation, which the unchanged real-Git oracles cover below.
fn raw_fixture(name: &str) -> Cleanup {
    let root = std::env::temp_dir().join(format!("git-zero-loader-{name}-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let cleanup = Cleanup(root);
    let root = &cleanup.0;
    std::fs::create_dir(root.join("raw")).unwrap();
    std::fs::write(root.join("raw").join(EMPTY_BLOB_ID), []).unwrap();
    std::fs::write(root.join("objects.txt"), format!("{EMPTY_BLOB_ID}\tblob\n")).unwrap();
    std::fs::write(root.join("refs.txt"), "").unwrap();
    std::fs::write(root.join("head.txt"), format!("{EMPTY_BLOB_ID}\n\n")).unwrap();
    cleanup
}

#[test]
fn raw_fixture_object_count_accepts_256_and_rejects_257() {
    let cleanup = raw_fixture("objects");
    let root = &cleanup.0;
    let row = format!("{EMPTY_BLOB_ID}\tblob\n");
    let objects = root.join("objects.txt");
    std::fs::write(&objects, row.repeat(OBJECT_CAPACITY)).unwrap();
    assert_eq!(
        Fixture::load(root).unwrap().objects().len(),
        OBJECT_CAPACITY
    );
    std::fs::write(&objects, row.repeat(OBJECT_CAPACITY + 1)).unwrap();
    assert_loader_error(root, "object count exceeds limit");
}

#[test]
fn raw_fixture_bytes_accept_8_mib_and_reject_larger_bodies() {
    let cleanup = raw_fixture("bytes");
    let root = &cleanup.0;
    let file = std::fs::File::options()
        .write(true)
        .open(root.join("raw").join(EMPTY_BLOB_ID))
        .unwrap();
    file.set_len(fixture::OBJECT_BYTES).unwrap();
    assert_eq!(
        Fixture::load(root).unwrap().objects()[0].info().size,
        fixture::OBJECT_BYTES
    );
    file.set_len(fixture::OBJECT_BYTES + 1).unwrap();
    assert_loader_error(root, "object bytes exceed limit");
}

#[test]
fn raw_fixture_metadata_accepts_64_kib_and_rejects_larger_files() {
    let cleanup = raw_fixture("metadata");
    let root = &cleanup.0;
    let head = root.join("head.txt");
    let mut text = format!("{EMPTY_BLOB_ID}\n\n").into_bytes();
    text.resize(65_536, b'\n');
    std::fs::write(&head, &text).unwrap();
    assert!(Fixture::load(root).unwrap().head().is_ok());
    text.push(b'\n');
    std::fs::write(&head, &text).unwrap();
    assert_loader_error(root, "metadata exceeds limit");
}

#[test]
fn raw_fixture_ref_count_accepts_32_and_rejects_33() {
    let cleanup = raw_fixture("refs");
    let root = &cleanup.0;
    let refs = root.join("refs.txt");
    let reference = format!("refs/heads/main\t{EMPTY_BLOB_ID}\t-\t-\n");
    std::fs::write(&refs, reference.repeat(32)).unwrap();
    assert_eq!(Fixture::load(root).unwrap().references().unwrap().len(), 32);
    std::fs::write(&refs, reference.repeat(33)).unwrap();
    assert!(
        Fixture::load(root)
            .unwrap()
            .references()
            .unwrap_err()
            .to_string()
            .contains("ref count exceeds limit")
    );
}

#[test]
fn raw_fixture_objects_accept_regular_files_and_reject_symlinks() {
    let cleanup = raw_fixture("symlink");
    let root = &cleanup.0;
    let raw = root.join("raw").join(EMPTY_BLOB_ID);
    assert_eq!(Fixture::load(root).unwrap().objects().len(), 1);
    std::fs::rename(&raw, root.join("backing-object")).unwrap();
    std::os::unix::fs::symlink(root.join("backing-object"), &raw).unwrap();
    assert_loader_error(root, "file object");
}

fn assert_loader_error(root: &Path, expected: &str) {
    let error = Fixture::load(root)
        .err()
        .expect("oversized or unsafe fixture accepted");
    assert!(error.to_string().contains(expected), "{error}");
}

#[test]
fn real_git_symbolic_refs_tags_and_selected_closures_match_git_servers() {
    compare("symbolic");
}

#[test]
fn real_git_symbolic_head_through_a_lightweight_tag_matches_git_servers() {
    compare("symbolic-tag");
}

#[test]
fn real_git_detached_head_beyond_ref_closures_matches_git_servers() {
    compare("detached");
}

#[test]
fn real_git_unborn_head_with_other_refs_matches_git_servers() {
    compare("unborn");
}

#[test]
fn real_git_sha256_symbolic_refs_tags_and_selected_closures_match_git_servers() {
    compare_format("symbolic", "sha256");
}

#[test]
fn real_git_sha256_symbolic_head_through_a_lightweight_tag_matches_git_servers() {
    compare_format("symbolic-tag", "sha256");
}

#[test]
fn real_git_sha256_detached_head_beyond_ref_closures_matches_git_servers() {
    compare_format("detached", "sha256");
}

#[test]
fn real_git_sha256_unborn_head_with_other_refs_matches_git_servers() {
    compare_format("unborn", "sha256");
}

#[test]
fn real_git_sha256_empty_repositories_match_git_servers() {
    compare_format("empty", "sha256");
    compare_format("empty-custom", "sha256");
}

#[test]
fn real_git_sha1_empty_repositories_match_git_servers() {
    compare_format("empty", "sha1");
    compare_format("empty-custom", "sha1");
}

#[test]
fn real_git_deep_binary_modes_and_packed_fixtures_match_git_servers_in_both_formats() {
    compare_format("rich", "sha1");
    compare_format("rich", "sha256");
}
