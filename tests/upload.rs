#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/http_request.rs"]
mod http_request;

use git_zero::{
    Error, File, ObjectFormat, ObjectId, ObjectInfo, Repository, Snapshot, ValidatedRepository,
    http::{self, Request, SelectionStorage},
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};

fn packet(body: &mut Vec<u8>, line: &[u8]) {
    body.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    body.extend_from_slice(line);
}

struct FailedMetadata<'a>(&'a dyn Repository);

impl Repository for FailedMetadata<'_> {
    fn head(&self) -> ObjectId {
        self.0.head()
    }
    fn object_count(&self) -> usize {
        self.0.object_count()
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.0.object_id(index)
    }
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        self.0.find_index(id)
    }
    fn object_info(&self, _index: usize) -> git_zero::Result<ObjectInfo> {
        Err(Error::Backend)
    }
}

#[test]
fn have_metadata_failures_propagate_in_every_ack_mode_before_pack_writers_run() {
    let mut tree = [0; 128];
    let mut commit = [0; 512];
    let snapshot = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let repository = FailedMetadata(&snapshot);
    for caps in [
        "",
        " multi_ack",
        " multi_ack_detailed",
        " multi_ack multi_ack_detailed",
        " multi_ack_detailed multi_ack",
        " multi_ack ",
        "  multi_ack_detailed  ",
    ] {
        for done in [false, true] {
            let body = body(snapshot.head(), caps, &[snapshot.head()], done);
            assert_eq!(assert_rejected(&repository, &body), Error::Backend);
        }
    }
}

fn body(head: ObjectId, caps: &str, haves: &[ObjectId], done: bool) -> Vec<u8> {
    let mut body = Vec::new();
    packet(&mut body, format!("want {head}{caps}\n").as_bytes());
    body.extend_from_slice(b"0000");
    for id in haves {
        packet(&mut body, format!("have {id}\n").as_bytes());
    }
    if done {
        packet(&mut body, b"done\n");
    } else if !haves.is_empty() {
        body.extend_from_slice(b"0000");
    }
    body
}

fn request(body: &[u8]) -> Request<'_> {
    http_request::upload(body, 0)
}

fn for_each_format(files: &[File<'_>], mut check: impl FnMut(&Snapshot<'_>)) {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let mut tree = [0; 128];
        let mut commit = [0; 512];
        let snapshot = Snapshot::new_with_format(files, &mut tree, &mut commit, format).unwrap();
        check(&snapshot);
    }
}

fn assert_packet(wire: &mut &[u8], expected: &[u8]) {
    assert_eq!(pktline::read(wire).unwrap(), Some(Packet::Data(expected)));
}

fn check_ack_round(snapshot: &impl Repository, caps: &str, status: &str, done: bool) {
    let head = snapshot.head();
    let format = snapshot.object_format();
    let unknown = ObjectId::from_bytes(format, &vec![9; format.id_length()]).unwrap();
    let body = body(
        head,
        caps,
        &[unknown, snapshot.object_id(1).unwrap(), head],
        done,
    );
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    allocations::without_allocations(|| http::serve(snapshot, request(&body), &mut output))
        .unwrap();
    let mut wire = output.as_bytes();
    assert_packet(&mut wire, format!("ACK {head}{status}\n").as_bytes());
    if !caps.is_empty() {
        let final_ack = if done {
            format!("ACK {head}\n")
        } else {
            "NAK\n".to_owned()
        };
        assert_packet(&mut wire, final_ack.as_bytes());
    }
    if done {
        pack::validate_with_format(wire, format).unwrap();
    } else {
        assert!(wire.is_empty());
    }
}

#[test]
fn stateless_ack_modes_ignore_unknown_and_non_commit_haves_without_allocating() {
    for_each_format(&[], |snapshot| {
        for (caps, status) in [
            ("", ""),
            (" multi_ack", " continue"),
            (" multi_ack_detailed", " common"),
            (" multi_ack multi_ack_detailed", " common"),
            (" multi_ack_detailed multi_ack", " common"),
            (" multi_ack ", " continue"),
            ("  multi_ack_detailed  ", " common"),
            (" multi_ack  multi_ack_detailed ", " common"),
        ] {
            check_ack_round(snapshot, caps, status, false);
            check_ack_round(snapshot, caps, status, true);
        }
    });
}

fn decode_sideband(mut wire: &[u8], limit: usize) -> Vec<u8> {
    assert_packet(&mut wire, b"NAK\n");
    let mut packed = Vec::new();
    loop {
        let before = wire.len();
        let packet = pktline::read(&mut wire).unwrap().unwrap();
        assert!(before - wire.len() <= limit);
        match packet {
            Packet::Data(data) => {
                assert_eq!(data[0], 1);
                packed.extend_from_slice(&data[1..]);
            }
            Packet::Flush => break,
        }
    }
    assert!(wire.is_empty());
    packed
}

fn check_sideband(snapshot: &impl Repository, caps: &str, limit: usize, expected: &[u8]) {
    let body = body(snapshot.head(), caps, &[], true);
    let mut bytes = vec![0xcc; 85_002];
    let mut output = SliceWriter::new(&mut bytes[1..85_001]);
    allocations::without_allocations(|| http::serve(snapshot, request(&body), &mut output))
        .unwrap();
    let packed = decode_sideband(output.as_bytes(), limit);
    assert_eq!(packed, expected);
    pack::validate_with_format(&packed, snapshot.object_format()).unwrap();
    assert_eq!((bytes[0], bytes[85_001]), (0xcc, 0xcc));
}

#[test]
fn sideband_modes_preserve_large_pack_bytes_checksums_and_packet_bounds() {
    let contents = vec![0xab; 70_000];
    let files = [File {
        name: "large",
        contents: &contents,
    }];
    for_each_format(&files, |snapshot| {
        let mut expected_storage = vec![0; 80_000];
        let mut expected = SliceWriter::new(&mut expected_storage);
        pack::write(snapshot, &mut expected).unwrap();
        check_sideband(
            snapshot,
            " side-band no-progress",
            1000,
            expected.as_bytes(),
        );
        check_sideband(
            snapshot,
            " side-band-64k",
            pktline::MAX_LENGTH,
            expected.as_bytes(),
        );
    });
}

fn assert_rejected(repository: &impl Repository, bytes: &[u8]) -> Error {
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    let mut calls = 0;
    let result = allocations::without_allocations(|| {
        http::serve_with_pack(repository, request(bytes), &mut [], &mut output, |_, _| {
            calls += 1;
            Ok(())
        })
    });
    assert_eq!(calls, 0);
    assert!(output.is_empty());
    result.expect_err("invalid upload request succeeded")
}

#[test]
fn parser_rejects_phase_errors_conflicting_caps_and_truncations_before_output() {
    let mut tree = [0; 128];
    let mut commit = [0; 512];
    let snapshot = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let head = snapshot.head();
    for caps in [
        " multi_ack multi_ack",
        " multi_ack_detailed multi_ack_detailed",
        " side-band side-band-64k",
        " no-progress no-progress",
        " multi_ack\t",
        " filter",
        " no-done",
        " include-tag",
        " object-format=sha256",
    ] {
        assert_rejected(&snapshot, &body(head, caps, &[], true));
    }
    for lines in [
        vec![format!("have {head}\n"), format!("want {head}\n")],
        vec![format!("want {head}\n"), format!("have {head}\n")],
        vec![format!("want {head}\n"), "done\n".to_owned()],
    ] {
        let mut bytes = Vec::new();
        for line in lines {
            packet(&mut bytes, line.as_bytes());
        }
        assert_rejected(&snapshot, &bytes);
    }
    for extra in [
        format!("want {head}\n"),
        format!("have {head}\n"),
        "done\n".to_owned(),
        "shallow bad\n".to_owned(),
    ] {
        let mut bytes = body(head, "", &[], true);
        packet(&mut bytes, extra.as_bytes());
        assert_rejected(&snapshot, &bytes);
    }
    let valid = body(head, " multi_ack_detailed side-band-64k", &[head], true);
    for length in 0..valid.len() {
        // The wants flush is itself a complete negotiation round.
        if valid[..length].ends_with(b"0000") {
            continue;
        }
        assert_rejected(&snapshot, &valid[..length]);
    }
}

fn assert_selection_retry(
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    selection: &mut SelectionStorage<'_>,
    body: &[u8],
    object_count: u32,
) {
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    selection.graph_read_limit = 0;
    assert!(matches!(
        http::serve_with_selection(repository, request(body), selection, &mut output),
        Err(Error::LimitExceeded)
    ));
    assert!(output.is_empty());
    selection.graph_read_limit = 4096;
    allocations::without_allocations(|| {
        http::serve_with_selection(repository, request(body), selection, &mut output)
    })
    .unwrap();
    let mut wire = output.as_bytes();
    pktline::read(&mut wire).unwrap();
    pack::validate(wire).unwrap();
    assert_eq!(
        u32::from_be_bytes(wire[8..12].try_into().unwrap()),
        object_count
    );
}

#[test]
fn selected_haves_can_omit_every_object_and_retries_reset_exclusions() {
    let mut tree = [0; 128];
    let mut commit = [0; 512];
    let snapshot = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let mut metadata = [None; 2];
    let mut by_id = [0; 2];
    let repository = ValidatedRepository::new(
        &snapshot,
        &mut metadata,
        &mut by_id,
        &mut [0; 1],
        &mut [0; 128],
    )
    .unwrap();
    let mut wants = [snapshot.head(); 1];
    let mut work = [None; 2];
    let mut indices = [0; 2];
    let mut read = [0; 1];
    let mut parse = [0; 128];
    let mut selection = SelectionStorage {
        wants: &mut wants,
        objects: &mut work,
        indices: &mut indices,
        read: &mut read,
        parse: &mut parse,
        graph_read_limit: 0,
    };
    let head = snapshot.head();
    assert_selection_retry(
        &repository,
        &mut selection,
        &body(head, "", &[head], true),
        0,
    );
    assert_selection_retry(&repository, &mut selection, &body(head, "", &[], true), 2);
}

#[test]
fn bounded_error_packets_replace_failed_output_without_backend_details() {
    for error in [
        Error::InvalidRequest,
        Error::Unsupported,
        Error::Backend,
        Error::LimitExceeded,
        Error::BufferTooSmall,
    ] {
        let mut bytes = [0; 128];
        let mut output = SliceWriter::new(&mut bytes);
        allocations::without_allocations(|| http::write_upload_error(error, &mut output)).unwrap();
        let mut wire = output.as_bytes();
        let Some(Packet::Data(line)) = pktline::read(&mut wire).unwrap() else {
            panic!("missing error");
        };
        assert!(line.starts_with(b"ERR "));
        assert!(wire.is_empty());
        assert_eq!(
            http::write_upload_error(error, &mut SliceWriter::new(&mut [0; 4])),
            Err(Error::BufferTooSmall)
        );
    }
}

#[test]
fn sideband_output_failures_propagate_and_fresh_requests_can_retry() {
    let mut tree = [0; 128];
    let mut commit = [0; 512];
    let snapshot = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let body = body(snapshot.head(), " side-band-64k", &[], true);
    for capacity in [0, 4, 8, 9, 16, 32] {
        let mut storage = [0xcc; 34];
        let mut output = SliceWriter::new(&mut storage[1..1 + capacity]);
        assert!(matches!(
            http::serve(&snapshot, request(&body), &mut output),
            Err(Error::BufferTooSmall)
        ));
        assert_eq!((storage[0], storage[capacity + 1]), (0xcc, 0xcc));
    }
    let mut storage = [0; 4096];
    let mut output = SliceWriter::new(&mut storage);
    assert!(matches!(
        http::serve_with_pack(
            &snapshot,
            request(&body),
            &mut [],
            &mut output,
            |_, packed| {
                git_zero::io::Write::write_all(packed, b"incomplete pack")?;
                Err(Error::Backend)
            }
        ),
        Err(Error::Backend)
    ));
    assert_eq!(output.as_bytes(), b"0008NAK\n");
    let mut output = SliceWriter::new(&mut storage);
    allocations::without_allocations(|| http::serve(&snapshot, request(&body), &mut output))
        .unwrap();
    assert!(output.as_bytes().ends_with(b"0000"));
}
