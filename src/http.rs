//! Routing for paths relative to the repository URL; HTTP framing belongs to the host.
use crate::{
    Error, ObjectId, Repository, Result,
    io::Write,
    pack,
    pktline::{self, Packet},
};

/// Default HEAD body. The server writes `Repository::head_ref` dynamically.
pub const HEAD: &[u8] = b"ref: refs/heads/main\n";

#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub method: &'a str,
    /// Repository-relative URL path. Escape URL-reserved bytes with `%HH`.
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub body: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
}

/// Always send `Cache-Control: no-store` for discovery and upload-pack responses.
/// A failed call invalidates all output; hosts must discard it before replying.
pub fn serve(
    repository: &impl Repository,
    request: Request<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    let path = request.path.trim_start_matches('/');
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    match (request.method, path) {
        ("GET", "HEAD") => {
            output.write_all(b"ref: ")?;
            output.write_all(repository.head_ref().as_bytes())?;
            output.write_all(b"\n")?;
        }
        ("GET", "info/refs") => match service(request.query) {
            Some("git-upload-pack") => {
                response.content_type = "application/x-git-upload-pack-advertisement";
                pktline::write(output, b"# service=git-upload-pack\n")?;
                output.write_all(b"0000")?;
                let head = repository.head().to_hex();
                pktline::write_parts(
                    output,
                    &[
                        &head,
                        b" HEAD\0symref=HEAD:",
                        repository.head_ref().as_bytes(),
                        b"\n",
                    ],
                )?;
                for index in 0..repository.reference_count() {
                    let name = repository
                        .reference_name(index)
                        .ok_or(Error::InvalidReference)?;
                    pktline::write_parts(output, &[&head, b" ", name.as_bytes(), b"\n"])?;
                }
                output.write_all(b"0000")?;
            }
            None => {
                for index in 0..repository.reference_count() {
                    let name = repository
                        .reference_name(index)
                        .ok_or(Error::InvalidReference)?;
                    output.write_all(&repository.head().to_hex())?;
                    output.write_all(b"\t")?;
                    output.write_all(name.as_bytes())?;
                    output.write_all(b"\n")?;
                }
            }
            _ => response.status = 403,
        },
        ("POST", "git-upload-pack") => {
            response.content_type = "application/x-git-upload-pack-result";
            if upload_request(repository.head(), request.body)? {
                pktline::write(output, b"NAK\n")?;
                pack::write(repository, output)?;
            } else {
                pktline::write(output, b"NAK\n")?;
            }
        }
        ("GET", "objects/info/packs") => {}
        ("GET", _) if path.starts_with("refs/") => {
            if (0..repository.reference_count()).any(|index| {
                repository
                    .reference_name(index)
                    .is_some_and(|name| ref_path_matches(path, name))
            }) {
                output.write_all(&repository.head().to_hex())?;
                output.write_all(b"\n")?;
            } else {
                response.status = 404;
            }
        }
        ("GET", _) if path.starts_with("objects/") => {
            let object_path = &path.as_bytes()[8..];
            if object_path.len() != 41 || object_path[2] != b'/' {
                response.status = 404;
            } else {
                let mut hex = [0; 40];
                hex[..2].copy_from_slice(&object_path[..2]);
                hex[2..].copy_from_slice(&object_path[3..]);
                if let Ok(id) = ObjectId::from_hex(&hex) {
                    if let Some(object) = repository.find(id) {
                        response.content_type = "application/x-git-loose-object";
                        pack::write_loose(object, output)?;
                    } else {
                        response.status = 404;
                    }
                } else {
                    response.status = 404;
                }
            }
        }
        ("GET" | "POST", _) => response.status = 404,
        _ => response.status = 405,
    }
    Ok(response)
}

fn ref_path_matches(path: &str, name: &str) -> bool {
    let mut bytes = path.bytes();
    for expected in name.bytes() {
        let Some(mut byte) = bytes.next() else {
            return false;
        };
        if byte == b'%' {
            let Some(high) = bytes.next().and_then(|byte| (byte as char).to_digit(16)) else {
                return false;
            };
            let Some(low) = bytes.next().and_then(|byte| (byte as char).to_digit(16)) else {
                return false;
            };
            byte = ((high << 4) | low) as u8;
        }
        if byte != expected {
            return false;
        }
    }
    bytes.next().is_none()
}

fn service(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|field| field.strip_prefix("service="))
}

fn upload_request(head: ObjectId, mut bytes: &[u8]) -> Result<bool> {
    let mut wanted = false;
    let mut done = false;
    while let Some(packet) = pktline::read(&mut bytes)? {
        let Packet::Data(line) = packet else {
            continue;
        };
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        if done {
            return Err(Error::InvalidRequest);
        }
        if let Some(want) = line.strip_prefix(b"want ") {
            if want.len() != 40 || ObjectId::from_hex(want)? != head {
                return Err(Error::InvalidRequest);
            }
            wanted = true;
        } else if let Some(have) = line.strip_prefix(b"have ") {
            ObjectId::from_hex(have)?;
        } else if line == b"done" {
            done = true;
        } else {
            return Err(Error::Unsupported);
        }
    }
    if !wanted {
        return Err(Error::InvalidRequest);
    }
    Ok(done)
}
