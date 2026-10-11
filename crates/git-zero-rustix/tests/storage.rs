#[path = "../../../tests/common/mod.rs"]
mod common;
#[path = "../../../tests/common/streamed.rs"]
mod streamed;

use git_zero::{
    Error, ObjectHasher, ObjectId, ObjectInfo, ObjectKind, ObjectReader, Repository, Snapshot,
    ValidatedRepository, client,
    http::{self, Request},
    io::{SliceWriter, Write},
    pack, pktline,
};
use git_zero_rustix::{
    FileObject, FileRepository, HttpTransport, accept, listen, local_port, read_request, respond,
};
use rustix::fd::{FromRawFd, OwnedFd};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    ffi::CString,
    fs::{File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    os::fd::IntoRawFd,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct CountingAllocator;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
fn record_allocation() {
    let _ = TRACK.try_with(|track| {
        if track.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}
// SAFETY: All operations delegate unchanged to System; counters are thread-local.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn without_allocations(operation: impl FnOnce()) {
    ALLOCATIONS.with(|count| count.set(0));
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            TRACK.with(|track| track.set(false));
        }
    }
    TRACK.with(|track| track.set(true));
    let guard = Guard;
    operation();
    drop(guard);
    assert_eq!(
        ALLOCATIONS.with(Cell::get),
        0,
        "adapter operation allocated"
    );
}

struct Temporary(PathBuf);

fn owned(file: File) -> OwnedFd {
    // SAFETY: into_raw_fd transfers ownership; exactly one OwnedFd receives it.
    unsafe { OwnedFd::from_raw_fd(file.into_raw_fd()) }
}
impl Temporary {
    fn new() -> Self {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "git-zero-storage-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn fd(&self) -> OwnedFd {
        owned(File::open(&self.0).unwrap())
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct HostWriter<W>(W);
impl<W: std::io::Write> Write for HostWriter<W> {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.write_all(bytes).map_err(|_| Error::Backend)
    }
}

fn persist(source: &impl Repository, directory: &Path) -> Vec<FileObject> {
    let directory_fd = owned(File::open(directory).unwrap());
    let mut scratch = [0; 257];
    (0..source.object_count())
        .map(|index| {
            let name = format!("object-{index}");
            let mut file = File::create(directory.join(&name)).unwrap();
            let mut reader = ObjectReader::new(source, index).unwrap();
            while reader.remaining() != 0 {
                let length = reader.read(&mut scratch).unwrap();
                file.write_all(&scratch[..length]).unwrap();
            }
            FileObject::open_at(
                &directory_fd,
                &CString::new(name).unwrap(),
                reader.info().kind,
                source.object_id(index).unwrap(),
            )
            .unwrap()
        })
        .collect()
}

fn save_pack(repository: &impl Repository, path: &Path) -> OwnedFd {
    let mut file = HostWriter(File::create(path).unwrap());
    pack::write_with_scratch(repository, &mut [0; 139], &mut file).unwrap();
    drop(file);
    owned(File::open(path).unwrap())
}

#[test]
fn raw_files_validate_hash_and_match_generated_pack_and_loose_bytes() {
    let directory = Temporary::new();
    let source = streamed::GeneratedRepository::new(1_048_577, 139);
    let objects = persist(&source, &directory.0);
    let repository = FileRepository::new(source.head(), &objects);
    let mut metadata = [None; 3];
    let mut by_id = [0; 3];
    let validated = ValidatedRepository::new(
        &repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let mut expected = vec![0; 1_052_672];
    let mut actual = vec![0; expected.len()];
    for index in 0..validated.object_count() {
        assert!(validated.object(index).is_none());
        assert_eq!(
            validated.object_info(index).unwrap(),
            source.object_info(index).unwrap()
        );
        assert_eq!(validated.object_id(index), source.object_id(index));
        assert_eq!(
            ObjectHasher::new(&validated, index)
                .unwrap()
                .finish(&mut [0; 31])
                .unwrap(),
            source.object_id(index).unwrap()
        );
        let mut left = SliceWriter::new(&mut expected);
        let mut right = SliceWriter::new(&mut actual);
        pack::write_loose_with_scratch(&source, index, &mut [0; 257], &mut left).unwrap();
        pack::write_loose_with_scratch(&validated, index, &mut [0; 17], &mut right).unwrap();
        assert_eq!(left.as_bytes(), right.as_bytes());
    }
    let mut left = SliceWriter::new(&mut expected);
    let mut right = SliceWriter::new(&mut actual);
    pack::write_with_scratch(&source, &mut [0; 257], &mut left).unwrap();
    repository
        .write_pack_with_scratch(&mut [0; 17], &mut right)
        .unwrap();
    assert_eq!(left.as_bytes(), right.as_bytes());
    assert_eq!(repository.object_info(3), Err(Error::ObjectNotFound));
    assert_eq!(repository.object_id(3), None);
    assert_eq!(
        repository.read_object(3, 0, &mut [0]),
        Err(Error::ObjectNotFound)
    );
    assert_eq!(repository.read_object(2, u64::MAX, &mut [0]), Ok(0));
}

#[test]
fn positional_ranges_interleave_without_changing_shared_descriptor_cursor() {
    let directory = Temporary::new();
    let path = directory.0.join("data");
    std::fs::write(&path, b"prefixabcdefghsuffix").unwrap();
    let mut file = File::open(path).unwrap();
    file.seek(SeekFrom::Start(2)).unwrap();
    let id = git_zero::Object {
        kind: ObjectKind::Blob,
        data: b"abcdefgh",
    }
    .id();
    let object = FileObject::from_range(
        owned(file.try_clone().unwrap()),
        6,
        ObjectInfo {
            kind: ObjectKind::Blob,
            size: 8,
        },
        id,
    )
    .unwrap();
    let objects = [object];
    let repository = FileRepository::new(id, &objects);
    let mut first = ObjectReader::new(&repository, 0).unwrap();
    let mut second = ObjectReader::new(&repository, 0).unwrap();
    second.seek(5).unwrap();
    let mut scratch = [0; 4];
    assert_eq!(first.read(&mut scratch).unwrap(), 4);
    assert_eq!(&scratch, b"abcd");
    assert_eq!(second.read(&mut scratch).unwrap(), 3);
    assert_eq!(&scratch[..3], b"fgh");
    assert_eq!(first.read(&mut scratch).unwrap(), 4);
    assert_eq!(&scratch, b"efgh");
    assert_eq!(first.read(&mut []).unwrap(), 0);
    assert_eq!(file.stream_position().unwrap(), 2);
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"e");
    std::thread::scope(|scope| {
        for offset in 0..8 {
            let repository = &repository;
            scope.spawn(move || {
                for _ in 0..100 {
                    let mut byte = [0];
                    assert_eq!(repository.read_object(0, offset, &mut byte).unwrap(), 1);
                    assert_eq!(byte[0], b'a' + offset as u8);
                }
            });
        }
    });
}

#[test]
fn sparse_files_support_offsets_above_four_gib_and_reject_invalid_ranges() {
    let directory = Temporary::new();
    let path = directory.0.join("sparse");
    let offset = (1_u64 << 32) + 7;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(b"large-offset").unwrap();
    let id = ObjectId([1; 20]);
    let objects =
        [FileObject::from_fd(owned(file.try_clone().unwrap()), ObjectKind::Blob, id).unwrap()];
    let repository = FileRepository::new(id, &objects);
    assert_eq!(repository.object_info(0).unwrap().size, offset + 12);
    let mut reader = ObjectReader::new(&repository, 0).unwrap();
    reader.seek(offset).unwrap();
    let mut scratch = [0; 20];
    assert_eq!(reader.read(&mut scratch).unwrap(), 12);
    assert_eq!(&scratch[..12], b"large-offset");
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    for (start, size) in [
        (offset + 13, 0),
        (offset + 10, 3),
        (u64::MAX, 2),
        (i64::MAX as u64, 1),
    ] {
        assert!(matches!(
            FileObject::from_range(
                owned(file.try_clone().unwrap()),
                start,
                ObjectInfo {
                    kind: ObjectKind::Blob,
                    size
                },
                id
            ),
            Err(Error::InvalidObject)
        ));
    }
    assert!(matches!(
        FileObject::from_fd(directory.fd(), ObjectKind::Blob, id),
        Err(Error::InvalidObject)
    ));
    assert!(matches!(
        FileObject::open_at(directory.fd(), c"missing", ObjectKind::Blob, id),
        Err(Error::ObjectNotFound)
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&path, directory.0.join("symlink")).unwrap();
        assert!(matches!(
            FileObject::open_at(directory.fd(), c"symlink", ObjectKind::Blob, id),
            Err(Error::Backend)
        ));
    }
}

#[test]
fn pinned_files_survive_replacement_and_unlink_but_report_truncation() {
    let directory = Temporary::new();
    let path = directory.0.join("object");
    std::fs::write(&path, b"original").unwrap();
    let object = FileObject::open_at(
        directory.fd(),
        c"object",
        ObjectKind::Blob,
        ObjectId([1; 20]),
    )
    .unwrap();
    std::fs::rename(&path, directory.0.join("old")).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    std::fs::remove_file(directory.0.join("old")).unwrap();
    let objects = [object];
    let repository = FileRepository::new(ObjectId([1; 20]), &objects);
    let mut scratch = [0; 20];
    assert_eq!(repository.read_object(0, 0, &mut scratch).unwrap(), 8);
    assert_eq!(&scratch[..8], b"original");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let objects = [FileObject::from_fd(
        owned(file.try_clone().unwrap()),
        ObjectKind::Blob,
        ObjectId([1; 20]),
    )
    .unwrap()];
    let repository = FileRepository::new(ObjectId([1; 20]), &objects);
    file.set_len(0).unwrap();
    assert_eq!(
        repository.read_object(0, 0, &mut scratch),
        Err(Error::InvalidObject)
    );
}

#[test]
fn prepacked_files_reject_corruption_truncation_extras_and_wrong_closure() {
    let directory = Temporary::new();
    let source = streamed::GeneratedRepository::new(100, 7);
    let objects = persist(&source, &directory.0);
    let path = directory.0.join("pack");
    let fd = save_pack(&source, &path);
    let repository = FileRepository::new(source.head(), &objects)
        .with_prepacked(fd, &mut [0; 3], &mut [0; 1])
        .unwrap();
    assert!(repository.has_prepacked());
    let expected = std::fs::read(&path).unwrap();
    for capacity in [1, 17, 1024] {
        let mut bytes = vec![0; expected.len()];
        let mut output = SliceWriter::new(&mut bytes);
        repository
            .write_pack_with_scratch(&mut vec![0; capacity], &mut output)
            .unwrap();
        assert_eq!(output.as_bytes(), expected);
    }
    let mut bytes = vec![0; expected.len()];
    assert_eq!(
        repository.write_pack_with_scratch(&mut [], &mut SliceWriter::new(&mut bytes)),
        Err(Error::BufferTooSmall)
    );
    for position in [0, 8, 30, expected.len() - 1] {
        let mut corrupt = expected.clone();
        corrupt[position] ^= 1;
        std::fs::write(&path, corrupt).unwrap();
        assert!(matches!(
            FileRepository::new(source.head(), &objects).with_prepacked(
                owned(File::open(&path).unwrap()),
                &mut [0; 7],
                &mut [0; 17]
            ),
            Err(Error::CorruptPack)
        ));
    }
    for length in [0, 12, expected.len() - 1, expected.len() + 1] {
        let mut corrupt = expected.clone();
        corrupt.resize(length, 0);
        std::fs::write(&path, corrupt).unwrap();
        assert!(matches!(
            FileRepository::new(source.head(), &objects).with_prepacked(
                owned(File::open(&path).unwrap()),
                &mut [0; 7],
                &mut [0; 17]
            ),
            Err(Error::CorruptPack)
        ));
    }
    let wrong = streamed::GeneratedRepository::new(101, 7);
    let fd = save_pack(&wrong, &path);
    assert!(matches!(
        FileRepository::new(source.head(), &objects).with_prepacked(fd, &mut [0; 7], &mut [0; 17]),
        Err(Error::CorruptPack)
    ));
    let fd = save_pack(&source, &path);
    assert!(matches!(
        FileRepository::new(source.head(), &objects).with_prepacked(fd, &mut [0; 7], &mut []),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        FileRepository::new(source.head(), &objects).with_prepacked(
            directory.fd(),
            &mut [0; 7],
            &mut [0; 17]
        ),
        Err(Error::CorruptPack)
    ));
}

#[test]
fn prepacked_serving_reads_only_the_pack_and_upload_hook_preserves_routing() {
    let directory = Temporary::new();
    let source = streamed::GeneratedRepository::new(100, 7);
    let objects = persist(&source, &directory.0);
    let path = directory.0.join("pack");
    let fd = save_pack(&source, &path);
    let repository = FileRepository::new(source.head(), &objects)
        .with_prepacked(fd, &mut [0; 7], &mut [0; 17])
        .unwrap();
    let mut storage = [0; 128];
    let mut request = SliceWriter::new(&mut storage);
    pktline::write_parts(&mut request, &[b"want ", &source.head().to_hex(), b"\n"]).unwrap();
    request.write_all(b"00000009done\n").unwrap();
    let request = Request {
        method: "POST",
        path: "git-upload-pack",
        query: None,
        body: request.as_bytes(),
    };
    let mut expected = [0; 1024];
    let mut output = SliceWriter::new(&mut expected);
    http::serve_with_scratch(&source, request, &mut [0; 7], &mut output).unwrap();
    let expected = output.finish();
    // Violate source immutability deliberately: cached serving should perform
    // no source-object reads. Dumb reads and uncached generation should fail.
    OpenOptions::new()
        .write(true)
        .open(directory.0.join("object-2"))
        .unwrap()
        .set_len(0)
        .unwrap();
    let mut actual = [0; 1024];
    let mut output = SliceWriter::new(&mut actual);
    let response = repository
        .serve_with_scratch(request, &mut [0; 1], &mut output)
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(output.as_bytes(), expected);
    let mut calls = 0;
    for request in [
        Request {
            method: "GET",
            path: "HEAD",
            query: None,
            body: &[],
        },
        Request {
            method: "POST",
            path: "git-upload-pack",
            query: None,
            body: b"0032want 0000000000000000000000000000000000000000\n00000009done\n",
        },
        Request {
            method: "POST",
            path: "git-upload-pack",
            query: None,
            body: &request.body[..50],
        },
    ] {
        let _ = http::serve_with_pack(
            &repository,
            request,
            &mut [0; 1],
            &mut SliceWriter::new(&mut actual),
            |_, _| {
                calls += 1;
                Ok(())
            },
        );
    }
    assert_eq!(calls, 0);
    let mut output = SliceWriter::new(&mut actual);
    assert_eq!(
        http::serve_with_pack(&repository, request, &mut [0; 1], &mut output, |_, _| Err(
            Error::Backend
        ))
        .map(|_| ()),
        Err(Error::Backend)
    );
    assert_eq!(output.as_bytes(), b"0008NAK\n");
    let plain = FileRepository::new(source.head(), &objects);
    assert_eq!(
        plain.write_pack_with_scratch(&mut [0; 7], &mut SliceWriter::new(&mut actual)),
        Err(Error::InvalidObject)
    );
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert_eq!(
        repository.write_pack_with_scratch(&mut [0; 7], &mut SliceWriter::new(&mut actual)),
        Err(Error::InvalidObject)
    );
}

#[test]
fn provider_validation_pack_verification_and_serving_allocate_nothing() {
    let directory = Temporary::new();
    let source = streamed::GeneratedRepository::new(100, 7);
    let objects = persist(&source, &directory.0);
    let path = directory.0.join("pack");
    let fd = save_pack(&source, &path);
    let directory_fd = directory.fd();
    let mut metadata = [None; 3];
    let mut by_id = [0; 3];
    let mut output = [0; 1024];
    without_allocations(|| {
        let object = FileObject::open_at(
            &directory_fd,
            c"object-2",
            ObjectKind::Blob,
            source.object_id(2).unwrap(),
        )
        .unwrap();
        assert_eq!(object.info().size, 100);
        let repository = FileRepository::new(source.head(), &objects)
            .with_prepacked(fd, &mut [0; 7], &mut [0; 17])
            .unwrap();
        let validated = ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 7],
            &mut [0; 128],
        )
        .unwrap();
        for index in 0..3 {
            assert_eq!(
                ObjectHasher::new(&validated, index)
                    .unwrap()
                    .finish(&mut [0; 7])
                    .unwrap(),
                source.object_id(index).unwrap()
            );
        }
        pack::write_with_scratch(&validated, &mut [0; 7], &mut SliceWriter::new(&mut output))
            .unwrap();
        repository
            .write_pack_with_scratch(&mut [0; 7], &mut SliceWriter::new(&mut output))
            .unwrap();
        repository
            .serve_with_scratch(
                Request {
                    method: "GET",
                    path: "HEAD",
                    query: None,
                    body: &[],
                },
                &mut [0; 7],
                &mut SliceWriter::new(&mut output),
            )
            .unwrap();
    });
}

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

#[test]
fn real_git_clones_file_backed_nested_snapshots_with_and_without_prepacked_data() {
    let directory = Temporary::new();
    let mut trees = [0; 1024];
    let mut commit = [0; 512];
    let source = Snapshot::from_entries(common::ENTRIES, &mut trees, &mut commit).unwrap();
    let objects = persist(&source, &directory.0);
    for prepacked in [false, true] {
        let mut repository = FileRepository::new(source.head(), &objects)
            .with_refs(common::HEAD_REF, common::REFS)
            .unwrap();
        if prepacked {
            repository = repository
                .with_prepacked(
                    save_pack(&source, &directory.0.join("pack")),
                    &mut [0; 17],
                    &mut [0; 13],
                )
                .unwrap();
        }
        let mut metadata = [None; 6];
        let mut by_id = [0; 6];
        let validated = ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 17],
            &mut [0; 128],
        )
        .unwrap();
        let listener = listen(0).unwrap();
        let port = local_port(&listener).unwrap();
        let url = format!("http://127.0.0.1:{port}/repo.git");
        let stopped = AtomicBool::new(false);
        struct Stop<'a>(&'a AtomicBool, u16);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
                let _ = TcpStream::connect(("127.0.0.1", self.1));
            }
        }
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut input = [0; 4096];
                let mut bytes = [0; 8192];
                loop {
                    let socket = accept(&listener).unwrap();
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut request = read_request(&socket, &mut input).unwrap();
                    request.path = request.path.strip_prefix("/repo.git/").unwrap();
                    let mut output = SliceWriter::new(&mut bytes);
                    let response = http::serve_with_pack(
                        &validated,
                        request,
                        &mut [0; 17],
                        &mut output,
                        |scratch, output| repository.write_pack_with_scratch(scratch, output),
                    )
                    .unwrap();
                    respond(&socket, response, output.as_bytes()).unwrap();
                }
            });
            let _stop = Stop(&stopped, port);
            for dumb in [false, true] {
                let clone = directory.0.join(format!("clone-{prepacked}-{dumb}"));
                let path = clone.to_str().unwrap();
                git(&["clone", "-q", &url, path], dumb);
                if dumb {
                    let empty =
                        ObjectId::from_hex(b"4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();
                    let mut wire = [0; 1024];
                    let mut response = [0; 256];
                    let mut transport = HttpTransport {
                        address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
                        repository_path: "/repo.git",
                        wire_storage: &mut wire,
                    };
                    let loose = client::fetch_loose(&mut transport, empty, &mut response).unwrap();
                    let objects = clone.join(".git/objects/4b");
                    std::fs::create_dir_all(&objects).unwrap();
                    std::fs::write(
                        objects.join("825dc642cb6eb9a060e54bf8d69288fbee4904"),
                        loose,
                    )
                    .unwrap();
                }
                git(&["-C", path, "fsck", "--strict"], dumb);
                git(&["-C", path, "fetch", "origin"], dumb);
                assert_eq!(
                    git(&["-C", path, "rev-parse", "HEAD"], dumb).stdout,
                    format!("{}\n", source.head()).as_bytes()
                );
                assert_eq!(
                    git(&["-C", path, "symbolic-ref", "HEAD"], dumb).stdout,
                    format!("{}\n", common::HEAD_REF).as_bytes()
                );
                assert_eq!(
                    std::fs::read(clone.join("a/run")).unwrap(),
                    b"#!/bin/sh\nprintf 'snapshot\\n'\n"
                );
                assert_eq!(
                    std::fs::read_link(clone.join("b/target")).unwrap(),
                    PathBuf::from("run")
                );
            }
        });
    }
}
