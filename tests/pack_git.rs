use git_zero::{File, ObjectKind, Repository, Snapshot, io::SliceWriter, pack};
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
