#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{Error, File, ObjectKind, Repository, Snapshot, io::SliceWriter, pack};
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[test]
fn git_accepts_empty_duplicate_and_multiblock_objects() {
    let directory = std::env::temp_dir().join(format!("git-zero-pack-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let git = || {
        let mut command = Command::new("git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        command
    };
    assert!(
        git()
            .args(["init", "-q", "--bare", "--object-format=sha1"])
            .arg(&directory)
            .status()
            .unwrap()
            .success()
    );
    let mut large = [0; 131071];
    for (index, byte) in large.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let files = [
        File {
            name: "a-empty",
            contents: b"",
        },
        File {
            name: "b-large",
            contents: &large,
        },
        File {
            name: "c-repeat",
            contents: &large,
        },
    ];
    let mut tree = [0; 1024];
    let mut commit = [0; 1024];
    let repo = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let mut storage = [0; 140000];
    let mut output = SliceWriter::new(&mut storage);
    pack::write(&repo, &mut output).unwrap();
    let mut child = git()
        .arg("-C")
        .arg(&directory)
        .args(["index-pack", "--strict", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(output.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for index in 0..repo.object_count() {
        let object = repo.object(index).unwrap();
        if object.kind == ObjectKind::Blob {
            let result = git()
                .arg("-C")
                .arg(&directory)
                .args(["cat-file", "blob", &object.id().to_string()])
                .output()
                .unwrap();
            assert!(result.status.success());
            assert_eq!(result.stdout, object.data);
        }
    }
}

struct HostWriter<W>(W);

impl<W: std::io::Write> git_zero::io::Write for HostWriter<W> {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.write_all(bytes).map_err(|_| Error::Backend)
    }
}

#[test]
fn generated_pack_and_loose_objects_stream_to_git_without_full_body_buffers() {
    let directory =
        std::env::temp_dir().join(format!("git-zero-streamed-pack-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let repository = streamed::GeneratedRepository::new(1_048_577, 139);
    let git = |repo: &std::path::Path| {
        let mut command = Command::new("git");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(repo);
        command
    };
    for packed in [false, true] {
        let repo = directory.join(if packed { "packed" } else { "loose" });
        std::fs::create_dir(&repo).unwrap();
        assert!(
            git(&repo)
                .args(["init", "-q", "--bare", "--object-format=sha1"])
                .status()
                .unwrap()
                .success()
        );
        let mut scratch = [0; 257];
        if packed {
            let mut child = git(&repo)
                .args(["index-pack", "--strict", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut output = HostWriter(child.stdin.take().unwrap());
            pack::write_with_scratch(&repository, &mut scratch, &mut output).unwrap();
            drop(output);
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        } else {
            for index in 0..repository.object_count() {
                let id = repository.object_id(index).unwrap().to_string();
                let objects = repo.join("objects").join(&id[..2]);
                std::fs::create_dir_all(&objects).unwrap();
                let file = std::fs::File::create(objects.join(&id[2..])).unwrap();
                let mut output = HostWriter(std::io::BufWriter::with_capacity(1024, file));
                pack::write_loose_with_scratch(&repository, index, &mut scratch, &mut output)
                    .unwrap();
                output.0.flush().unwrap();
            }
        }
        assert!(
            git(&repo)
                .args([
                    "update-ref",
                    "refs/heads/main",
                    &repository.head().to_string()
                ])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            git(&repo)
                .args(["symbolic-ref", "HEAD", "refs/heads/main"])
                .status()
                .unwrap()
                .success()
        );
        let result = git(&repo).args(["fsck", "--strict"]).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
