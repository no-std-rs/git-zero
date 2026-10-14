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
        method: ["GET", "POST", "PUT", "DELETE", "HEAD"][usize::from(data[0]) % 5],
        path,
        query: (!query.is_empty()).then_some(query),
        body,
    };
    let capacity = [0, 1, 16, 64, MAX_INPUT][usize::from(data[1]) % 5];
    let mut storage = [0; MAX_INPUT];
    let mut output = SliceWriter::new(&mut storage[..capacity]);
    if let Ok(response) = http::serve_with_scratch(&repository, request, &mut [0; 1], &mut output) {
        assert!(matches!(response.status, 200 | 403 | 404 | 405));
        assert!(output.len() <= capacity);
        if response.content_type == "application/x-git-upload-pack-result" {
            check_upload(output.as_bytes());
        }
    }
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
