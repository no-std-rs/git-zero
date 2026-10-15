use super::MAX_INPUT;
use git_zero::{
    Snapshot,
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
    let mut storage = [0; MAX_INPUT];
    let mut output = SliceWriter::new(&mut storage[..capacity]);
    if let Ok(response) = http::serve_with_scratch(&repository, request, &mut [0; 1], &mut output) {
        assert!(matches!(response.status, 200 | 403 | 404 | 405 | 415));
        assert!(output.len() <= capacity);
        if response.content_type == "application/x-git-upload-pack-result" {
            if request.git_protocol == Some("version=2") {
                check_v2(output.as_bytes());
            } else {
                check_upload(output.as_bytes());
            }
        }
    }
    // Replay intake with arbitrary fragmentation, then drive an independent
    // cursor through guarded output slices and terminal cancellation.
    let mut intake = [0; MAX_INPUT];
    let mut incoming = http::stream::RequestBody::new(&mut intake, Some(body.len())).unwrap();
    for fragment in body.chunks(1 + usize::from(data[1]) % 31) {
        incoming.push(fragment).unwrap();
    }
    let body = incoming.finish().unwrap();
    let mut control = [0; MAX_INPUT];
    let request = Request { body, ..request };
    if let Ok(mut encoder) =
        http::stream::ResponseEncoder::new(&repository, request, &mut control[..capacity])
    {
        let response = encoder.response();
        let mut bytes = [0; MAX_INPUT];
        let mut staged = SliceWriter::new(&mut bytes);
        for _ in 0..MAX_INPUT * 2 {
            let mut guard = [0xa5; 66];
            let end = 2 + usize::from(data[1]) % 64;
            let count = encoder.read(&repository, &mut guard[1..end]).unwrap();
            assert_eq!(guard[0], 0xa5);
            assert!(guard[end..].iter().all(|&byte| byte == 0xa5));
            if count == 0 {
                break;
            }
            git_zero::io::Write::write_all(&mut staged, &guard[1..1 + count]).unwrap();
        }
        assert_eq!(encoder.read(&repository, &mut [0; 1]).unwrap(), 0);
        if response.content_type == "application/x-git-upload-pack-result" {
            if request.git_protocol == Some("version=2") {
                check_v2(staged.as_bytes());
            } else {
                check_upload(staged.as_bytes());
            }
        }
        encoder.cancel();
        assert_eq!(
            encoder.read(&repository, &mut [0; 1]),
            Err(git_zero::Error::InvalidRequest)
        );
    }
}

fn check_v2(mut wire: &[u8]) {
    let mut storage = [0; MAX_INPUT];
    let mut packed = SliceWriter::new(&mut storage);
    let mut packfile = false;
    while let Some(packet) = pktline::read(&mut wire).unwrap() {
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
            Packet::Data(line) => {
                assert!(acknowledged);
                assert_eq!(line[0], 1);
                git_zero::io::Write::write_all(&mut packed, &line[1..]).unwrap();
            }
            Packet::Flush => {
                assert!(wire.is_empty());
                pack::validate(packed.as_bytes()).unwrap();
                return;
            }
        }
    }
    assert!(acknowledged && packed.is_empty());
}
