//! Smart/dumb HTTP dispatch, raw refs and loose objects, analogous to Git's http-backend.
//! The host owns HTTP transfer framing, authentication and transport I/O.
use crate::{
    Request, Response, route,
    upload_pack::{self, PackOutput, advertisement},
    v2,
};
use git_zero_core::{
    Error, ObjectId, Repository, Result, io::Write, pack, reader::DEFAULT_READ_BUFFER_SIZE,
};

/// Default HEAD body. The server writes `Repository::head_state` dynamically.
pub const HEAD: &[u8] = b"ref: refs/heads/main\n";

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
    match prepare(repository, request, output, false)? {
        Plan::Buffered(response) => Ok(response),
        Plan::Loose(index) => write_loose(repository, index, scratch, output),
        Plan::Upload(version) => {
            upload_pack::write_upload(repository, request, version, scratch, output, write_pack)?;
            Ok(upload_pack::response())
        }
    }
}

pub(crate) fn write_loose(
    repository: &impl Repository,
    index: usize,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<Response> {
    pack::write_loose_with_scratch(repository, index, scratch, output)?;
    Ok(Response {
        status: 200,
        content_type: "application/x-git-loose-object",
    })
}

/// A validated HTTP route, shared by buffered output and pull-based encoders.
pub(crate) enum Plan {
    Buffered(Response),
    Loose(usize),
    Upload(route::Version),
}

pub(crate) fn prepare(
    repository: &impl Repository,
    request: Request<'_>,
    output: &mut impl Write,
    selection: bool,
) -> Result<Plan> {
    prepare_with_refs(repository, request, output, |route, output| {
        advertisement::serve_info_refs(repository, route, output, selection)
    })
}

/// The caller chooses discovery capabilities; all HTTP routing stays shared.
pub(crate) fn prepare_with_refs<W: Write>(
    repository: &impl Repository,
    request: Request<'_>,
    output: &mut W,
    advertise: impl FnOnce(&route::Route<'_>, &mut W) -> Result<Response>,
) -> Result<Plan> {
    let route = route::validate(request)?;
    if let Some(response) = route.rejection {
        return Ok(Plan::Buffered(response));
    }
    let status = match (request.method, route.path) {
        ("GET", "HEAD") => {
            write_head(repository, output)?;
            200
        }
        ("GET", "info/refs") => {
            return advertise(&route, output).map(Plan::Buffered);
        }
        ("GET", "objects/info/packs") => 200,
        ("GET", path) if path.starts_with("refs/") => serve_reference(repository, path, output)?,
        ("GET", path) if path.starts_with("objects/") => {
            if let Some(index) = loose_index(repository, &path.as_bytes()[8..]) {
                return Ok(Plan::Loose(index));
            }
            404
        }
        ("POST", "git-upload-pack") => {
            if route.version == route::Version::V2
                && v2::serve_refs(repository, request.body, output)?
            {
                return Ok(Plan::Buffered(upload_pack::response()));
            }
            return Ok(Plan::Upload(route.version));
        }
        ("GET" | "POST", _) => 404,
        _ => 405,
    };
    Ok(Plan::Buffered(Response {
        status,
        content_type: "text/plain; charset=utf-8",
    }))
}

fn write_head(repository: &impl Repository, output: &mut impl Write) -> Result<()> {
    if let Some(target) = repository.head_state().target() {
        output.write_all(b"ref: ")?;
        output.write_all(target.as_bytes())?;
    } else {
        output.write_all(&repository.head().to_hex())?;
    }
    output.write_all(b"\n")
}

fn serve_reference(
    repository: &impl Repository,
    path: &str,
    output: &mut impl Write,
) -> Result<u16> {
    let index = (0..repository.reference_count()).find(|&index| {
        repository
            .reference_name(index)
            .is_some_and(|name| route::decoded(path).eq(name.bytes().map(Ok)))
    });
    let Some(index) = index else { return Ok(404) };
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
    Ok(200)
}

fn loose_index(repository: &impl Repository, path: &[u8]) -> Option<usize> {
    let width = repository.object_format().hex_length();
    if path.len() != width + 1 || path[2] != b'/' {
        return None;
    }
    let mut storage = [0; 64];
    let hex = &mut storage[..width];
    hex[..2].copy_from_slice(&path[..2]);
    hex[2..].copy_from_slice(&path[3..]);
    ObjectId::from_hex(hex)
        .ok()
        .and_then(|id| repository.find_index(id))
}
