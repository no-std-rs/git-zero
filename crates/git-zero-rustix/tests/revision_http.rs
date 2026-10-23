#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../examples/support/fixture.rs"]
mod fixture;
use git_zero::{
    Error, RefRepository, Repository, RevisionLimits, ValidatedRepository,
    http::{self, RevisionBuffers},
    io::SliceWriter,
};
use git_zero_rustix::{
    FileRepository, accept, listen, local_port, read_request, respond, respond_stream,
};
use std::{
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained revision oracle: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
struct Stop<'a>(&'a AtomicBool, u16);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.1));
    }
}
fn python(root: &Path, phase: &str, format: &str, url: Option<&str>) {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/server_revisions.py"
        ))
        .arg(phase)
        .arg("--root")
        .arg(root)
        .arg("--object-format")
        .arg(format);
    if let Some(url) = url {
        command.arg("--zero-url").arg(url);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{phase}/{format}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn respond_error(socket: &impl rustix::fd::AsFd, error: Error, control: &mut [u8]) {
    let mut body = SliceWriter::new(control);
    http::write_upload_error(error, &mut body).unwrap();
    respond(
        socket,
        http::Response {
            status: if matches!(error, Error::BufferTooSmall | Error::LimitExceeded) {
                413
            } else {
                400
            },
            content_type: "application/x-git-upload-pack-result",
        },
        body.as_bytes(),
    )
    .unwrap();
}
fn serve<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    listener: &impl rustix::fd::AsFd,
    stopped: &AtomicBool,
) {
    let mut input = [0; 8192];
    let mut control = [0; 8192];
    let mut staging = [0; 31];
    let mut buffers = RevisionBuffers::<64, { fixture::OBJECTS }, 17, 512>::new();
    loop {
        let socket = accept(listener).unwrap();
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let mut request = read_request(&socket, &mut input).unwrap();
        request.path = request.path.strip_prefix("/repo.git/").unwrap();
        let result = allocations::without_allocations(|| {
            http::stream::ResponseEncoder::with_revision_storage(
                repository,
                request,
                buffers.storage(1_048_576, RevisionLimits { max_exclusions: 8 }),
                &mut control,
            )
        });
        match result {
            Ok(mut encoder) => {
                respond_stream(&socket, repository, &mut encoder, &mut staging).unwrap()
            }
            Err(error) => respond_error(&socket, error, &mut control),
        }
    }
}
fn check_revision_format(format: &str) {
    let root = std::env::temp_dir().join(format!(
        "git-zero-revision-http-{}-{format}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    python(&root, "prepare", format, None);
    let fixture = fixture::Fixture::load(&root).unwrap();
    let (id, head) = fixture.head().unwrap();
    let refs = fixture.references().unwrap();
    let files = FileRepository::new(id, fixture.objects());
    let source = RefRepository::new(&files, head, &refs).unwrap();
    let mut metadata = [None; fixture::OBJECTS];
    let mut by_id = [0; fixture::OBJECTS];
    let repository = allocations::without_allocations(|| {
        ValidatedRepository::new(
            &source,
            &mut metadata,
            &mut by_id,
            &mut [0; 17],
            &mut [0; 512],
        )
    })
    .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| serve(&repository, &listener, &stopped));
        let _stop = Stop(&stopped, port);
        python(
            &root,
            "compare",
            format,
            Some(&format!("http://127.0.0.1:{port}/repo.git")),
        );
    });
}

#[test]
fn real_git_revision_history_matches_http_backend_in_both_formats_and_all_protocols() {
    for format in ["sha1", "sha256"] {
        check_revision_format(format);
    }
}
