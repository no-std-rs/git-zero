#[path = "../../../tests/common/mod.rs"]
mod common;
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
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicBool, Ordering},
};

fn git(args: &[&str], dumb: bool) -> Output {
    let mut command = Command::new("git");
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.as_encoded_bytes().starts_with(b"GIT_"))
    {
        command.env_remove(name);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    if dumb {
        command.env("GIT_SMART_HTTP", "0");
    }
    let result = command.args(args).output().unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Stop<'a> {
    stopped: &'a AtomicBool,
    port: u16,
}

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake the blocking accept even if a clone/assertion panics.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
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
    let stopped = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut request_storage = [0; 4096];
            let mut response_storage = [0; 8192];
            loop {
                let socket = accept(&listener).unwrap();
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let mut request = read_request(&socket, &mut request_storage).unwrap();
                request.path = request.path.strip_prefix("/repo.git/").unwrap();
                let mut output = SliceWriter::new(&mut response_storage);
                let response = http::serve(&repo, request, &mut output).unwrap();
                respond(&socket, response, output.as_bytes()).unwrap();
            }
        });
        let _stop = Stop {
            stopped: &stopped,
            port,
        };
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
            git(&["clone", "-q", &url, clone_path], dumb);
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
            let refs = git(&["ls-remote", &url], dumb);
            for name in common::REFS {
                assert!(
                    std::str::from_utf8(&refs.stdout)
                        .unwrap()
                        .contains(&format!("{}\t{name}\n", repo.head()))
                );
            }
        }
    });
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
    let stopped = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut request_storage = [0; 4096];
            // The current native HTTP host buffers responses. Object generation
            // in the core uses a separate 17-byte read budget for every object.
            let mut response_storage = vec![0; SIZE + 4096];
            let mut scratch = [0; 17];
            loop {
                let socket = accept(&listener).unwrap();
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let mut request = read_request(&socket, &mut request_storage).unwrap();
                request.path = request.path.strip_prefix("/repo.git/").unwrap();
                let mut output = SliceWriter::new(&mut response_storage);
                let response =
                    http::serve_with_scratch(&repo, request, &mut scratch, &mut output).unwrap();
                respond(&socket, response, output.as_bytes()).unwrap();
            }
        });
        let _stop = Stop {
            stopped: &stopped,
            port,
        };
        for dumb in [false, true] {
            let clone = directory.join(if dumb { "dumb" } else { "smart" });
            let clone_path = clone.to_str().unwrap();
            git(&["clone", "-q", &url, clone_path], dumb);
            git(&["-C", clone_path, "fsck", "--strict"], dumb);
            git(&["-C", clone_path, "fetch", "origin"], dumb);
            assert_eq!(
                git(&["-C", clone_path, "rev-parse", "HEAD"], dumb).stdout,
                format!("{}\n", repo.head()).as_bytes()
            );
            let path = clone.join("generated.bin");
            assert_eq!(std::fs::metadata(&path).unwrap().len(), SIZE as u64);
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
            assert_eq!(offset, SIZE as u64);
        }
    });
}
