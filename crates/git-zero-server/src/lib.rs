//! Routing for paths relative to the repository URL; HTTP framing belongs to the host.
#![no_std]
#![forbid(unsafe_code)]

use git_zero_core::{
    Error, Head, ObjectId, Repository, Result,
    io::Write,
    pack, pktline,
    reader::DEFAULT_READ_BUFFER_SIZE,
    repository::{resolve_reference, uses_only_head},
};
mod upload;
#[cfg(feature = "read")]
use git_zero_core::{ValidatedRepository, ValidationMetadata};
use upload::UploadRequest;
pub use upload::{PackOutput, write_upload_error};

/// Default HEAD body. The server writes `Repository::head_state` dynamically.
pub const HEAD: &[u8] = b"ref: refs/heads/main\n";

pub use git_zero_core::http::{Request, Response};

/// Independent per-request storage for bounded reachable-object selection.
/// `wants` bounds distinct wanted IDs. `objects` and `indices` each need the
/// repository's complete object count; their extra slots remain untouched.
/// Read/parse scratch budgets match `ValidatedRepository::select_objects`.
#[cfg(feature = "read")]
pub struct SelectionStorage<'a> {
    pub wants: &'a mut [ObjectId],
    pub objects: &'a mut [Option<ValidationMetadata>],
    pub indices: &'a mut [usize],
    pub read: &'a mut [u8],
    pub parse: &'a mut [u8],
    /// Maximum structural bytes read while traversing a request's graph.
    pub graph_read_limit: u64,
}

/// Serve independent refs, tags and multiple wanted IDs with caller-owned
/// selection storage. Accept only advertised ref/peeled/HEAD IDs, rejecting
/// arbitrary or merely reachable wants. Select the exact union before writing
/// ACKs or pack. Present commit haves exclude their complete reachable history;
/// unknown and non-commit haves are ignored. Requests are stateless: clients
/// replay wants and acknowledged haves each round. No-done rounds only negotiate.
/// Shallow/filter requests are rejected before excluding any history. Errors
/// invalidate all output; selection failures produce no output at all.
#[cfg(feature = "read")]
pub fn serve_with_selection<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    if request.method != "POST" || request.path.trim_start_matches('/') != "git-upload-pack" {
        return serve_with_scratch(repository, request, storage.read, output);
    }
    let mut count = 0;
    let upload = UploadRequest::parse(request.body, repository.object_format(), |id| {
        if !advertised(repository, id) {
            return Err(Error::InvalidRequest);
        }
        if !storage.wants[..count].contains(&id) {
            *storage.wants.get_mut(count).ok_or(Error::BufferTooSmall)? = id;
            count += 1;
        }
        Ok(())
    })?;
    if upload.done {
        let indices = repository.select_missing_objects(
            &storage.wants[..count],
            upload.common(repository),
            storage.objects,
            storage.indices,
            (storage.read, storage.parse),
            storage.graph_read_limit,
        )?;
        upload.acknowledge(repository, output)?;
        let mut packed = upload.output(output);
        pack::write_selected_with_scratch(repository, indices, storage.read, &mut packed)?;
        packed.finish()?;
    } else {
        upload.acknowledge(repository, output)?;
    }
    Ok(Response {
        status: 200,
        content_type: "application/x-git-upload-pack-result",
    })
}

#[cfg(feature = "read")]
fn advertised(repository: &impl Repository, id: ObjectId) -> bool {
    repository.head_state().id() == Some(id)
        || (0..repository.reference_count()).any(|index| {
            repository.reference_id(index) == Some(id)
                || repository.reference_peeled(index) == Some(id)
        })
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
/// a valid upload-pack request ending in `done`, after writing ACK/NAK packets.
/// The writer must emit a complete checksummed pack containing exactly this
/// repository's objects, and respect the repository's stability contract.
/// All refs must alias HEAD; independent roots return `Unsupported` before
/// output or invoking the writer. Use `serve_with_selection` for those roots.
/// The supplied sink handles negotiated sideband framing. Complete-pack hooks
/// may resend common history; use selection to omit it. Other routes and no-done
/// requests use the ordinary behavior. On error discard the entire response.
pub fn serve_with_pack<W: Write>(
    repository: &impl Repository,
    request: Request<'_>,
    scratch: &mut [u8],
    output: &mut W,
    write_pack: impl FnOnce(&mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
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
                let head = repository.head_state();
                if head.id().is_some() || repository.reference_count() == 0 {
                    advertise_head(repository, output)?;
                }
                advertise_references(repository, true, output)?;
                output.write_all(b"0000")?;
            }
            None => {
                advertise_references(repository, false, output)?;
            }
            _ => response.status = 403,
        },
        ("POST", "git-upload-pack") => {
            response.content_type = "application/x-git-upload-pack-result";
            if !uses_only_head(repository) {
                return Err(Error::Unsupported);
            }
            let head = repository.head_state().id().ok_or(Error::InvalidRequest)?;
            let upload = UploadRequest::parse(request.body, repository.object_format(), |id| {
                if id == head {
                    Ok(())
                } else {
                    Err(Error::InvalidRequest)
                }
            })?;
            upload.acknowledge(repository, output)?;
            if upload.done {
                let mut packed = upload.output(output);
                write_pack(scratch, &mut packed)?;
                packed.finish()?;
            }
        }
        ("GET", "objects/info/packs") => {}
        ("GET", _) if path.starts_with("refs/") => {
            if let Some(index) = (0..repository.reference_count()).find(|&index| {
                repository
                    .reference_name(index)
                    .is_some_and(|name| ref_path_matches(path, name))
            }) {
                if let Some(target) = repository.reference_symbolic_target(index) {
                    output.write_all(b"ref: ")?;
                    output.write_all(target.as_bytes())?;
                } else {
                    output.write_all(
                        &repository
                            .reference_id(index)
                            .ok_or(Error::InvalidObject)?
                            .to_hex(),
                    )?;
                }
                output.write_all(b"\n")?;
            } else {
                response.status = 404;
            }
        }
        ("GET", _) if path.starts_with("objects/") => {
            let object_path = &path.as_bytes()[8..];
            let format = repository.object_format();
            if object_path.len() != format.hex_length() + 1 || object_path[2] != b'/' {
                response.status = 404;
            } else {
                let mut storage = [0; 64];
                let hex = &mut storage[..format.hex_length()];
                hex[..2].copy_from_slice(&object_path[..2]);
                hex[2..].copy_from_slice(&object_path[3..]);
                if let Ok(id) = ObjectId::from_hex(hex) {
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

fn advertise_head(repository: &impl Repository, output: &mut impl Write) -> Result<()> {
    let head = repository.head_state();
    let format = repository.object_format();
    let id = head.id().unwrap_or(format.zero_id()).to_hex();
    let name = if head.id().is_some() {
        b" HEAD\0".as_slice()
    } else {
        b" capabilities^{}\0"
    };
    if let Some(target) = head.target() {
        let target = if matches!(head, Head::Symbolic { .. }) {
            resolve_reference(repository, target)?.0
        } else {
            target
        };
        pktline::write_parts(
            output,
            &[
                &id,
                name,
                b"symref=HEAD:",
                target.as_bytes(),
                b" ",
                format.capability(),
                upload::CAPABILITIES,
                b"\n",
            ],
        )
    } else {
        pktline::write_parts(
            output,
            &[&id, name, format.capability(), upload::CAPABILITIES, b"\n"],
        )
    }
}

fn advertise_references(
    repository: &impl Repository,
    smart: bool,
    output: &mut impl Write,
) -> Result<()> {
    for index in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
        let capabilities = smart && index == 0 && repository.head_state().id().is_none();
        reference_line(id, name, b"", smart, capabilities, output)?;
        if let Some(peeled) = repository.reference_peeled(index) {
            reference_line(peeled, name, b"^{}", smart, false, output)?;
        }
    }
    Ok(())
}

fn reference_line(
    id: ObjectId,
    name: &str,
    suffix: &[u8],
    smart: bool,
    capabilities: bool,
    output: &mut impl Write,
) -> Result<()> {
    let format = id.format();
    let id = id.to_hex();
    if smart {
        pktline::write_parts(
            output,
            &[
                &id,
                b" ",
                name.as_bytes(),
                suffix,
                if capabilities { b"\0" } else { b"" },
                if capabilities {
                    format.capability()
                } else {
                    b""
                },
                if capabilities {
                    upload::CAPABILITIES
                } else {
                    b""
                },
                b"\n",
            ],
        )
    } else {
        output.write_all(&id)?;
        output.write_all(b"\t")?;
        output.write_all(name.as_bytes())?;
        output.write_all(suffix)?;
        output.write_all(b"\n")
    }
}
