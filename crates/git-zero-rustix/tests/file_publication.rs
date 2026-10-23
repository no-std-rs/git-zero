#[path = "../../../tests/common/allocations.rs"]
mod allocations;
use git_zero::{
    Error, File, ObjectFormat, ObjectId, ObjectInfo, Repository, Snapshot,
    http::{Request, dumb::LooseObjects},
    io::Write,
    pack::{self, IndexEntry},
};
use git_zero_rustix::{FilePublication, FilePublishedResponse};
use std::{
    cell::Cell,
    fs::{File as DiskFile, OpenOptions},
    io::{Seek, SeekFrom},
    path::PathBuf,
};

fn owned(file: DiskFile) -> rustix::fd::OwnedFd {
    use rustix::fd::FromRawFd;
    use std::os::fd::IntoRawFd;
    // SAFETY: Transfer the unique owning descriptor without closing it.
    unsafe { rustix::fd::OwnedFd::from_raw_fd(file.into_raw_fd()) }
}

struct Directory(PathBuf);
impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "git-zero-file-publication-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn create(&self, name: &str, bytes: &[u8]) -> DiskFile {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        DiskFile::open(path).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Output<'a>(&'a mut Vec<u8>);
impl Write for Output<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> git_zero::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}
fn encode(repository: &impl Repository) -> (Vec<u8>, Vec<u8>) {
    let mut entries = vec![IndexEntry::EMPTY; repository.object_count()];
    let mut pack = Vec::new();
    let mut index = Vec::new();
    pack::write_indexed_with_scratch(
        repository,
        None,
        &mut entries,
        &mut [0; 19],
        &mut Output(&mut pack),
        &mut Output(&mut index),
    )
    .unwrap();
    (pack, index)
}
fn request(path: &str) -> Request<'_> {
    Request {
        method: "GET",
        path,
        query: None,
        body: b"",
        git_protocol: None,
        content_type: None,
    }
}
fn drain<R: Repository>(cursor: &mut FilePublishedResponse<'_, R>, capacity: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut guarded = vec![0xa7; capacity + 2];
    loop {
        let count = allocations::without_allocations(|| cursor.read(&mut guarded[1..capacity + 1]))
            .unwrap();
        assert!(count <= capacity);
        assert_eq!(guarded[0], 0xa7);
        assert_eq!(guarded[capacity + 1], 0xa7);
        if count == 0 {
            break;
        }
        result.extend_from_slice(&guarded[1..count + 1]);
    }
    assert_eq!(result.len() as u64, cursor.size());
    result
}
struct Observed<'a, R> {
    source: &'a R,
    fail: Cell<bool>,
    reads: Cell<usize>,
}
impl<R: Repository> Repository for Observed<'_, R> {
    fn head(&self) -> ObjectId {
        self.source.head()
    }
    fn object_format(&self) -> ObjectFormat {
        self.source.object_format()
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.source.object_id(index)
    }
    fn object_info(&self, index: usize) -> git_zero::Result<ObjectInfo> {
        self.source.object_info(index)
    }
    fn read_object(
        &self,
        index: usize,
        offset: u64,
        storage: &mut [u8],
    ) -> git_zero::Result<usize> {
        self.reads.set(self.reads.get() + 1);
        if self.fail.get() {
            Err(Error::Backend)
        } else {
            self.source.read_object(index, offset, storage)
        }
    }
}

#[test]
fn verified_files_serve_both_formats_without_raw_body_reads_or_shared_file_positions() {
    let directory = Directory::new("both-formats");
    let binary: Vec<_> = (0..=255).cycle().take(131071).collect();
    let files = [File {
        name: "binary",
        contents: &binary,
    }];
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 1024];
        let mut commit = [0; 1024];
        let source = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let repository = Observed {
            source: &source,
            fail: Cell::new(false),
            reads: Cell::new(0),
        };
        let (pack, index) = encode(&repository);
        let mut pack_file = directory.create("data.pack", &pack);
        let mut index_file = directory.create("data.idx", &index);
        pack_file.seek(SeekFrom::Start(17)).unwrap();
        index_file.seek(SeekFrom::Start(23)).unwrap();
        let mut pack_position = pack_file.try_clone().unwrap();
        let mut index_position = index_file.try_clone().unwrap();
        let mut metadata = [IndexEntry::EMPTY; 3];
        let publication = allocations::without_allocations(|| {
            FilePublication::from_fds(
                &repository,
                &mut metadata,
                owned(pack_file),
                owned(index_file),
                &mut [0; 7],
                &mut [0; 3],
                LooseObjects::Disabled,
            )
        })
        .unwrap();
        assert_eq!(pack_position.stream_position().unwrap(), 17);
        assert_eq!(index_position.stream_position().unwrap(), 23);
        assert_eq!(publication.pack_size(), pack.len() as u64);
        assert_eq!(publication.index_size(), index.len() as u64);
        let reads = repository.reads.get();
        repository.fail.set(true);
        for (extension, expected) in [("pack", &pack), ("idx", &index)] {
            let path = format!("objects/pack/pack-{}.{extension}", publication.checksum());
            for capacity in [1, 7, 4096] {
                let mut cursor =
                    allocations::without_allocations(|| publication.prepare(request(&path)))
                        .unwrap()
                        .unwrap();
                assert_eq!(cursor.response().status, 200);
                assert_eq!(cursor.read(&mut []), Err(Error::BufferTooSmall));
                assert_eq!(&drain(&mut cursor, capacity), expected);
                assert_eq!(cursor.read(&mut [0; 1]), Ok(0));
            }
        }
        assert_eq!(repository.reads.get(), reads);
        assert_eq!(pack_position.stream_position().unwrap(), 17);
        assert_eq!(index_position.stream_position().unwrap(), 23);
        let mut listing = publication
            .prepare(request("objects/info/packs"))
            .unwrap()
            .unwrap();
        assert_eq!(
            drain(&mut listing, 1),
            format!("P pack-{}.pack\n\n", publication.checksum()).as_bytes()
        );
        assert!(publication.prepare(request("HEAD")).unwrap().is_none());
        let mut missing = publication
            .prepare(request("objects/pack/pack-unknown.pack"))
            .unwrap()
            .unwrap();
        assert_eq!(missing.response().status, 404);
        assert!(drain(&mut missing, 1).is_empty());
    }
}

#[test]
fn both_files_are_authenticated_before_publication_including_exact_trailing_length() {
    let directory = Directory::new("corruption");
    let files = [File {
        name: "file",
        contents: b"some content\0",
    }];
    let mut tree = [0; 1024];
    let mut commit = [0; 1024];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let (pack, index) = encode(&repository);
    for which in 0..2 {
        let original = if which == 0 { &pack } else { &index };
        for position in [0, 7, original.len() / 2, original.len() - 1] {
            let mut corrupt = original.clone();
            corrupt[position] ^= 0xa7;
            let (p, i) = if which == 0 {
                (&corrupt, &index)
            } else {
                (&pack, &corrupt)
            };
            let mut metadata = [IndexEntry::EMPTY; 3];
            assert!(matches!(
                FilePublication::from_fds(
                    &repository,
                    &mut metadata,
                    owned(directory.create("data.pack", p)),
                    owned(directory.create("data.idx", i)),
                    &mut [0; 5],
                    &mut [0; 11],
                    LooseObjects::Disabled
                ),
                Err(Error::CorruptPack)
            ));
        }
        for data in [
            original[..original.len() - 1].to_vec(),
            [original.as_slice(), b"extra"].concat(),
        ] {
            let (p, i) = if which == 0 {
                (&data, &index)
            } else {
                (&pack, &data)
            };
            let mut metadata = [IndexEntry::EMPTY; 3];
            assert!(matches!(
                FilePublication::from_fds(
                    &repository,
                    &mut metadata,
                    owned(directory.create("data.pack", p)),
                    owned(directory.create("data.idx", i)),
                    &mut [0; 3],
                    &mut [0; 7],
                    LooseObjects::Disabled
                ),
                Err(Error::CorruptPack)
            ));
        }
    }
    for (a, b) in [(0, 3), (3, 0)] {
        let mut metadata = [IndexEntry::EMPTY; 3];
        assert!(matches!(
            FilePublication::from_fds(
                &repository,
                &mut metadata,
                owned(directory.create("data.pack", &pack)),
                owned(directory.create("data.idx", &index)),
                &mut vec![0; a],
                &mut vec![0; b],
                LooseObjects::Disabled
            ),
            Err(Error::BufferTooSmall)
        ));
    }
    let mut metadata = [IndexEntry::EMPTY; 3];
    assert!(
        FilePublication::from_fds(
            &repository,
            &mut metadata,
            owned(directory.create("data.pack", &pack)),
            owned(directory.create("data.idx", &index)),
            &mut [0; 3],
            &mut [0; 7],
            LooseObjects::Disabled
        )
        .is_ok()
    );
}

#[test]
fn open_files_survive_unlink_and_replace_with_independent_response_cancellation() {
    let directory = Directory::new("pinning");
    let files = [File {
        name: "file",
        contents: b"immutable body\n",
    }];
    let mut tree = [0; 1024];
    let mut commit = [0; 1024];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let (pack, index) = encode(&repository);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = FilePublication::from_fds(
        &repository,
        &mut metadata,
        owned(directory.create("data.pack", &pack)),
        owned(directory.create("data.idx", &index)),
        &mut [0; 3],
        &mut [0; 7],
        LooseObjects::Disabled,
    )
    .unwrap();
    std::fs::remove_file(directory.0.join("data.pack")).unwrap();
    std::fs::remove_file(directory.0.join("data.idx")).unwrap();
    directory.create("data.pack", b"unrelated replacement");
    directory.create("data.idx", b"unrelated replacement");
    let path = format!("objects/pack/pack-{}.pack", publication.checksum());
    let mut cancelled = publication.prepare(request(&path)).unwrap().unwrap();
    let mut live = publication.prepare(request(&path)).unwrap().unwrap();
    assert_eq!(cancelled.read(&mut [0; 1]), Ok(1));
    cancelled.cancel();
    assert_eq!(cancelled.read(&mut [0; 7]), Err(Error::InvalidRequest));
    assert_eq!(drain(&mut live, 3), pack);
    let path = format!("objects/pack/pack-{}.idx", publication.checksum());
    let mut cursor = publication.prepare(request(&path)).unwrap().unwrap();
    assert_eq!(drain(&mut cursor, 7), index);
}

#[test]
fn post_publication_truncation_terminally_fails_the_affected_cursor() {
    let directory = Directory::new("truncation");
    let files = [File {
        name: "file",
        contents: b"immutable body\n",
    }];
    let mut tree = [0; 1024];
    let mut commit = [0; 1024];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let (pack, index) = encode(&repository);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = FilePublication::from_fds(
        &repository,
        &mut metadata,
        owned(directory.create("data.pack", &pack)),
        owned(directory.create("data.idx", &index)),
        &mut [0; 3],
        &mut [0; 7],
        LooseObjects::Disabled,
    )
    .unwrap();
    let path = format!("objects/pack/pack-{}.pack", publication.checksum());
    let mut cursor = publication.prepare(request(&path)).unwrap().unwrap();
    assert_eq!(cursor.read(&mut [0; 1]), Ok(1));
    OpenOptions::new()
        .write(true)
        .open(directory.0.join("data.pack"))
        .unwrap()
        .set_len(1)
        .unwrap();
    assert_eq!(cursor.read(&mut [0; 3]), Err(Error::InvalidObject));
    std::fs::write(directory.0.join("data.pack"), &pack).unwrap();
    assert_eq!(cursor.read(&mut [0; 3]), Err(Error::InvalidRequest));
    let path = format!("objects/pack/pack-{}.idx", publication.checksum());
    assert_eq!(
        drain(
            &mut publication.prepare(request(&path)).unwrap().unwrap(),
            7
        ),
        index
    );
}
