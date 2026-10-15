#[path = "../../../tests/common/allocations.rs"]
mod allocations;

#[cfg(not(any(
    target_os = "espidf",
    target_os = "horizon",
    target_os = "solaris",
    target_os = "vita",
    target_os = "wasi"
)))]
#[path = "disk_cache/coordination.rs"]
mod coordination;

use git_zero::{
    Error, File, ObjectFormat, ObjectId, Repository, Snapshot,
    cache::selected::{self, Buffers, Key, Outcome, PackCache, Selection},
    io::SliceWriter,
    pack,
};
use git_zero_rustix::cache::DiskCache;
use rustix::fd::{AsFd, OwnedFd};
use std::{
    ffi::CString,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Directory {
    path: PathBuf,
    fd: OwnedFd,
}
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "git-zero-disk-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            name.as_c_str(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        Self { path, fd }
    }
    fn entry(&self, key: Key) -> PathBuf {
        let hex: String = key
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.path.join(format!("{hex}.pack"))
    }
    fn assert_no_staging(&self) {
        assert!(std::fs::read_dir(&self.path).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-")
        }));
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn key(repository: &impl Repository) -> Key {
    Key::derive(
        repository,
        Selection::All,
        &mut vec![repository.object_format().zero_id(); repository.object_count()],
    )
    .unwrap()
}
fn encoded(repository: &impl Repository) -> Vec<u8> {
    let mut storage = [0; 8192];
    let mut output = SliceWriter::new(&mut storage);
    pack::write(repository, &mut output).unwrap();
    output.as_bytes().to_vec()
}
fn request(
    repository: &impl Repository,
    cache: &mut impl PackCache,
    expected: Outcome,
    bytes: &[u8],
) {
    let mut ids = vec![repository.object_format().zero_id(); repository.object_count()];
    let mut staging = vec![0; if expected == Outcome::Hit { 0 } else { 8192 }];
    let mut storage = vec![0; bytes.len()];
    let mut output = SliceWriter::new(&mut storage);
    let result = allocations::without_allocations(|| {
        selected::write(
            repository,
            Selection::All,
            cache,
            Buffers {
                ids: &mut ids,
                pack: &mut staging,
                read: &mut [0; 17],
                byte_limit: 8192,
            },
            &mut output,
        )
    })
    .unwrap();
    assert_eq!(result, expected);
    assert_eq!(output.as_bytes(), bytes);
}

#[test]
fn persisted_misses_hits_disabled_mode_and_eviction_are_allocation_free() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = Directory::new();
        let files = [
            File {
                name: "data",
                contents: b"persisted\0\xff",
            },
            File {
                name: "empty",
                contents: b"",
            },
        ];
        let mut tree = [0; 512];
        let mut commit = [0; 512];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let bytes = encoded(&repository);
        let identity = key(&repository);
        let mut buffer = [0; 8192];
        let mut cache = DiskCache::new(directory.fd.as_fd(), &mut buffer, [1; 16]);
        request(&repository, &mut cache, Outcome::Miss, &bytes);
        assert_eq!(std::fs::read(directory.entry(identity)).unwrap(), bytes);
        import_with_git(&directory, &repository, &bytes);
        request(&repository, &mut cache, Outcome::Hit, &bytes);
        cache.set_enabled(false);
        request(&repository, &mut cache, Outcome::Miss, &bytes);
        cache.set_enabled(true);
        request(&repository, &mut cache, Outcome::Hit, &bytes);
        assert_eq!(cache.statistics().insertions, 1);
        // An independent backend instance must load the persistent entry with
        // no previous key or resident-memory state.
        let mut other = [0; 8192];
        let mut reopened = DiskCache::new(directory.fd.as_fd(), &mut other, [2; 16]);
        request(&repository, &mut reopened, Outcome::Hit, &bytes);
        assert!(allocations::without_allocations(|| reopened.evict(identity)).unwrap());
        assert!(!reopened.evict(identity).unwrap());
        request(&repository, &mut cache, Outcome::Miss, &bytes);
        directory.assert_no_staging();
    }
}

#[test]
fn corrupt_truncated_and_oversized_files_fail_before_output() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let good = encoded(&repository);
    for bad in [
        b"not a pack".to_vec(),
        good[..good.len() - 1].to_vec(),
        vec![0; 8193],
    ] {
        std::fs::write(directory.entry(identity), bad).unwrap();
        let mut buffer = [0; 8192];
        let mut cache = DiskCache::new(directory.fd.as_fd(), &mut buffer, [3; 16]);
        let mut ids = vec![ObjectId::Sha1([0; 20]); repository.object_count()];
        let mut bytes = [0; 8192];
        let mut output = SliceWriter::new(&mut bytes);
        let result = allocations::without_allocations(|| {
            selected::write(
                &repository,
                Selection::All,
                &mut cache,
                Buffers {
                    ids: &mut ids,
                    pack: &mut [],
                    read: &mut [],
                    byte_limit: 8192,
                },
                &mut output,
            )
        });
        assert!(result.is_err());
        assert!(output.is_empty());
        assert_eq!(cache.statistics().insertions, 0);
    }
}

#[test]
fn sink_failure_leaves_a_complete_persistent_entry_for_retry() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let good = encoded(&repository);
    let identity = key(&repository);
    let mut resident = [0; 8192];
    let mut cache = DiskCache::new(directory.fd.as_fd(), &mut resident, [8; 16]);
    let mut ids = vec![ObjectId::Sha1([0; 20]); repository.object_count()];
    let mut staging = [0; 8192];
    let mut output = SliceWriter::new(&mut []);
    assert_eq!(
        allocations::without_allocations(|| selected::write(
            &repository,
            Selection::All,
            &mut cache,
            Buffers {
                ids: &mut ids,
                pack: &mut staging,
                read: &mut [0; 17],
                byte_limit: 8192
            },
            &mut output
        )),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(std::fs::read(directory.entry(identity)).unwrap(), good);
    let mut retry = [0; 8192];
    let mut reopened = DiskCache::new(directory.fd.as_fd(), &mut retry, [9; 16]);
    request(&repository, &mut reopened, Outcome::Hit, &good);
    directory.assert_no_staging();
}

#[test]
fn failed_capacity_collision_and_publication_preserve_prior_files() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let good = encoded(&repository);
    let mut buffer = [0; 8192];
    let mut cache = DiskCache::new(directory.fd.as_fd(), &mut buffer, [9; 16]);
    allocations::without_allocations(|| cache.put(identity, &good)).unwrap();
    assert_eq!(
        allocations::without_allocations(|| cache.put(identity, &[0; 8193])),
        Err(Error::BufferTooSmall)
    );
    assert_eq!(cache.get(identity).unwrap(), Some(good.as_slice()));
    let empty = Key::derive(&repository, Selection::Indices(&[]), &mut []).unwrap();
    let target = directory.entry(empty);
    std::fs::create_dir(&target).unwrap();
    let mut empty_bytes = [0; 64];
    let mut empty_output = SliceWriter::new(&mut empty_bytes);
    pack::write_selected_with_scratch(&repository, &[], &mut [], &mut empty_output).unwrap();
    assert_eq!(
        allocations::without_allocations(|| cache.put(empty, empty_output.as_bytes())),
        Err(Error::Backend)
    );
    assert!(target.is_dir());
    assert_eq!(std::fs::read(directory.entry(identity)).unwrap(), good);
    directory.assert_no_staging();
    // Exclusive creation must never remove a pre-existing foreign staging file.
    let foreign = directory
        .path
        .join(format!(".tmp-{}{}.pack", "07".repeat(16), "0".repeat(16)));
    std::fs::write(&foreign, b"foreign producer").unwrap();
    let mut other = [0; 8192];
    let mut colliding = DiskCache::new(directory.fd.as_fd(), &mut other, [7; 16]);
    assert_eq!(
        allocations::without_allocations(|| colliding.put(identity, &good)),
        Err(Error::Backend)
    );
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign producer");
    assert_eq!(std::fs::read(directory.entry(identity)).unwrap(), good);
    std::fs::remove_file(foreign).unwrap();
    allocations::without_allocations(|| colliding.put(identity, &good)).unwrap();
    directory.assert_no_staging();
}

#[test]
fn cache_paths_reject_symlinks_and_nonregular_files() {
    let directory = Directory::new();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let outside = directory.path.join("outside");
    std::fs::write(&outside, b"must not be opened or replaced").unwrap();
    let target = directory.entry(identity);
    std::os::unix::fs::symlink(&outside, &target).unwrap();
    let mut buffer = [0; 8192];
    let mut cache = DiskCache::new(directory.fd.as_fd(), &mut buffer, [4; 16]);
    assert_eq!(
        allocations::without_allocations(|| cache
            .get(identity)
            .map(|entry| entry.map(<[u8]>::len))),
        Err(Error::Backend)
    );
    cache.evict(identity).unwrap();
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"must not be opened or replaced"
    );
    std::fs::create_dir(&target).unwrap();
    assert_eq!(cache.get(identity), Err(Error::InvalidObject));
    assert_eq!(cache.evict(identity), Err(Error::Backend));
    assert!(target.is_dir());
}

#[test]
fn independent_reader_buffers_survive_eviction_and_reordered_replacement() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = Directory::new();
        let files = [File {
            name: "data",
            contents: b"shared",
        }];
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let identity = key(&repository);
        let first = encoded(&repository);
        let reverse: Vec<_> = (0..repository.object_count()).rev().collect();
        let mut reordered = [0; 8192];
        let mut output = SliceWriter::new(&mut reordered);
        pack::write_selected_with_scratch(&repository, &reverse, &mut [], &mut output).unwrap();
        let second = output.as_bytes();
        assert_ne!(first, second);
        identity
            .verify_pack(
                second,
                &mut vec![format.zero_id(); repository.object_count()],
                8192,
            )
            .unwrap();
        let mut a = [0; 8192];
        let mut b = [0; 8192];
        let mut reader = DiskCache::new(directory.fd.as_fd(), &mut a, [5; 16]);
        let mut publisher = DiskCache::new(directory.fd.as_fd(), &mut b, [6; 16]);
        publisher.put(identity, &first).unwrap();
        let held = allocations::without_allocations(|| reader.get(identity))
            .unwrap()
            .unwrap();
        allocations::without_allocations(|| publisher.evict(identity)).unwrap();
        allocations::without_allocations(|| publisher.put(identity, second)).unwrap();
        assert_eq!(held, first);
        assert_eq!(publisher.get(identity).unwrap(), Some(second));
        assert_eq!(std::fs::read(directory.entry(identity)).unwrap(), second);
        directory.assert_no_staging();
    }
}

#[test]
fn concurrent_cold_producers_publish_only_complete_packs() {
    let directory = Directory::new();
    let files = [File {
        name: "data",
        contents: b"racing writers",
    }];
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let identity = key(&repository);
    let first = encoded(&repository);
    let reverse: Vec<_> = (0..repository.object_count()).rev().collect();
    let mut second = [0; 8192];
    let mut output = SliceWriter::new(&mut second);
    pack::write_selected_with_scratch(&repository, &reverse, &mut [], &mut output).unwrap();
    let second = output.as_bytes();
    let first = first.as_slice();
    let mut a = [0; 8192];
    let mut b = [0; 8192];
    let mut first_producer = DiskCache::new(directory.fd.as_fd(), &mut a, [1; 16]);
    let mut second_producer = DiskCache::new(directory.fd.as_fd(), &mut b, [2; 16]);
    assert_eq!(first_producer.get(identity).unwrap(), None);
    assert_eq!(second_producer.get(identity).unwrap(), None);
    // Both producers observed the cold miss before either published. Seed the
    // first complete publication so every concurrent read below must succeed.
    first_producer.put(identity, first).unwrap();
    std::thread::scope(|scope| {
        for (mut writer, bytes) in [(first_producer, first), (second_producer, second)] {
            scope.spawn(move || {
                for _ in 0..16 {
                    allocations::without_allocations(|| writer.put(identity, bytes)).unwrap();
                }
            });
        }
        let mut resident = [0; 8192];
        let mut reader = DiskCache::new(directory.fd.as_fd(), &mut resident, [3; 16]);
        let mut ids = vec![ObjectId::Sha1([0; 20]); repository.object_count()];
        for _ in 0..128 {
            allocations::without_allocations(|| {
                let bytes = reader.get(identity).unwrap().unwrap();
                assert!(bytes == first || bytes == second);
                identity.verify_pack(bytes, &mut ids, 8192).unwrap();
            });
        }
    });
    directory.assert_no_staging();
}

fn import_with_git(directory: &Directory, repository: &impl Repository, bytes: &[u8]) {
    use std::{
        io::Write as _,
        process::{Command, Stdio},
    };
    let target = directory.path.join("git-reference");
    std::fs::create_dir(&target).unwrap();
    let executable = std::env::var_os("GIT_ZERO_TEST_GIT").unwrap_or_else(|| "git".into());
    let git = || {
        let mut command = Command::new(&executable);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&target);
        command
    };
    let init = git()
        .args([
            "init",
            "--bare",
            "-q",
            &format!("--object-format={}", repository.object_format().name()),
        ])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let mut child = git()
        .args(["index-pack", "--strict", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for index in 0..repository.object_count() {
        let id = repository.object_id(index).unwrap().to_string();
        let object = repository.object(index).unwrap();
        let result = git()
            .args(["cat-file", object.kind.name(), &id])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, object.data);
    }
    let result = git().args(["fsck", "--strict"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
