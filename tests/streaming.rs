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

#[path = "common/pull.rs"]
mod pull;
use pull::collect;

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
    append_sideband(wire, &mut result);
    result
}

#[test]
fn response_cursors_cover_all_protocols_formats_selection_and_loose_routes() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        check_repository_responses(format);
    }
}

fn check_repository_responses(format: ObjectFormat) {
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
        check_protocol(&repository, version);
    }
    for index in 0..repository.object_count() {
        let hex = repository.object_id(index).unwrap().to_string();
        let path = format!("objects/{}/{}", &hex[..2], &hex[2..]);
        check_buffered_response(&repository, request("GET", &path, &[], None), &mut []);
    }
}

fn check_buffered_response(repository: &impl Repository, req: Request<'_>, control: &mut [u8]) {
    let mut expected = [0; 2048];
    let mut output = SliceWriter::new(&mut expected);
    let response = http::serve(repository, req, &mut output).unwrap();
    let mut encoder = ResponseEncoder::new(repository, req, control).unwrap();
    assert_eq!(encoder.response().status, response.status);
    assert_eq!(encoder.response().content_type, response.content_type);
    assert_eq!(
        collect(|output| encoder.read(repository, output), 1),
        output.as_bytes()
    );
}

#[test]
fn preparation_errors_release_buffers_for_retry_without_publishing_a_response() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let files = [File {
        name: "blob",
        contents: b"body",
    }];
    let repository = Snapshot::new(&files, &mut tree, &mut commit).unwrap();
    let mut control = [0xa5; 130];
    let body = upload(&repository, 0, "");
    let req = request("POST", "git-upload-pack", &body, None);
    assert!(matches!(
        ResponseEncoder::new(&repository, req, &mut control[1..2]),
        Err(Error::BufferTooSmall)
    ));
    assert_eq!(control[0], 0xa5);
    assert_eq!(control[2], 0xa5);
    let mut invalid = req;
    invalid.body = &body[..body.len() - 1];
    assert!(matches!(
        ResponseEncoder::new(&repository, invalid, &mut control[1..129]),
        Err(Error::InvalidPacket)
    ));
    let mut unadvertised = Vec::new();
    packet(
        &mut unadvertised,
        &format!("want {}\n", repository.object_id(2).unwrap()),
    );
    unadvertised.extend_from_slice(b"0000");
    packet(&mut unadvertised, "done\n");
    invalid.body = &unadvertised;
    assert!(matches!(
        ResponseEncoder::new(&repository, invalid, &mut control[1..129]),
        Err(Error::InvalidRequest)
    ));
    let mut encoder = ResponseEncoder::new(&repository, req, &mut control[1..129]).unwrap();
    let wire = collect(|output| encoder.read(&repository, output), 1);
    pack::validate(&pack_bytes(&wire, 0, "")).unwrap();
    assert_eq!(control[0], 0xa5);
    assert_eq!(control[129], 0xa5);
}

#[test]
fn empty_common_closures_and_negotiation_only_streams_have_distinct_terminators() {
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut metadata = [None; 2];
    let mut by_id = [0; 2];
    let repository = ValidatedRepository::new(
        &repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 17],
        &mut [0; 128],
    )
    .unwrap();
    let mut body = Vec::new();
    packet(
        &mut body,
        &format!("want {} side-band\n", repository.head()),
    );
    body.extend_from_slice(b"0000");
    packet(&mut body, &format!("have {}\n", repository.head()));
    let done = body.len();
    packet(&mut body, "done\n");
    let mut expected = [0; 2048];
    let mut output = SliceWriter::new(&mut expected);
    pack::write_selected_with_scratch(&repository, &[], &mut [0; 1], &mut output).unwrap();
    let wire = streaming_upload(
        &repository,
        request("POST", "git-upload-pack", &body, None),
        true,
    );
    let mut wire = wire.as_slice();
    let ack = format!("ACK {}\n", repository.head());
    assert_eq!(
        pktline::read(&mut wire).unwrap(),
        Some(Packet::Data(ack.as_bytes()))
    );
    let mut empty_pack = Vec::new();
    append_sideband(wire, &mut empty_pack);
    assert_eq!(empty_pack, output.as_bytes());
    let mut negotiating = body[..done].to_vec();
    negotiating.extend_from_slice(b"0000");
    let wire = streaming_upload(
        &repository,
        request("POST", "git-upload-pack", &negotiating, None),
        true,
    );
    assert_eq!(wire, format!("{:04x}{ack}", ack.len() + 4).as_bytes());
}

fn append_sideband(mut wire: &[u8], output: &mut Vec<u8>) {
    loop {
        match pktline::read(&mut wire).unwrap().unwrap() {
            Packet::Data(bytes) => {
                assert_eq!(bytes[0], 1);
                output.extend_from_slice(&bytes[1..]);
            }
            Packet::Flush => {
                assert!(wire.is_empty());
                return;
            }
        }
    }
}

fn check_protocol(repository: &ValidatedRepository<'_, '_, impl Repository>, version: u8) {
    let value = format!("version={version}");
    let header = (version != 0).then_some(value.as_str());
    let mut discovery = request("GET", "info/refs", &[], header);
    discovery.query = Some("service=git-upload-pack");
    for req in [
        discovery,
        request("GET", "HEAD", &[], header),
        request("GET", "refs/heads/main", &[], header),
        request("DELETE", "HEAD", &[], header),
    ] {
        check_buffered_response(repository, req, &mut [0; 2048]);
    }
    for band in ["", " side-band", " side-band-64k"] {
        check_upload_response(repository, version, band, header);
    }
}

fn check_upload_response(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    version: u8,
    band: &str,
    header: Option<&str>,
) {
    let body = upload(repository, version, band);
    let req = request("POST", "git-upload-pack", &body, header);
    let mut expected = [0; 2048];
    let mut output = SliceWriter::new(&mut expected);
    http::serve(repository, req, &mut output).unwrap();
    let expected = pack_bytes(output.as_bytes(), version, band);
    for selected in [false, true] {
        let actual = streaming_upload(repository, req, selected);
        assert_eq!(pack_bytes(&actual, version, band), expected);
    }
}

fn streaming_upload(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    req: Request<'_>,
    selected: bool,
) -> Vec<u8> {
    let mut wants = [repository.object_format().zero_id(); 4];
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
        ResponseEncoder::with_selection(repository, req, &mut selection, &mut control)
    } else {
        ResponseEncoder::new(repository, req, &mut control)
    }
    .unwrap();
    collect(|output| encoder.read(repository, output), 3)
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
    collect_until_backend_error(&repository, &mut first, &mut prefix, &mut output);
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

fn collect_until_backend_error(
    repository: &impl Repository,
    encoder: &mut ResponseEncoder<'_>,
    prefix: &mut Vec<u8>,
    output: &mut [u8],
) {
    loop {
        match encoder.read(repository, output) {
            Ok(count) => prefix.extend_from_slice(&output[..count]),
            Err(Error::Backend) => break,
            result => panic!("unexpected {result:?}"),
        }
    }
}

#[test]
fn incremental_request_intake_enforces_lengths_eof_limits_and_terminal_cancel() {
    assert!(core::mem::size_of::<pack::Encoder<'_>>() <= 512);
    assert!(core::mem::size_of::<ResponseEncoder<'_>>() <= 2048);
    for split in 0..=8 {
        check_intake_split(split);
    }
    check_intake_eof_and_limits();
}

fn check_intake_split(split: usize) {
    let mut storage = [0; 8];
    let mut body = RequestBody::new(&mut storage, Some(8)).unwrap();
    body.push(&b"01234567"[..split]).unwrap();
    assert_eq!(body.received(), split);
    body.push(&b"01234567"[split..]).unwrap();
    assert_eq!(body.push(b"x"), Err(Error::InvalidRequest));
    assert_eq!(body.finish().unwrap(), b"01234567");
}

fn check_intake_eof_and_limits() {
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
