#[path = "common/server.rs"]
mod server;
use git_zero::{
    EmptyRepository, File, Repository, Snapshot, ValidatedRepository, http, io::SliceWriter,
};
use git_zero_rustix::{
    FileObject, FileRepository, accept, listen, local_port, read_request, respond,
};
use std::{
    ffi::CString,
    path::{Path, PathBuf},
    process::Command,
};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "Retained HEAD-state fixtures and traces: {}",
                self.0.display()
            );
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn serve(
    repository: &impl Repository,
    socket: &impl rustix::fd::AsFd,
    request_storage: &mut [u8],
    response_storage: &mut [u8],
) {
    let mut request = read_request(socket, request_storage).unwrap();
    request.path = request.path.strip_prefix("/repo.git/").unwrap();
    let mut output = SliceWriter::new(response_storage);
    let response =
        http::serve_with_scratch(repository, request, &mut [0; 17], &mut output).unwrap();
    respond(socket, response, output.as_bytes()).unwrap();
}

fn compare(repository: &(impl Repository + Sync), state: &str) {
    let mut metadata = vec![None; repository.object_count()];
    let mut by_id = vec![0; repository.object_count()];
    let repository = ValidatedRepository::new(
        repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let root = std::env::temp_dir().join(format!("git-zero-head-{}-{port}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    let mut request_storage = [0; 4096];
    let mut response_storage = [0; 8192];
    server::while_serving(
        port,
        || accept(&listener).unwrap(),
        |socket| {
            serve(
                &repository,
                socket,
                &mut request_storage,
                &mut response_storage,
            )
        },
        |port| compare_client(&root, port, state),
    );
}

fn compare_client(root: &Path, port: u16, state: &str) {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/server_head_states.py"
        ))
        .arg("--root")
        .arg(root)
        .arg("--zero-url")
        .arg(format!("http://127.0.0.1:{port}/repo.git"))
        .arg("--state")
        .arg(state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{state}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_git_empty_and_unborn_servers_match_git_http_backend() {
    compare(
        &EmptyRepository::new("refs/heads/main").unwrap(),
        "unborn-main",
    );
    compare(
        &EmptyRepository::new("refs/heads/new/default").unwrap(),
        "unborn-custom",
    );
}

#[test]
fn real_git_detached_servers_with_and_without_refs_match_git_http_backend() {
    let files = [File {
        name: "README.md",
        contents: b"Hello from an allocation-free in-memory Git repository.\n",
    }];
    let mut tree = [0; 128];
    let mut commit = [0; 256];
    let snapshot = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let directory = std::env::temp_dir().join(format!("git-zero-detached-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let _cleanup = Cleanup(directory.clone());
    let objects: Vec<_> = (0..snapshot.object_count())
        .map(|index| {
            let object = snapshot.object(index).unwrap();
            let path = directory.join(format!("{index}"));
            std::fs::write(&path, object.data).unwrap();
            FileObject::from_fd(
                rustix::fs::open(
                    CString::new(path.as_os_str().as_encoded_bytes())
                        .unwrap()
                        .as_c_str(),
                    rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .unwrap(),
                object.kind,
                object.id(),
            )
            .unwrap()
        })
        .collect();
    for (refs, state) in [
        (&["refs/heads/main"][..], "detached-alias"),
        (&[][..], "detached-only"),
    ] {
        let repository = FileRepository::new(snapshot.head(), &objects)
            .with_detached_head(refs)
            .unwrap();
        compare(&repository, state);
    }
}
