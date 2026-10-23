use super::MAX_INPUT;
use git_zero::{
    Repository, Snapshot,
    http::{self, Request},
    io::SliceWriter,
    pack,
    pktline::{self, Packet},
};

/// Decode one bounded HTTP request and check any successful response.
///
/// Byte zero selects GET/POST/PUT/DELETE/HEAD; byte one selects a writer capacity
/// of 0, 1, 16, 64, or 4096 bytes. The rest is path, query, and body separated by
/// two NULs; invalid UTF-8 paths/queries are skipped. An empty snapshot uses fixed
/// tree/commit storage and one-byte read scratch. Upload replies must begin with
/// ACK/NAK and raw/sideband packs must validate; output fits the chosen writer.
pub fn routing(data: &[u8]) {
    if data.len() < 2 || data.len() > MAX_INPUT {
        return;
    }
    let mut fields = data[2..].splitn(3, |&byte| byte == 0);
    let Ok(path) = core::str::from_utf8(fields.next().unwrap_or_default()) else {
        return;
    };
    let Ok(query) = core::str::from_utf8(fields.next().unwrap_or_default()) else {
        return;
    };
    let body = fields.next().unwrap_or_default();
    let mut tree = [0; 256];
    let mut commit = [0; 512];
    let repository = Snapshot::new(&[], &mut tree, &mut commit).unwrap();
    let request = Request {
        content_type: Some("application/x-git-upload-pack-request"),
        git_protocol: match data[0] / 5 % 4 {
            1 => Some("version=1"),
            2 => Some("version=2"),
            3 => Some("version=99"),
            _ => None,
        },
        method: ["GET", "POST", "PUT", "DELETE", "HEAD"][usize::from(data[0]) % 5],
        path,
        query: (!query.is_empty()).then_some(query),
        body,
    };
    let capacity = [0, 1, 16, 64, MAX_INPUT][usize::from(data[1]) % 5];
    exercise(
        &repository,
        request,
        capacity,
        1 + usize::from(data[1]) % 31,
    );
}

fn exercise(
    repository: &impl git_zero::Repository,
    request: Request<'_>,
    capacity: usize,
    fragment_size: usize,
) {
    let mut metadata = [None; 2];
    let mut by_id = [0; 2];
    let repository = git_zero::ValidatedRepository::new(
        repository,
        &mut metadata,
        &mut by_id,
        &mut [0; 1],
        &mut [0; 128],
    )
    .unwrap();
    let mut wants = [git_zero::ObjectId::Sha1([0; 20]); 2];
    let mut objects = [None; 2];
    let mut indices = [0; 2];
    let mut selection = http::SelectionStorage {
        wants: &mut wants,
        objects: &mut objects,
        indices: &mut indices,
        read: &mut [0; 1],
        parse: &mut [0; 128],
        graph_read_limit: MAX_INPUT as u64,
    };
    let mut bytes = [0; MAX_INPUT];
    let mut output = SliceWriter::new(&mut bytes[..capacity]);
    if let Ok(response) =
        http::serve_with_selection(&repository, request, &mut selection, &mut output)
    {
        check_response(response, request.git_protocol, output.as_bytes());
    }
    streaming(
        &repository,
        request,
        capacity,
        fragment_size,
        &mut selection,
    );
    prepared(repository, request, capacity, fragment_size);
}

// Exercise the transport-silent cache phase with the same fuzzed routing and
// bounded control budgets. Failed attachment has no response cursor to drain.
fn prepared(
    repository: git_zero::ValidatedRepository<'_, '_, impl git_zero::Repository>,
    request: Request<'_>,
    capacity: usize,
    fragment_size: usize,
) {
    use http::stream::{ResponseEncoder, preparation::PreparedResponse};
    let mut selection = http::SelectionBuffers::<2, 2, 1, 128>::new();
    let mut control = [0; MAX_INPUT];
    let Ok(encoder) = ResponseEncoder::with_selection_storage_policy(
        &repository,
        request,
        selection.storage(MAX_INPUT as u64),
        http::WantOptions::STRICT,
        &mut control[..capacity],
    ) else {
        return;
    };
    let mut ids = [repository.object_format().zero_id(); 2];
    let Ok(mut pending) = PreparedResponse::new(encoder, &repository, &mut ids) else {
        return;
    };
    if pending.key().is_none() {
        check_stream(
            &repository,
            request,
            fragment_size,
            &mut pending.without_pack().unwrap(),
        );
        return;
    }
    let mut pack = [0; MAX_INPUT];
    let mut length = 0;
    loop {
        let end = (length + fragment_size).min(pack.len());
        let Ok(count) = pending.read_pack(&repository, &mut pack[length..end]) else {
            return;
        };
        if count == 0 {
            break;
        }
        length += count;
    }
    pending
        .key()
        .unwrap()
        .verify_pack(&pack[..length], &mut ids, MAX_INPUT)
        .unwrap();
    // Reuse request bytes as a mutation selector without extending fuzz input.
    if request.body.first().is_some_and(|byte| byte & 1 != 0) {
        pack[length - 1] ^= 1;
        assert!(
            pending
                .attach(&pack[..length], &mut ids, MAX_INPUT)
                .is_err()
        );
    } else {
        let mut encoder = pending
            .attach(&pack[..length], &mut ids, MAX_INPUT)
            .unwrap();
        check_stream(&repository, request, fragment_size, &mut encoder);
    }
}

fn streaming(
    repository: &git_zero::ValidatedRepository<'_, '_, impl git_zero::Repository>,
    request: Request<'_>,
    capacity: usize,
    fragment_size: usize,
    selection: &mut http::SelectionStorage<'_>,
) {
    let mut intake = [0; MAX_INPUT];
    let mut incoming =
        http::stream::RequestBody::new(&mut intake, Some(request.body.len())).unwrap();
    for fragment in request.body.chunks(fragment_size) {
        incoming.push(fragment).unwrap();
    }
    let request = Request {
        body: incoming.finish().unwrap(),
        ..request
    };
    for selected in [false, true] {
        let mut control = [0; MAX_INPUT];
        let result = if selected {
            http::stream::ResponseEncoder::with_selection(
                repository,
                request,
                selection,
                &mut control[..capacity],
            )
        } else {
            http::stream::ResponseEncoder::new(repository, request, &mut control[..capacity])
        };
        if let Ok(mut encoder) = result {
            check_stream(repository, request, fragment_size, &mut encoder);
        }
    }
}

fn check_stream(
    repository: &impl git_zero::Repository,
    request: Request<'_>,
    fragment_size: usize,
    encoder: &mut http::stream::ResponseEncoder<'_>,
) {
    let mut bytes = [0; MAX_INPUT];
    let mut staged = SliceWriter::new(&mut bytes);
    drain_response(repository, encoder, fragment_size, &mut staged);
    check_response(encoder.response(), request.git_protocol, staged.as_bytes());
    encoder.cancel();
    assert_eq!(
        encoder.read(repository, &mut [0; 1]),
        Err(git_zero::Error::InvalidRequest)
    );
}

fn drain_response(
    repository: &impl git_zero::Repository,
    encoder: &mut http::stream::ResponseEncoder<'_>,
    fragment_size: usize,
    staged: &mut SliceWriter<'_>,
) {
    for _ in 0..MAX_INPUT * 2 {
        let mut guard = [0xa5; 66];
        let end = 1 + fragment_size;
        let count = encoder.read(repository, &mut guard[1..end]).unwrap();
        assert_eq!(guard[0], 0xa5);
        assert!(guard[end..].iter().all(|&byte| byte == 0xa5));
        if count == 0 {
            break;
        }
        git_zero::io::Write::write_all(staged, &guard[1..1 + count]).unwrap();
    }
    assert_eq!(encoder.read(repository, &mut [0; 1]).unwrap(), 0);
}

fn check_response(response: http::Response, version: Option<&str>, wire: &[u8]) {
    if response.content_type != "application/x-git-upload-pack-result" {
        return;
    }
    match version {
        Some("version=2") => check_v2(wire),
        _ => check_upload(wire),
    }
}

fn check_v2(mut wire: &[u8]) {
    let mut storage = [0; MAX_INPUT];
    let mut packed = SliceWriter::new(&mut storage);
    let mut packfile = false;
    while !wire.is_empty() {
        if wire.starts_with(b"0001") {
            wire = &wire[4..];
            continue;
        }
        let packet = pktline::read(&mut wire).unwrap().unwrap();
        match packet {
            Packet::Data(b"packfile\n") => packfile = true,
            Packet::Data(line) if packfile => {
                assert_eq!(line[0], 1);
                git_zero::io::Write::write_all(&mut packed, &line[1..]).unwrap();
            }
            Packet::Data(_) => {}
            Packet::Flush => {
                assert!(wire.is_empty());
                if packfile {
                    pack::validate(packed.as_bytes()).unwrap();
                }
                return;
            }
        }
    }
    assert!(wire.is_empty() && !packfile);
}

fn check_upload(mut wire: &[u8]) {
    let mut storage = [0; MAX_INPUT];
    let mut packed = SliceWriter::new(&mut storage);
    let mut acknowledged = false;
    while !wire.is_empty() {
        if wire.starts_with(b"PACK") {
            assert!(acknowledged);
            pack::validate(wire).unwrap();
            return;
        }
        match pktline::read(&mut wire).unwrap().unwrap() {
            Packet::Data(line) if line.starts_with(b"ACK ") || line == b"NAK\n" => {
                acknowledged = true;
            }
            Packet::Data(line)
                if !acknowledged
                    && (line.starts_with(b"shallow ") || line.starts_with(b"unshallow ")) => {}
            Packet::Data(line) => {
                assert!(acknowledged);
                assert_eq!(line[0], 1);
                git_zero::io::Write::write_all(&mut packed, &line[1..]).unwrap();
            }
            Packet::Flush => {
                if !acknowledged {
                    if wire.is_empty() {
                        return;
                    }
                    continue;
                }
                assert!(wire.is_empty());
                pack::validate(packed.as_bytes()).unwrap();
                return;
            }
        }
    }
    assert!(acknowledged && packed.is_empty());
}
