#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/streamed.rs"]
mod streamed;

use git_zero::{
    Error, File, ObjectFormat, ObjectId, ObjectInfo, Repository, Result, Snapshot,
    ValidatedRepository,
    http::{
        self, Request, SelectionStorage,
        stream::{RequestBody, ResponseEncoder},
    },
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};
use std::cell::Cell;

fn collect(mut read: impl FnMut(&mut [u8]) -> Result<usize>, capacity: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut storage = vec![0xa5; capacity + 2];
    loop {
        let count = allocations::without_allocations(|| read(&mut storage[1..=capacity])).unwrap();
        assert!(count <= capacity);
        assert_eq!(storage[0], 0xa5);
        assert_eq!(storage[capacity + 1], 0xa5);
        if count == 0 {
            break;
        }
        // Model a host accepting one byte per write, retaining the unsent tail.
        for byte in &storage[1..1 + count] {
            result.push(*byte);
        }
    }
    assert_eq!(read(&mut []).unwrap(), 0);
    result
}

#[test]
fn pull_packs_and_loose_objects_match_synchronous_encoders_at_all_block_boundaries() {
    for size in [0, 1, 65534, 65535, 65536, 131071] {
        let repository = streamed::GeneratedRepository::new(size, 29);
        let mut expected = vec![0; size as usize + 4096];
        let mut writer = SliceWriter::new(&mut expected);
        let checksum = pack::write(&repository, &mut writer).unwrap();
        for capacity in [1, 7, 1024, 65536] {
            let mut encoder =
                allocations::without_allocations(|| pack::Encoder::new(&repository, None)).unwrap();
            assert_eq!(
                encoder.read(&repository, &mut []),
                Err(Error::BufferTooSmall)
            );
            let bytes = collect(|output| encoder.read(&repository, output), capacity);
            assert_eq!(bytes, writer.as_bytes());
            assert_eq!(encoder.checksum(), Some(checksum));
            pack::validate(&bytes).unwrap();
            for index in 0..repository.object_count() {
                let mut loose = SliceWriter::new(&mut expected);
                pack::write_loose_with_scratch(&repository, index, &mut [0; 17], &mut loose)
                    .unwrap();
                let mut encoder = pack::LooseEncoder::new(&repository, index).unwrap();
                assert_eq!(
                    collect(|output| encoder.read(&repository, output), capacity),
                    loose.as_bytes()
                );
            }
            // Restore the expected pack after reusing the bounded oracle buffer.
            writer = SliceWriter::new(&mut expected);
            pack::write(&repository, &mut writer).unwrap();
        }
    }
}

#[test]
fn selected_packs_preserve_format_order_empty_closures_and_validate_before_output() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let files = [File {
            name: "blob",
            contents: b"hello",
        }];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        for indices in [&[][..], &[2, 0, 1][..], &[1][..]] {
            let mut bytes = [0; 2048];
            let mut output = SliceWriter::new(&mut bytes);
            let checksum =
                pack::write_selected_with_scratch(&repository, indices, &mut [0; 13], &mut output)
                    .unwrap();
            let mut encoder = pack::Encoder::new(&repository, Some(indices)).unwrap();
            assert_eq!(
                collect(|output| encoder.read(&repository, output), 3),
                output.as_bytes()
            );
            assert_eq!(encoder.checksum(), Some(checksum));
        }
        assert!(matches!(
            pack::Encoder::new(&repository, Some(&[0, 0])),
            Err(Error::InvalidObject)
        ));
        assert!(matches!(
            pack::Encoder::new(&repository, Some(&[3])),
            Err(Error::ObjectNotFound)
        ));
    }
}

fn request<'a>(
    method: &'a str,
    path: &'a str,
    body: &'a [u8],
    version: Option<&'a str>,
) -> Request<'a> {
    Request {
        method,
        path,
        body,
        git_protocol: version,
        query: None,
        content_type: Some("application/x-git-upload-pack-request"),
    }
}

fn packet(body: &mut Vec<u8>, line: &str) {
    body.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
}

fn upload(repository: &impl Repository, version: u8, band: &str) -> Vec<u8> {
    let mut body = Vec::new();
    if version == 2 {
        packet(&mut body, "command=fetch\n");
        packet(
            &mut body,
            core::str::from_utf8(repository.object_format().capability()).unwrap(),
        );
        body.extend_from_slice(b"0001");
        packet(&mut body, &format!("want {}\n", repository.head()));
        packet(&mut body, "done\n");
        body.extend_from_slice(b"0000");
    } else {
        packet(&mut body, &format!("want {}{band}\n", repository.head()));
        body.extend_from_slice(b"0000");
        packet(&mut body, "done\n");
    }
    body
}

fn pack_bytes(mut wire: &[u8], version: u8, band: &str) -> Vec<u8> {
    assert_eq!(
        pktline::read(&mut wire).unwrap(),
        Some(Packet::Data(if version == 2 {
            b"packfile\n"
        } else {
            b"NAK\n"
        }))
    );
    if version != 2 && band.is_empty() {
        return wire.to_vec();
    }
    let mut result = Vec::new();
    loop {
        match pktline::read(&mut wire).unwrap().unwrap() {
            Packet::Data(bytes) => {
                assert_eq!(bytes[0], 1);
                result.extend_from_slice(&bytes[1..]);
            }
            Packet::Flush => {
                assert!(wire.is_empty());
                return result;
            }
        }
    }
}

#[test]
fn response_cursors_cover_all_protocols_formats_selection_and_loose_routes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 256];
        let mut commit = [0; 512];
        let files = [File {
            name: "blob",
            contents: b"a small body",
        }];
        let repository = Snapshot::new_with_format(&files, &mut tree, &mut commit, format).unwrap();
        let mut metadata = [None; 3];
        let mut by_id = [0; 3];
        let repository = ValidatedRepository::new(
            &repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 17],
            &mut [0; 128],
        )
        .unwrap();
        for version in 0..=2 {
            let header = format!("version={version}");
            let header = (version != 0).then_some(header.as_str());
            let mut discovery = request("GET", "info/refs", &[], header);
            discovery.query = Some("service=git-upload-pack");
            for req in [
                discovery,
                request("GET", "HEAD", &[], header),
                request("GET", "refs/heads/main", &[], header),
                request("DELETE", "HEAD", &[], header),
            ] {
                let mut expected = [0; 2048];
                let mut output = SliceWriter::new(&mut expected);
                let response = http::serve(&repository, req, &mut output).unwrap();
                let mut control = [0; 2048];
                let mut encoder = ResponseEncoder::new(&repository, req, &mut control).unwrap();
                assert_eq!(encoder.response().status, response.status);
                assert_eq!(
                    collect(|output| encoder.read(&repository, output), 1),
                    output.as_bytes()
                );
            }
            for band in ["", " side-band", " side-band-64k"] {
                let body = upload(&repository, version, band);
                let req = request("POST", "git-upload-pack", &body, header);
                let mut expected = [0; 2048];
                let mut output = SliceWriter::new(&mut expected);
                http::serve(&repository, req, &mut output).unwrap();
                let expected = pack_bytes(output.as_bytes(), version, band);
                for selected in [false, true] {
                    let mut wants = [format.zero_id(); 4];
                    let mut objects = [None; 3];
                    let mut indices = [0; 3];
                    let mut read = [0; 17];
                    let mut parse = [0; 128];
                    let mut selection = SelectionStorage {
                        wants: &mut wants,
                        objects: &mut objects,
                        indices: &mut indices,
                        read: &mut read,
                        parse: &mut parse,
                        graph_read_limit: 4096,
                    };
                    let mut control = [0; 2048];
                    let mut encoder = if selected {
                        ResponseEncoder::with_selection(
                            &repository,
                            req,
                            &mut selection,
                            &mut control,
                        )
                    } else {
                        ResponseEncoder::new(&repository, req, &mut control)
                    }
                    .unwrap();
                    let actual = collect(|output| encoder.read(&repository, output), 3);
                    assert_eq!(pack_bytes(&actual, version, band), expected);
                }
            }
        }
        for index in 0..repository.object_count() {
            let hex = repository.object_id(index).unwrap().to_string();
            let path = format!("objects/{}/{}", &hex[..2], &hex[2..]);
            let req = request("GET", &path, &[], None);
            let mut expected = [0; 2048];
            let mut output = SliceWriter::new(&mut expected);
            http::serve(&repository, req, &mut output).unwrap();
            let mut control = []; // Loose bodies never need a control buffer.
            let mut encoder = ResponseEncoder::new(&repository, req, &mut control).unwrap();
            assert_eq!(
                collect(|output| encoder.read(&repository, output), 7),
                output.as_bytes()
            );
        }
    }
}

struct Observed {
    repository: streamed::GeneratedRepository,
    reads: Cell<usize>,
    fail: Cell<bool>,
}
impl Repository for Observed {
    fn head(&self) -> ObjectId {
        self.repository.head()
    }
    fn object_count(&self) -> usize {
        self.repository.object_count()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.repository.object_info(index)
    }
    fn read_object(&self, index: usize, offset: u64, output: &mut [u8]) -> Result<usize> {
        self.reads.set(self.reads.get() + 1);
        if self.fail.get() {
            return Err(Error::Backend);
        }
        self.repository.read_object(index, offset, output)
    }
}

#[test]
fn backpressure_retry_cancellation_and_interleaved_responses_preserve_state() {
    let repository = Observed {
        repository: streamed::GeneratedRepository::new(65536, 11),
        reads: Cell::new(0),
        fail: Cell::new(false),
    };
    let body = upload(&repository, 0, "");
    let req = request("POST", "git-upload-pack", &body, None);
    let mut first_control = [0; 128];
    let mut second_control = [0; 128];
    let mut first = ResponseEncoder::new(&repository, req, &mut first_control).unwrap();
    let mut second = ResponseEncoder::new(&repository, req, &mut second_control).unwrap();
    assert_eq!(repository.reads.get(), 0);
    assert_eq!(first.read(&repository, &mut []), Err(Error::BufferTooSmall));
    let mut prefix = Vec::new();
    let mut output = [0; 7];
    repository.fail.set(true);
    loop {
        match first.read(&repository, &mut output) {
            Ok(count) => prefix.extend_from_slice(&output[..count]),
            Err(Error::Backend) => break,
            result => panic!("unexpected {result:?}"),
        }
    }
    let reads = repository.reads.get();
    assert_eq!(first.read(&repository, &mut output), Err(Error::Backend));
    assert_eq!(repository.reads.get(), reads + 1);
    // A paused host makes no further pull, so the provider is never called.
    repository.fail.set(false);
    prefix.extend_from_slice(&collect(|output| first.read(&repository, output), 7));
    assert_eq!(
        prefix,
        collect(|output| second.read(&repository, output), 13)
    );
    let mut control = [0; 128];
    let mut cancelled = ResponseEncoder::new(&repository, req, &mut control).unwrap();
    let before = repository.reads.get();
    while repository.reads.get() == before {
        assert!(cancelled.read(&repository, &mut output).unwrap() > 0);
    }
    cancelled.cancel();
    let reads = repository.reads.get();
    assert_eq!(
        cancelled.read(&repository, &mut output),
        Err(Error::InvalidRequest)
    );
    assert_eq!(repository.reads.get(), reads);
}

#[test]
fn incremental_request_intake_enforces_lengths_eof_limits_and_terminal_cancel() {
    assert!(core::mem::size_of::<pack::Encoder<'_>>() <= 512);
    assert!(core::mem::size_of::<ResponseEncoder<'_>>() <= 2048);
    for split in 0..=8 {
        let mut storage = [0; 8];
        let mut body = RequestBody::new(&mut storage, Some(8)).unwrap();
        body.push(&b"01234567"[..split]).unwrap();
        assert_eq!(body.received(), split);
        body.push(&b"01234567"[split..]).unwrap();
        assert_eq!(body.push(b"x"), Err(Error::InvalidRequest));
        assert_eq!(body.finish().unwrap(), b"01234567");
    }
    let mut storage = [0; 4];
    assert!(matches!(
        RequestBody::new(&mut storage, Some(5)),
        Err(Error::BufferTooSmall)
    ));
    let mut body = RequestBody::new(&mut storage, None).unwrap();
    body.push(b"abc").unwrap();
    assert_eq!(body.push(b"de"), Err(Error::BufferTooSmall));
    assert_eq!(body.finish().unwrap(), b"abc");
    let body = RequestBody::new(&mut storage, Some(4)).unwrap();
    assert_eq!(body.finish(), Err(Error::InvalidRequest));
    let mut body = RequestBody::new(&mut storage, None).unwrap();
    body.cancel();
    assert_eq!(body.push(b""), Err(Error::InvalidRequest));
    assert_eq!(body.finish(), Err(Error::InvalidRequest));
}
