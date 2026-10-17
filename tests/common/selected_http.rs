#[path = "http_request.rs"]
mod http_request;

use git_zero::{ObjectFormat, ObjectId};
pub use http_request::upload as request;

pub fn packet(bytes: &mut Vec<u8>, line: &[u8]) {
    bytes.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    bytes.extend_from_slice(line);
}

pub fn body(
    format: ObjectFormat,
    version: u8,
    wants: &[ObjectId],
    haves: &[ObjectId],
    extra: &[&str],
    done: bool,
    band: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    if version == 2 {
        packet(&mut bytes, b"command=fetch\n");
        packet(
            &mut bytes,
            format!("{}\n", std::str::from_utf8(format.capability()).unwrap()).as_bytes(),
        );
        bytes.extend_from_slice(b"0001");
    }
    for (index, id) in wants.iter().enumerate() {
        let caps = if version != 2 && index == 0 {
            format!(
                " {}{band}",
                std::str::from_utf8(format.capability()).unwrap()
            )
        } else {
            String::new()
        };
        packet(&mut bytes, format!("want {id}{caps}\n").as_bytes());
    }
    for line in extra {
        packet(&mut bytes, format!("{line}\n").as_bytes());
    }
    if version != 2 {
        bytes.extend_from_slice(b"0000");
    }
    for id in haves {
        packet(&mut bytes, format!("have {id}\n").as_bytes());
    }
    if done {
        packet(&mut bytes, b"done\n");
    }
    if version == 2 || !done && !haves.is_empty() {
        bytes.extend_from_slice(b"0000");
    }
    bytes
}

/// Serialize the same protocol request through the native HTTP boundary.
#[allow(dead_code)]
pub fn wire(body: &[u8], version: u8) -> Vec<u8> {
    let request = request(body, version);
    let mut wire = format!(
        "POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: {}\r\nContent-Type: {}\r\n",
        body.len(),
        request.content_type.unwrap()
    );
    if let Some(protocol) = request.git_protocol {
        wire.push_str(&format!("Git-Protocol: {protocol}\r\n"));
    }
    wire.push_str("\r\n");
    let mut wire = wire.into_bytes();
    wire.extend_from_slice(body);
    wire
}
