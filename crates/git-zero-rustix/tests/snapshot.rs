#[path = "../../../tests/common/mod.rs"]
mod common;
#[path = "common/git.rs"]
mod git_support;
#[path = "common/server.rs"]
mod server;
#[path = "../../../tests/common/streamed.rs"]
mod streamed;

use git_zero::{
    IndexedRepository, ObjectId, Repository, Snapshot, ValidatedRepository,
    client::{self, Transport},
    http,
    io::SliceWriter,
};
use git_zero_rustix::{HttpTransport, accept, listen, local_port, read_request, respond};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    process::Output,
};

fn git(args: &[&str], dumb: bool) -> Output {
    git_support::run("git", args, dumb)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_git_clones_nested_modes_empty_trees_and_custom_refs() {
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let repo = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit)
        .unwrap()
        .with_refs(common::HEAD_REF, common::REFS)
        .unwrap();
    let mut objects = [None; 6];
    let mut by_id = [0; 6];
    let repo = IndexedRepository::new(&repo, &mut objects, &mut by_id).unwrap();
    let mut validated_objects = [None; 6];
    let mut validated_index = [0; 6];
    let repo = ValidatedRepository::new(
        &repo,
        &mut validated_objects,
        &mut validated_index,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let url = format!("http://127.0.0.1:{port}/repo.git");
    let directory =
        std::env::temp_dir().join(format!("git-zero-nested-{}-{port}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let _cleanup = Cleanup(directory.clone());

    let mut request_storage = [0; 4096];
    let mut response_storage = [0; 8192];
    server::while_serving(
        port,
        || accept(&listener).unwrap(),
        |socket| serve_nested(&repo, socket, &mut request_storage, &mut response_storage),
        |port| check_nested(&repo, port, &url, &directory),
    );
}

#[test]
fn real_git_clones_generated_objects_with_bounded_read_scratch() {
    const SIZE: usize = 1_048_577;
    let repo = streamed::GeneratedRepository::new(SIZE as u64, 113);
    let mut objects = [None; 3];
    let mut by_id = [0; 3];
    let repo =
        ValidatedRepository::new(&repo, &mut objects, &mut by_id, &mut [0; 17], &mut [0; 128])
            .unwrap();
    let listener = listen(0).unwrap();
    let port = local_port(&listener).unwrap();
    let url = format!("http://127.0.0.1:{port}/repo.git");
    let directory =
        std::env::temp_dir().join(format!("git-zero-generated-{}-{port}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let _cleanup = Cleanup(directory.clone());

    let mut request_storage = [0; 4096];
    let mut control = [0; 4096];
    let mut staging = [0; 17];
    server::while_serving(
        port,
        || accept(&listener).unwrap(),
        |socket| {
            serve_generated(
                &repo,
                socket,
                &mut request_storage,
                &mut control,
                &mut staging,
            )
        },
        |_| check_generated(&repo, &url, &directory, SIZE),
    );
}

fn serve_nested(
    repo: &impl Repository,
    socket: &impl rustix::fd::AsFd,
    request_storage: &mut [u8],
    response_storage: &mut [u8],
) {
    let mut request = read_request(socket, request_storage).unwrap();
    request.path = request.path.strip_prefix("/repo.git/").unwrap();
    let mut output = SliceWriter::new(response_storage);
    let response = http::serve(repo, request, &mut output).unwrap();
    respond(socket, response, output.as_bytes()).unwrap();
}

fn check_nested(repo: &impl Repository, port: u16, url: &str, directory: &Path) {
    let mut wire = [0; 4096];
    let mut response = [0; 256];
    let mut transport = HttpTransport {
        address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
        repository_path: "/repo.git",
        wire_storage: &mut wire,
    };
    for path in [
        "refs/heads/releases/current%23%25%C3%A9+",
        "refs/tags/snapshot%2523",
    ] {
        let (status, length) = transport
            .request(
                http::Request {
                    content_type: None,
                    git_protocol: None,
                    method: "GET",
                    path,
                    query: None,
                    body: &[],
                },
                &mut response,
            )
            .unwrap();
        assert_eq!(status, 200, "{path}");
        assert_eq!(&response[..length], format!("{}\n", repo.head()).as_bytes());
    }
    for dumb in [false, true] {
        let clone = directory.join(if dumb { "dumb" } else { "smart" });
        let clone_path = clone.to_str().unwrap();
        git(&["clone", "-q", url, clone_path], dumb);
        if dumb {
            // Git's dumb walker can treat the built-in empty tree as already
            // available, leaving it absent on disk (observed with Git 2.53).
            // Fetch the actual server object before the strict disk check.
            // See Git's odb.c find_cached_object and walker.c process.
            let empty_tree =
                ObjectId::from_hex(b"4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();
            let mut wire = [0; 1024];
            let mut response = [0; 256];
            let mut transport = HttpTransport {
                address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
                repository_path: "/repo.git",
                wire_storage: &mut wire,
            };
            let loose = client::fetch_loose(&mut transport, empty_tree, &mut response).unwrap();
            let objects = clone.join(".git/objects/4b");
            std::fs::create_dir_all(&objects).unwrap();
            std::fs::write(
                objects.join("825dc642cb6eb9a060e54bf8d69288fbee4904"),
                loose,
            )
            .unwrap();
        }
        git(&["-C", clone_path, "fsck", "--strict"], dumb);
        git(&["-C", clone_path, "fetch", "origin"], dumb);
        assert_eq!(
            git(&["-C", clone_path, "rev-parse", "HEAD"], dumb).stdout,
            format!("{}\n", repo.head()).as_bytes()
        );
        assert_eq!(
            git(&["-C", clone_path, "symbolic-ref", "HEAD"], dumb).stdout,
            format!("{}\n", common::HEAD_REF).as_bytes()
        );
        assert_eq!(
            git(&["-C", clone_path, "rev-list", "--count", "HEAD"], dumb).stdout,
            b"1\n"
        );
        assert_eq!(
            std::fs::read(clone.join("a/run")).unwrap(),
            b"#!/bin/sh\nprintf 'snapshot\\n'\n"
        );
        assert_eq!(
            std::fs::read_link(clone.join("a/target")).unwrap(),
            PathBuf::from("run")
        );
        assert_eq!(
            std::fs::read_link(clone.join("b/target")).unwrap(),
            PathBuf::from("run")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(clone.join("a/run"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
            assert_eq!(
                std::fs::metadata(clone.join("a.c"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        let tree = git(&["-C", clone_path, "ls-tree", "-rt", "HEAD"], dumb);
        let entries: Vec<_> = std::str::from_utf8(&tree.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let (metadata, path) = line.split_once('\t').unwrap();
                (metadata.split_whitespace().next().unwrap(), path)
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("100644", "a.c"),
                ("040000", "a"),
                ("100755", "a/run"),
                ("120000", "a/target"),
                ("040000", "b"),
                ("100755", "b/run"),
                ("120000", "b/target"),
                ("040000", "empty-again"),
                ("040000", "empty"),
            ]
        );
        let refs = git(&["ls-remote", url], dumb);
        for name in common::REFS {
            assert!(
                std::str::from_utf8(&refs.stdout)
                    .unwrap()
                    .contains(&format!("{}\t{name}\n", repo.head()))
            );
        }
    }
}

fn serve_generated(
    repo: &impl Repository,
    socket: &impl rustix::fd::AsFd,
    request_storage: &mut [u8],
    control: &mut [u8],
    staging: &mut [u8],
) {
    let mut request = read_request(socket, request_storage).unwrap();
    request.path = request.path.strip_prefix("/repo.git/").unwrap();
    let mut encoder = http::stream::ResponseEncoder::new(repo, request, control).unwrap();
    git_zero_rustix::respond_stream(socket, repo, &mut encoder, staging).unwrap();
}

fn check_generated(repo: &impl Repository, url: &str, directory: &Path, size: usize) {
    for dumb in [false, true] {
        let clone = directory.join(if dumb { "dumb" } else { "smart" });
        let clone_path = clone.to_str().unwrap();
        git(&["clone", "-q", url, clone_path], dumb);
        git(&["-C", clone_path, "fsck", "--strict"], dumb);
        git(&["-C", clone_path, "fetch", "origin"], dumb);
        assert_eq!(
            git(&["-C", clone_path, "rev-parse", "HEAD"], dumb).stdout,
            format!("{}\n", repo.head()).as_bytes()
        );
        let path = clone.join("generated.bin");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size as u64);
        let mut file = std::fs::File::open(path).unwrap();
        let mut storage = [0; 1024];
        let mut offset = 0_u64;
        loop {
            use std::io::Read;
            let length = file.read(&mut storage).unwrap();
            if length == 0 {
                break;
            }
            for (index, &byte) in storage[..length].iter().enumerate() {
                assert_eq!(byte, streamed::byte_at(offset + index as u64));
            }
            offset += length as u64;
        }
        assert_eq!(offset, size as u64);
    }
}
