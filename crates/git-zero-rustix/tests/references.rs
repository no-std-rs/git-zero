#[path = "../../../tests/common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Head, ObjectId, ObjectKind, RefRepository, Reference, Repository, ValidatedRepository,
    http::{self, Response, SelectionStorage},
    io::SliceWriter,
};
use git_zero_rustix::{
    FileObject, FileRepository, accept, listen, local_port, read_request, respond,
};
use std::{
    ffi::CString,
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const OBJECT_CAPACITY: usize = 128;

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

fn python(root: &Path, phase: &str, state: &str, url: Option<&str>) {
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
        .arg(state);
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

fn kind(value: &str) -> ObjectKind {
    match value {
        "commit" => ObjectKind::Commit,
        "tree" => ObjectKind::Tree,
        "blob" => ObjectKind::Blob,
        "tag" => ObjectKind::Tag,
        _ => panic!("unexpected Git object kind {value}"),
    }
}

fn objects(root: &Path) -> Vec<FileObject> {
    std::fs::read_to_string(root.join("objects.txt"))
        .unwrap()
        .lines()
        .map(|line| {
            let (hex, value) = line.split_once('\t').unwrap();
            let id = ObjectId::from_hex(hex.as_bytes()).unwrap();
            let path = root.join("raw").join(hex);
            let fd = rustix::fs::open(
                CString::new(path.as_os_str().as_encoded_bytes())
                    .unwrap()
                    .as_c_str(),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )
            .unwrap();
            FileObject::from_fd(fd, kind(value), id).unwrap()
        })
        .collect()
}

fn references(text: &str) -> Vec<Reference<'_>> {
    text.lines()
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            Reference {
                name: fields[0],
                id: ObjectId::from_hex(fields[1].as_bytes()).unwrap(),
                symbolic_target: (fields[2] != "-").then_some(fields[2]),
                peeled: if fields[3] == "-" {
                    None
                } else {
                    Some(ObjectId::from_hex(fields[3].as_bytes()).unwrap())
                },
            }
        })
        .collect()
}

fn serve<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    listener: &impl rustix::fd::AsFd,
    stopped: &AtomicBool,
) {
    let mut request_storage = [0; 4096];
    let mut response_storage = [0; 262_144];
    let mut wants = [ObjectId([0; 20]); 16];
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
    loop {
        let socket = accept(listener).unwrap();
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let mut request = read_request(&socket, &mut request_storage).unwrap();
        request.path = request.path.strip_prefix("/repo.git/").unwrap();
        let mut output = SliceWriter::new(&mut response_storage);
        let result = allocations::without_allocations(|| {
            http::serve_with_selection(repository, request, &mut selection, &mut output)
        });
        match result {
            Ok(response) => respond(&socket, response, output.as_bytes()).unwrap(),
            Err(error) => {
                let status = match error {
                    Error::BufferTooSmall | Error::LimitExceeded => 413,
                    Error::Backend => 500,
                    _ => 400,
                };
                respond(
                    &socket,
                    Response {
                        status,
                        content_type: "text/plain",
                    },
                    b"invalid request\n",
                )
                .unwrap();
            }
        }
    }
}

fn compare(state: &str) {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "git-zero-refs-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    python(&root, "prepare", state, None);
    let objects = objects(&root);
    let refs_text = std::fs::read_to_string(root.join("refs.txt")).unwrap();
    let refs = references(&refs_text);
    let head_text = std::fs::read_to_string(root.join("head.txt")).unwrap();
    let mut lines = head_text.lines();
    let id = ObjectId::from_hex(lines.next().unwrap().as_bytes()).unwrap();
    let target = lines.next().unwrap_or("");
    let head = if id == ObjectId([0; 20]) {
        Head::Unborn(target)
    } else if target.is_empty() {
        Head::Detached(id)
    } else {
        Head::Symbolic { target, id }
    };
    let files = FileRepository::new(id, &objects);
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
    std::thread::scope(|scope| {
        scope.spawn(|| serve(&repository, &listener, &stopped));
        let _stop = Stop {
            stopped: &stopped,
            port,
        };
        python(
            &root,
            "compare",
            state,
            Some(&format!("http://127.0.0.1:{port}/repo.git")),
        );
    });
}

#[test]
fn real_git_symbolic_refs_tags_and_selected_closures_match_git_servers() {
    compare("symbolic");
}

#[test]
fn real_git_detached_head_beyond_ref_closures_matches_git_servers() {
    compare("detached");
}

#[test]
fn real_git_unborn_head_with_other_refs_matches_git_servers() {
    compare("unborn");
}
