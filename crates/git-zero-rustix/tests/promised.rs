#[path = "../../../tests/common/allocations.rs"]
mod allocations;
#[path = "../examples/support/fixture.rs"]
mod fixture;
#[path = "common/server.rs"]
mod server;
use git_zero::{
    RefRepository, Repository, ValidatedRepository,
    http::{self, WantOptions},
};
use git_zero_rustix::{
    FileRepository, accept, listen, local_port, read_request, respond, respond_stream,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained promised fixtures: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
fn checked(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
fn serve(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    socket: &impl rustix::fd::AsFd,
    request_storage: &mut [u8],
    control: &mut [u8],
    staging: &mut [u8],
    selection: &mut http::SelectionBuffers<64, 64, 512, 512>,
) {
    let mut request = read_request(socket, request_storage).unwrap();
    request.path = request.path.strip_prefix("/repo.git/").unwrap();
    let result = allocations::without_allocations(|| {
        http::stream::ResponseEncoder::with_selection_storage_policy(
            repository,
            request,
            selection.storage(1_048_576),
            WantOptions::all_visible_reachable(64),
            control,
        )
    });
    match result {
        Ok(mut encoder) => allocations::without_allocations(|| {
            respond_stream(socket, repository, &mut encoder, staging)
        })
        .unwrap(),
        Err(error) => respond(
            socket,
            http::Response {
                status: 400,
                content_type: "text/plain",
            },
            format!("{error:?}\n").as_bytes(),
        )
        .unwrap(),
    }
}

#[test]
fn git_partial_clone_and_lazy_checkout_match_git_in_both_formats_and_all_protocols() {
    let root = std::env::temp_dir().join(format!("git-zero-promised-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    let worker =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cloudflare-worker/test");
    checked(
        Command::new("python3")
            .arg(worker.join("fixture.py"))
            .arg("--root")
            .arg(root.join("fixtures")),
    );
    for format in ["sha1", "sha256"] {
        compare(&root, &worker, format);
    }
}

fn compare(root: &Path, worker: &Path, format: &str) {
    let fixture =
        fixture::Fixture::load(&root.join("fixtures").join(format).join("export")).unwrap();
    let (id, head) = fixture.head().unwrap();
    let refs: Vec<_> = fixture
        .references()
        .unwrap()
        .into_iter()
        .filter(|reference| reference.name != "refs/tags/inner")
        .collect();
    let files = FileRepository::new(id, fixture.objects());
    let repository = RefRepository::new(&files, head, &refs).unwrap();
    let mut metadata = [None; 64];
    let mut by_id = [0; 64];
    let repository = allocations::without_allocations(|| {
        ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 512],
            &mut [0; 512],
        )
    })
    .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let mut request = [0; 65536];
    let mut control = [0; 8192];
    let mut staging = [0; 37];
    let mut selection = http::SelectionBuffers::<64, 64, 512, 512>::new();
    server::while_serving(
        port,
        || accept(&listener).unwrap(),
        |socket| {
            serve(
                &repository,
                socket,
                &mut request,
                &mut control,
                &mut staging,
                &mut selection,
            )
        },
        |port| compare_client(root, worker, format, port),
    );
}

fn compare_client(root: &Path, worker: &Path, format: &str, port: u16) {
    checked(
        Command::new("node")
            .arg(worker.join("promised-native.mjs"))
            .arg(root)
            .arg(format)
            .arg(format!("http://127.0.0.1:{port}")),
    );
}
