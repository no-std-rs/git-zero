//! Routing for paths relative to the repository URL; HTTP framing belongs to the host.
use crate::{
    Error, Head, ObjectId, Repository, Result,
    io::Write,
    pack,
    pktline::{self, Packet},
    reader::DEFAULT_READ_BUFFER_SIZE,
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
/// Reserves 1 KiB of read scratch on the stack for streaming-only providers.
pub fn serve(
    repository: &impl Repository,
    request: Request<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    serve_with_scratch(
        repository,
        request,
        &mut [0; DEFAULT_READ_BUFFER_SIZE],
        output,
    )
}

/// Serve generated/file-backed objects with an explicit read-buffer budget.
/// Discovery/ref responses and borrowed bodies need no scratch; nonempty
/// streamed bodies need at least one byte. Errors invalidate all output.
pub fn serve_with_scratch(
    repository: &impl Repository,
    request: Request<'_>,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<Response> {
    serve_with_pack(repository, request, scratch, output, |scratch, output| {
        pack::write_with_scratch(repository, scratch, output).map(|_| ())
    })
}

/// Serve with a host-provided complete-pack writer. It is called once only for
/// a valid upload-pack request ending in `done`, after writing the NAK packet.
/// The writer must emit a complete checksummed pack containing exactly this
/// repository's objects, and respect the repository's stability contract.
/// Other routes and no-done requests use the ordinary core behavior. On any
/// error discard the entire partial response, including the NAK.
pub fn serve_with_pack<W: Write>(
    repository: &impl Repository,
    request: Request<'_>,
    scratch: &mut [u8],
    output: &mut W,
    write_pack: impl FnOnce(&mut [u8], &mut W) -> Result<()>,
) -> Result<Response> {
    let path = request.path.trim_start_matches('/');
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    match (request.method, path) {
        ("GET", "HEAD") => {
            if let Some(target) = repository.head_state().target() {
                output.write_all(b"ref: ")?;
                output.write_all(target.as_bytes())?;
            } else {
                output.write_all(&repository.head().to_hex())?;
            }
            output.write_all(b"\n")?;
        }
        ("GET", "info/refs") => match service(request.query) {
            Some("git-upload-pack") => {
                response.content_type = "application/x-git-upload-pack-advertisement";
                pktline::write(output, b"# service=git-upload-pack\n")?;
                output.write_all(b"0000")?;
                advertise_head(repository.head_state(), output)?;
                let head = repository.head().to_hex();
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
            if upload_request(repository.head_state().id(), request.body)? {
                pktline::write(output, b"NAK\n")?;
                write_pack(scratch, output)?;
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
                    if let Some(index) = repository.find_index(id) {
                        response.content_type = "application/x-git-loose-object";
                        pack::write_loose_with_scratch(repository, index, scratch, output)?;
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

fn advertise_head(head: Head<'_>, output: &mut impl Write) -> Result<()> {
    let id = head.id().unwrap_or(ObjectId([0; 20])).to_hex();
    let name = if head.id().is_some() {
        b" HEAD\0".as_slice()
    } else {
        b" capabilities^{}\0"
    };
    if let Some(target) = head.target() {
        pktline::write_parts(
            output,
            &[&id, name, b"symref=HEAD:", target.as_bytes(), b"\n"],
        )
    } else {
        pktline::write_parts(output, &[&id, name, b"\n"])
    }
}

fn upload_request(head: Option<ObjectId>, mut bytes: &[u8]) -> Result<bool> {
    let head = head.ok_or(Error::InvalidRequest)?;
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
