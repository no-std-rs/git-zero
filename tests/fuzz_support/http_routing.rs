use super::MAX_INPUT;
use git_zero::{
    Snapshot,
    http::{self, Request},
    io::SliceWriter,
    pack,
};

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
            assert!(output.as_bytes().starts_with(b"0008NAK\n"));
            if output.len() > 8 {
                pack::validate(&output.as_bytes()[8..]).unwrap();
            }
        }
    }
}
