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
mod fetch;
mod route;
pub mod stream;
mod upload;
mod v2;
#[cfg(feature = "read")]
use git_zero_core::{SelectionOptions, ValidatedRepository, ValidationMetadata};
use route::Version;
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

#[cfg(feature = "read")]
impl SelectionStorage<'_> {
    /// Borrow the same arrays for one request without moving the host descriptor.
    pub fn reborrow(&mut self) -> SelectionStorage<'_> {
        SelectionStorage {
            wants: self.wants,
            objects: self.objects,
            indices: self.indices,
            read: self.read,
            parse: self.parse,
            graph_read_limit: self.graph_read_limit,
        }
    }
}

/// Serve independent refs, tags and multiple wanted IDs with caller-owned
/// selection storage. Accept only advertised ref/peeled/HEAD IDs, rejecting
/// arbitrary or merely reachable wants. Select the exact union before writing
/// ACKs or pack. Present commit haves exclude their complete reachable history;
/// unknown and non-commit haves are ignored. Requests are stateless: clients
/// replay wants and acknowledged haves each round. No-done rounds only negotiate.
/// Absolute-depth shallow requests and blob filters use the same graph budget.
/// Shallow haves never imply complete history. Errors
/// invalidate all output; selection failures produce no output at all.
#[cfg(feature = "read")]
pub fn serve_with_selection<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    let route = route::validate(request)?;
    if let Some(response) = route.rejection {
        return Ok(response);
    }
    if request.method != "POST" || route.path != "git-upload-pack" {
        if request.method == "GET" && route.path == "info/refs" {
            return serve_info_refs(repository, &route, output, true);
        }
        return serve_with_scratch(repository, request, storage.read, output);
    }
    let version = route.version;
    if version == Version::V2 && v2::serve_refs(repository, request.body, output)? {
        return Ok(upload_response());
    }
    let selected = prepare_fetch(repository, request, version, storage.reborrow(), output)?;
    if let Some(indices) = selected.indices {
        let mut packed = selected.upload.output(output);
        pack::write_selected_with_scratch(repository, indices, selected.read, &mut packed)?;
        packed.finish()?;
    }
    Ok(upload_response())
}

#[cfg(feature = "read")]
struct SelectedFetch<'storage, 'body> {
    upload: UploadRequest<'body>,
    indices: Option<&'storage [usize]>,
    read: &'storage mut [u8],
}

#[cfg(feature = "read")]
fn prepare_fetch<'storage, 'body, R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'body>,
    version: Version,
    storage: SelectionStorage<'storage>,
    output: &mut impl Write,
) -> Result<SelectedFetch<'storage, 'body>> {
    let (upload, count) =
        UploadRequest::parse_selected(repository, request.body, version, storage.wants)?;
    let indices = if upload.done || upload.extensions.active() {
        Some(repository.select_bounded_fetch(
            &storage.wants[..count],
            (upload.common(repository), upload.extensions.shallow()),
            storage.objects,
            storage.indices,
            (storage.read, storage.parse),
            (
                SelectionOptions {
                    graph_read_limit: storage.graph_read_limit,
                    include_tags: upload.include_tags(),
                },
                upload.extensions.options,
            ),
        )?)
    } else {
        None
    };
    if upload.extensions.options.depth.is_some() {
        upload.shallow_info(&storage.objects[..repository.object_count()], output)?;
    }
    upload.acknowledge(repository, output)?;
    Ok(SelectedFetch {
        indices: indices.filter(|_| upload.done),
        upload,
        read: storage.read,
    })
}

#[cfg(feature = "read")]
fn upload_response() -> Response {
    Response {
        status: 200,
        content_type: "application/x-git-upload-pack-result",
    }
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
    let route = route::validate(request)?;
    if let Some(response) = route.rejection {
        return Ok(response);
    }
    if request.method == "GET" {
        return serve_get(repository, &route, scratch, output);
    }
    if request.method == "POST" && route.path == "git-upload-pack" {
        write_upload(
            repository,
            request,
            route.version,
            scratch,
            output,
            write_pack,
        )?;
        return Ok(Response {
            status: 200,
            content_type: "application/x-git-upload-pack-result",
        });
    }
    Ok(Response {
        status: if request.method == "POST" { 404 } else { 405 },
        content_type: "text/plain; charset=utf-8",
    })
}

fn serve_get(
    repository: &impl Repository,
    route: &route::Route<'_>,
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<Response> {
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    match route.path {
        "HEAD" => write_head(repository, output)?,
        "info/refs" => return serve_info_refs(repository, route, output, false),
        "objects/info/packs" => {}
        path if path.starts_with("refs/") => {
            response.status = serve_reference(repository, path, output)?
        }
        path if path.starts_with("objects/") => {
            return serve_loose(repository, &path.as_bytes()[8..], scratch, output);
        }
        _ => response.status = 404,
    }
    Ok(response)
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

fn serve_info_refs(
    repository: &impl Repository,
    route: &route::Route<'_>,
    output: &mut impl Write,
    selection: bool,
) -> Result<Response> {
    let mut response = Response {
        status: 200,
        content_type: "text/plain; charset=utf-8",
    };
    match route.service {
        Some("git-upload-pack") => {
            response.content_type = "application/x-git-upload-pack-advertisement";
            advertise_smart(repository, route.version, output, selection)?;
        }
        None => advertise_references(repository, false, output, upload::capabilities(selection))?,
        _ => response.status = 403,
    }
    Ok(response)
}

fn advertise_smart(
    repository: &impl Repository,
    version: Version,
    output: &mut impl Write,
    selection: bool,
) -> Result<()> {
    pktline::write(output, b"# service=git-upload-pack\n")?;
    output.write_all(b"0000")?;
    if version == Version::V2 {
        return v2::advertise(repository.object_format(), output, selection);
    }
    if version == Version::V1 {
        pktline::write(output, b"version 1\n")?;
    }
    if repository.head_state().id().is_some() || repository.reference_count() == 0 {
        advertise_head(repository, output, upload::capabilities(selection))?;
    }
    advertise_references(repository, true, output, upload::capabilities(selection))?;
    output.write_all(b"0000")
}

fn write_upload<W: Write>(
    repository: &impl Repository,
    request: Request<'_>,
    version: Version,
    scratch: &mut [u8],
    output: &mut W,
    write_pack: impl FnOnce(&mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
) -> Result<()> {
    if version == Version::V2 && v2::serve_refs(repository, request.body, output)? {
        return Ok(());
    }
    if !uses_only_head(repository) {
        return Err(Error::Unsupported);
    }
    let head = repository.head_state().id().ok_or(Error::InvalidRequest)?;
    let upload = UploadRequest::parse(request.body, repository.object_format(), version, |id| {
        if id == head {
            Ok(())
        } else {
            Err(Error::InvalidRequest)
        }
    })?;
    upload.require_complete()?;
    upload.acknowledge(repository, output)?;
    if upload.done {
        let mut packed = upload.output(output);
        write_pack(scratch, &mut packed)?;
        packed.finish()?;
    }
    Ok(())
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

fn serve_loose(
    repository: &impl Repository,
    path: &[u8],
    scratch: &mut [u8],
    output: &mut impl Write,
) -> Result<Response> {
    let missing = Response {
        status: 404,
        content_type: "text/plain; charset=utf-8",
    };
    let Some(index) = loose_index(repository, path) else {
        return Ok(missing);
    };
    pack::write_loose_with_scratch(repository, index, scratch, output)?;
    Ok(Response {
        status: 200,
        content_type: "application/x-git-loose-object",
    })
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

fn advertise_head(
    repository: &impl Repository,
    output: &mut impl Write,
    capabilities: &[u8],
) -> Result<()> {
    let head = repository.head_state();
    let format = repository.object_format();
    let id = head.id().unwrap_or(format.zero_id()).to_hex();
    let name = if head.id().is_some() {
        b" HEAD\0".as_slice()
    } else {
        b" capabilities^{}\0"
    };
    let target = match head {
        Head::Symbolic { target, .. } => Some(resolve_reference(repository, target)?.0),
        Head::Unborn(target) => Some(target),
        Head::Detached(_) => None,
    };
    let (symref, separator): (&[u8], &[u8]) = match target {
        Some(_) => (b"symref=HEAD:", b" "),
        None => (b"", b""),
    };
    pktline::write_parts(
        output,
        &[
            &id,
            name,
            symref,
            target.unwrap_or("").as_bytes(),
            separator,
            format.capability(),
            capabilities,
            b"\n",
        ],
    )
}

fn advertise_references(
    repository: &impl Repository,
    smart: bool,
    output: &mut impl Write,
    capabilities_bytes: &[u8],
) -> Result<()> {
    for index in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
        let capabilities = smart && index == 0 && repository.head_state().id().is_none();
        reference_line(
            id,
            name,
            b"",
            smart,
            if capabilities {
                capabilities_bytes
            } else {
                b""
            },
            output,
        )?;
        if let Some(peeled) = repository.reference_peeled(index) {
            reference_line(peeled, name, b"^{}", smart, b"", output)?;
        }
    }
    Ok(())
}

fn reference_line(
    id: ObjectId,
    name: &str,
    suffix: &[u8],
    smart: bool,
    capabilities: &[u8],
    output: &mut impl Write,
) -> Result<()> {
    let format = id.format();
    let id = id.to_hex();
    if smart {
        let parts: [&[u8]; 3] = if !capabilities.is_empty() {
            [b"\0", format.capability(), capabilities]
        } else {
            [b"", b"", b""]
        };
        pktline::write_parts(
            output,
            &[
                &id,
                b" ",
                name.as_bytes(),
                suffix,
                parts[0],
                parts[1],
                parts[2],
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
