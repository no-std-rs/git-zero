#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    EmptyRepository, Error, File, ObjectFormat, ObjectId, ObjectInfo, Repository, Snapshot,
    http::{
        self, Request,
        dumb::{LooseObjects, Publication},
    },
    io::{SliceWriter, Write},
    pack::{self, IndexEntry},
};
use std::cell::Cell;

fn get(path: &str) -> Request<'_> {
    Request {
        method: "GET",
        path,
        query: None,
        body: b"",
        git_protocol: None,
        content_type: None,
    }
}
fn path(publication: &Publication<'_, impl Repository>, extension: &str) -> String {
    format!("objects/pack/pack-{}.{extension}", publication.checksum())
}
fn drain(
    publication: &Publication<'_, impl Repository>,
    path: &str,
    capacity: usize,
) -> (http::Response, Vec<u8>) {
    let mut cursor = allocations::without_allocations(|| publication.prepare(get(path)))
        .unwrap()
        .unwrap();
    let response = cursor.response();
    let size = cursor.size();
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
    assert_eq!(size, result.len() as u64);
    assert_eq!(cursor.read(&mut guarded), Ok(0));
    (response, result)
}
fn encode(repository: &impl Repository) -> (Vec<u8>, Vec<u8>) {
    let mut metadata = vec![IndexEntry::EMPTY; repository.object_count()];
    let mut pack = vec![0; 512 * 1024];
    let mut index = [0; 8192];
    let mut pack_output = SliceWriter::new(&mut pack);
    let mut index_output = SliceWriter::new(&mut index);
    pack::write_indexed_with_scratch(
        repository,
        None,
        &mut metadata,
        &mut [0; 17],
        &mut pack_output,
        &mut index_output,
    )
    .unwrap();
    (
        pack_output.as_bytes().to_vec(),
        index_output.as_bytes().to_vec(),
    )
}

#[test]
fn publication_streams_the_exact_pack_index_and_checksum_name_in_both_formats() {
    let binary: Vec<_> = (0..=255).cycle().take(131_071).collect();
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let files = [
            File {
                name: "binary",
                contents: &binary,
            },
            File {
                name: "empty",
                contents: b"",
            },
        ];
        let mut tree = [0; 1024];
        let mut commit = [0; 1024];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let expected = encode(&repository);
        let sentinel = IndexEntry {
            id: format.zero_id(),
            offset: 17,
            crc32: 83,
        };
        let mut entries = [sentinel; 8];
        let publication = allocations::without_allocations(|| {
            Publication::new(
                &repository,
                &mut entries,
                &mut [0; 3],
                LooseObjects::Disabled,
            )
        })
        .unwrap();
        assert_eq!(
            publication.checksum().as_bytes(),
            &expected.0[expected.0.len() - format.id_length()..]
        );
        assert_eq!(publication.pack_size(), expected.0.len() as u64);
        assert_eq!(publication.index_size(), expected.1.len() as u64);
        let listing = format!("P pack-{}.pack\n\n", publication.checksum());
        for capacity in [1, 7, 31, 4096] {
            let (response, bytes) = drain(&publication, "objects/info/packs", capacity);
            assert_eq!(response.status, 200);
            assert_eq!(bytes, listing.as_bytes());
            let (response, bytes) = drain(&publication, &path(&publication, "pack"), capacity);
            assert_eq!(response.content_type, "application/x-git-packed-objects");
            assert_eq!(bytes, expected.0);
            let (response, bytes) = drain(&publication, &path(&publication, "idx"), capacity);
            assert_eq!(
                response.content_type,
                "application/x-git-packed-objects-toc"
            );
            assert_eq!(bytes, expected.1);
        }
        let mut sink_bytes = vec![0; expected.0.len()];
        let mut output = SliceWriter::new(&mut sink_bytes);
        assert_eq!(
            publication
                .serve(get(&path(&publication, "pack")), &mut [0; 43], &mut output)
                .unwrap()
                .unwrap()
                .status,
            200
        );
        assert_eq!(output.as_bytes(), expected.0);
        assert!(
            entries[repository.object_count()..]
                .iter()
                .all(|entry| *entry == sentinel)
        );
    }
}

#[test]
fn empty_publication_has_a_valid_empty_pack_and_index() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let repository = EmptyRepository::new_with_format("refs/heads/main", format).unwrap();
        let publication =
            Publication::new(&repository, &mut [], &mut [], LooseObjects::Disabled).unwrap();
        let expected = encode(&repository);
        assert_eq!(
            drain(&publication, &path(&publication, "pack"), 1).1,
            expected.0
        );
        assert_eq!(
            drain(&publication, &path(&publication, "idx"), 1).1,
            expected.1
        );
        assert_eq!(publication.pack_size(), (12 + format.id_length()) as u64);
    }
}

#[test]
fn generated_objects_need_only_bounded_metadata_and_short_read_scratch() {
    let repository = streamed::GeneratedRepository::new(180_001, 3);
    let expected = encode(&repository);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = allocations::without_allocations(|| {
        Publication::new(
            &repository,
            &mut metadata,
            &mut [0; 1],
            LooseObjects::Enabled,
        )
    })
    .unwrap();
    assert_eq!(
        drain(&publication, &path(&publication, "pack"), 29).1,
        expected.0
    );
    assert_eq!(
        drain(&publication, &path(&publication, "idx"), 13).1,
        expected.1
    );
}

#[test]
fn routing_rejects_wrong_names_and_ambiguous_metadata_without_fallback_or_output() {
    let repository = streamed::GeneratedRepository::new(19, 2);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 7],
        LooseObjects::Disabled,
    )
    .unwrap();
    for target in [
        "objects/pack/pack-deadbeef.pack",
        "objects/pack/pack-deadbeef.idx",
        "objects/info/alternates",
        "objects/00/1234",
    ] {
        let (response, bytes) = drain(&publication, target, 1);
        assert_eq!(response.status, 404);
        assert!(bytes.is_empty());
    }
    let valid = path(&publication, "pack");
    for target in [
        format!("{valid}.idx"),
        valid
            .to_uppercase()
            .replace("OBJECTS/PACK/PACK-", "objects/pack/pack-"),
        valid.replace(".pack", ".pack/extra"),
    ] {
        assert_eq!(drain(&publication, &target, 1).0.status, 404);
    }
    for target in ["HEAD", "info/refs", "refs/heads/main"] {
        let mut bytes = [0xa7; 13];
        let mut sink = SliceWriter::new(&mut bytes);
        assert!(
            publication
                .serve(get(target), &mut [0; 1], &mut sink)
                .unwrap()
                .is_none()
        );
        assert!(sink.is_empty());
        assert_eq!(bytes, [0xa7; 13]);
    }
    for target in [
        "objects/pack/../secret",
        "objects/pack/%2e%2e/secret",
        "objects/pack/pack-%xx.idx",
        "objects//pack/file",
    ] {
        assert!(matches!(
            publication.prepare(get(target)),
            Err(Error::InvalidRequest)
        ));
    }
    let mut request = get(&valid);
    request.method = "POST";
    assert_eq!(
        publication
            .prepare(request)
            .unwrap()
            .unwrap()
            .response()
            .status,
        405
    );
    request.method = "DELETE";
    assert_eq!(
        publication
            .prepare(request)
            .unwrap()
            .unwrap()
            .response()
            .status,
        405
    );
    request = get(&valid);
    request.git_protocol = Some("version=2:version=0");
    assert!(matches!(
        publication.prepare(request),
        Err(Error::InvalidRequest)
    ));
    request = get(&valid);
    request.query = Some("service=x&service=y");
    assert!(matches!(
        publication.prepare(request),
        Err(Error::InvalidRequest)
    ));
}

#[test]
fn loose_routes_are_an_explicit_host_choice_and_cancellation_is_per_response() {
    let repository = streamed::GeneratedRepository::new(201, 7);
    for loose in [LooseObjects::Enabled, LooseObjects::Disabled] {
        let mut metadata = [IndexEntry::EMPTY; 3];
        let publication = Publication::new(&repository, &mut metadata, &mut [0; 9], loose).unwrap();
        let id = repository.head().to_string();
        let loose_path = format!("objects/{}/{}", &id[..2], &id[2..]);
        let result = publication.prepare(get(&loose_path)).unwrap();
        assert_eq!(result.is_none(), loose == LooseObjects::Enabled);
        let target = path(&publication, "pack");
        let expected = drain(&publication, &target, 7).1;
        let mut first = publication.prepare(get(&target)).unwrap().unwrap();
        let mut second = publication.prepare(get(&target)).unwrap().unwrap();
        assert_eq!(first.read(&mut []), Err(Error::BufferTooSmall));
        assert_eq!(first.read(&mut [0; 1]), Ok(1));
        first.cancel();
        assert_eq!(first.read(&mut [0; 7]), Err(Error::InvalidRequest));
        let mut bytes = Vec::new();
        let mut scratch = [0; 23];
        loop {
            let count = second.read(&mut scratch).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&scratch[..count]);
        }
        assert_eq!(bytes, expected);
    }
}

struct Fallible<'a> {
    source: &'a streamed::GeneratedRepository,
    fail: Cell<bool>,
    reads: Cell<usize>,
}
impl Repository for Fallible<'_> {
    fn head(&self) -> ObjectId {
        self.source.head()
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
    fn read_object(&self, index: usize, offset: u64, bytes: &mut [u8]) -> git_zero::Result<usize> {
        self.reads.set(self.reads.get() + 1);
        if self.fail.get() {
            return Err(Error::Backend);
        }
        self.source.read_object(index, offset, bytes)
    }
}

#[test]
fn provider_failure_invalidates_only_its_cursor_and_indexes_need_no_object_reads() {
    let source = streamed::GeneratedRepository::new(163, 3);
    let repository = Fallible {
        source: &source,
        fail: Cell::new(false),
        reads: Cell::new(0),
    };
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 5],
        LooseObjects::Disabled,
    )
    .unwrap();
    let reads = repository.reads.get();
    repository.fail.set(true);
    let (_, index) = drain(&publication, &path(&publication, "idx"), 1);
    assert!(!index.is_empty());
    assert_eq!(repository.reads.get(), reads);
    let target = path(&publication, "pack");
    let mut cursor = publication.prepare(get(&target)).unwrap().unwrap();
    let mut scratch = [0; 31];
    while cursor.read(&mut scratch).is_ok() {}
    repository.fail.set(false);
    assert_eq!(cursor.read(&mut scratch), Err(Error::InvalidRequest));
    assert_eq!(drain(&publication, &target, 19).0.status, 200);
    repository.fail.set(true);
    assert!(matches!(
        Publication::new(
            &repository,
            &mut metadata,
            &mut [0; 5],
            LooseObjects::Disabled
        ),
        Err(Error::Backend)
    ));
    repository.fail.set(false);
    assert!(
        Publication::new(
            &repository,
            &mut metadata,
            &mut [0; 5],
            LooseObjects::Disabled
        )
        .is_ok()
    );
}

#[test]
fn preparation_capacity_errors_recover_without_publishing_partial_metadata() {
    let repository = streamed::GeneratedRepository::new(163, 3);
    let mut metadata = [IndexEntry::EMPTY; 3];
    assert!(matches!(
        Publication::new(
            &repository,
            &mut metadata[..2],
            &mut [0; 5],
            LooseObjects::Disabled
        ),
        Err(Error::BufferTooSmall)
    ));
    assert!(matches!(
        Publication::new(&repository, &mut metadata, &mut [], LooseObjects::Disabled),
        Err(Error::BufferTooSmall)
    ));
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 5],
        LooseObjects::Disabled,
    )
    .unwrap();
    let mut sink = SliceWriter::new(&mut []);
    assert!(matches!(
        publication.serve(get("objects/info/packs"), &mut [], &mut sink),
        Err(Error::BufferTooSmall)
    ));
    assert!(sink.is_empty());
    assert_eq!(
        drain(&publication, &path(&publication, "pack"), 1).0.status,
        200
    );
}

struct FailedOutput;
impl Write for FailedOutput {
    fn write_all(&mut self, _: &[u8]) -> git_zero::Result<()> {
        Err(Error::Backend)
    }
}
#[test]
fn sink_failure_does_not_change_the_publication_or_another_response() {
    let repository = streamed::GeneratedRepository::new(163, 3);
    let mut metadata = [IndexEntry::EMPTY; 3];
    let publication = Publication::new(
        &repository,
        &mut metadata,
        &mut [0; 5],
        LooseObjects::Disabled,
    )
    .unwrap();
    let target = path(&publication, "pack");
    let expected = drain(&publication, &target, 31).1;
    assert!(matches!(
        publication.serve(get(&target), &mut [0; 3], &mut FailedOutput),
        Err(Error::Backend)
    ));
    assert_eq!(drain(&publication, &target, 3).1, expected);
}
