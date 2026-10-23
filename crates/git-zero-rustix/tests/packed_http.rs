#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../examples/support/fixture.rs"]
mod fixture;
use git_zero::{
    RefRepository, Repository, ValidatedRepository,
    http::{
        self,
        dumb::{LooseObjects, Publication},
    },
    pack::IndexEntry,
};
use git_zero_rustix::{
    FilePublication, FileRepository, HeaderLimits, RequestLimits, RequestPolicy, accept, listen,
    local_port, read_request_with_policy, respond_file_published, respond_published,
    respond_stream,
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
            eprintln!("Retained packed HTTP fixture: {}", self.0.display());
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
fn python(root: &Path, phase: &str, format: &str, state: &str, url: Option<&str>) {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/server_packed.py"
        ))
        .arg(phase)
        .arg("--root")
        .arg(root)
        .arg("--object-format")
        .arg(format)
        .arg("--state")
        .arg(state);
    if let Some(url) = url {
        command.arg("--zero-url").arg(url);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{phase}/{format}/{state}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
struct FileSink(std::fs::File);
impl git_zero::io::Write for FileSink {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        std::io::Write::write_all(&mut self.0, bytes).map_err(|_| git_zero::Error::Backend)
    }
}
fn open_file(path: &Path) -> rustix::fd::OwnedFd {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    rustix::fs::open(
        path.as_c_str(),
        rustix::fs::OFlags::RDONLY,
        rustix::fs::Mode::empty(),
    )
    .unwrap()
}
fn serve<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    root: &Path,
    listener: &impl rustix::fd::AsFd,
    stopped: &AtomicBool,
) {
    let mut packed_metadata = [IndexEntry::EMPTY; fixture::OBJECTS];
    let mut mixed_metadata = [IndexEntry::EMPTY; fixture::OBJECTS];
    let packed = allocations::without_allocations(|| {
        Publication::new(
            repository,
            &mut packed_metadata,
            &mut [0; 13],
            LooseObjects::Disabled,
        )
    })
    .unwrap();
    let mixed = allocations::without_allocations(|| {
        Publication::new(
            repository,
            &mut mixed_metadata,
            &mut [0; 29],
            LooseObjects::Enabled,
        )
    })
    .unwrap();
    assert_eq!(packed.checksum(), mixed.checksum());
    let pack_path = root.join("native-publication.pack");
    let index_path = root.join("native-publication.idx");
    let mut entries = [IndexEntry::EMPTY; fixture::OBJECTS];
    let mut pack_sink = FileSink(std::fs::File::create(&pack_path).unwrap());
    let mut index_sink = FileSink(std::fs::File::create(&index_path).unwrap());
    git_zero::pack::write_indexed_with_scratch(
        repository,
        None,
        &mut entries,
        &mut [0; 1024],
        &mut pack_sink,
        &mut index_sink,
    )
    .unwrap();
    drop(pack_sink);
    drop(index_sink);
    let pack_fd = open_file(&pack_path);
    let index_fd = open_file(&index_path);
    let file_publication = allocations::without_allocations(|| {
        FilePublication::from_fds(
            repository,
            &mut entries,
            pack_fd,
            index_fd,
            &mut [0; 1024],
            &mut [0; 1024],
            LooseObjects::Disabled,
        )
    })
    .unwrap();
    let mut input = [0; 8192];
    let mut control = [0; 8192];
    let mut staging = [0; 1024];
    let mut selection = http::SelectionBuffers::<64, { fixture::OBJECTS }, 17, 512>::new();
    loop {
        let socket = accept(listener).unwrap();
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let mut request = read_request_with_policy(
            &socket,
            &mut input,
            RequestPolicy {
                request: RequestLimits {
                    max_wire_bytes: Some(8192),
                    max_body_bytes: Some(4096),
                },
                headers: HeaderLimits {
                    max_bytes: Some(4096),
                    max_fields: Some(16),
                    max_name_bytes: Some(64),
                    max_value_bytes: Some(1024),
                },
            },
        )
        .unwrap();
        if let Some(path) = request.path.strip_prefix("/files/repo.git/") {
            request.path = path;
            let cursor =
                allocations::without_allocations(|| file_publication.prepare(request)).unwrap();
            if let Some(mut cursor) = cursor {
                allocations::without_allocations(|| {
                    respond_file_published(&socket, &mut cursor, &mut staging)
                })
                .unwrap();
            } else {
                let mut cursor = http::stream::ResponseEncoder::with_selection_storage(
                    repository,
                    request,
                    selection.storage(1_048_576),
                    &mut control,
                )
                .unwrap();
                respond_stream(&socket, repository, &mut cursor, &mut staging).unwrap();
            }
            continue;
        }
        let publication = if let Some(path) = request.path.strip_prefix("/packed/repo.git/") {
            request.path = path;
            &packed
        } else {
            request.path = request.path.strip_prefix("/mixed/repo.git/").unwrap();
            &mixed
        };
        let cursor = allocations::without_allocations(|| publication.prepare(request)).unwrap();
        if let Some(mut cursor) = cursor {
            allocations::without_allocations(|| {
                respond_published(&socket, &mut cursor, &mut staging)
            })
            .unwrap();
        } else {
            // Model a mixed namespace with one loose blob and a complete pack.
            // Git may skip an empty subtree when every object is available loose;
            // a real mixed layout needs at least one missing loose object to make
            // the client consult objects/info/packs and download the complete pack.
            if request.path.starts_with("objects/") {
                let blob = (0..repository.object_count())
                    .find(|&index| {
                        repository.object_info(index).unwrap().kind == git_zero::ObjectKind::Blob
                    })
                    .unwrap();
                let id = repository.object_id(blob).unwrap().to_string();
                let loose = format!("objects/{}/{}", &id[..2], &id[2..]);
                if request.path != loose {
                    git_zero_rustix::respond(
                        &socket,
                        http::Response {
                            status: 404,
                            content_type: "text/plain",
                        },
                        b"",
                    )
                    .unwrap();
                    continue;
                }
            }
            let mut cursor = http::stream::ResponseEncoder::with_selection_storage(
                repository,
                request,
                selection.storage(1_048_576),
                &mut control,
            )
            .unwrap();
            respond_stream(&socket, repository, &mut cursor, &mut staging).unwrap();
        }
    }
}
fn check(format: &str, state: &str) {
    let root = std::env::temp_dir().join(format!(
        "git-zero-packed-http-{}-{format}-{state}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    python(&root, "prepare", format, state, None);
    let fixture = fixture::Fixture::load(&root).unwrap();
    let (id, head) = fixture.head().unwrap();
    let refs = fixture.references().unwrap();
    let files = FileRepository::new(id, fixture.objects());
    let source = RefRepository::new(&files, head, &refs).unwrap();
    let mut metadata = [None; fixture::OBJECTS];
    let mut by_id = [0; fixture::OBJECTS];
    let repository = ValidatedRepository::new(
        &source,
        &mut metadata,
        &mut by_id,
        &mut [0; 31],
        &mut [0; 512],
    )
    .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| serve(&repository, &root, &listener, &stopped));
        let _stop = Stop(&stopped, port);
        python(
            &root,
            "compare",
            format,
            state,
            Some(&format!("http://127.0.0.1:{port}")),
        );
    });
}
#[test]
fn real_git_packed_and_mixed_dumb_clones_need_no_assisted_import_in_either_format() {
    for format in ["sha1", "sha256"] {
        for state in ["symbolic", "detached", "unborn"] {
            check(format, state);
        }
    }
}
