//! Upload-pack: authorize wants, negotiate history and emit a selected pack.
//! HTTP dispatch lives in `http_backend`; pack encoding lives in git-zero-core.
#[cfg(feature = "read")]
use crate::http_backend;
use crate::{Request, Response, route::Version};
use git_zero_core::{Error, Repository, Result, io::Write, repository::uses_only_head};
#[cfg(feature = "read")]
use git_zero_core::{ObjectId, SelectionOptions, ValidatedRepository, pack};

pub(crate) mod advertisement;
pub(crate) mod extensions;
pub(crate) mod negotiation;
pub(crate) mod policy;
#[cfg(feature = "read")]
mod storage;

#[cfg(feature = "read")]
pub use negotiation::{Acknowledgments, ShallowUpdates};
pub use negotiation::{PackOutput, write_upload_error};
pub(crate) use negotiation::{UploadRequest, capabilities, id};
pub use policy::{WantOptions, WantPolicy};
#[cfg(feature = "read")]
pub use storage::{RevisionBuffers, RevisionStorage, SelectionBuffers, SelectionStorage};

/// Serve independent refs, tags and multiple wanted IDs with caller-owned
/// selection storage. Accept only advertised ref/peeled/HEAD IDs, rejecting
/// arbitrary or merely reachable wants. Select the exact union before writing
/// ACKs or pack. Present commit haves exclude their complete reachable history;
/// unknown and non-commit haves are ignored. Requests are stateless: clients
/// replay wants and acknowledged haves each round. No-done rounds only negotiate.
/// Absolute/relative-depth shallow requests and blob filters use the same graph budget.
/// Shallow haves never imply complete history. Errors
/// invalidate all output; selection failures produce no output at all.
///
/// Requests must contain a complete, bounded body and the upload-pack media type.
/// Absolute `deepen` requests may have an initial v0 shallow-list probe without
/// `done`; that probe consumes the same graph budget as the eventual pack.
/// Relative probes from full clients only flush an empty boundary list and need
/// no graph work slots, read/parse scratch or structural-read budget.
/// A client must replay its shallow IDs and negotiation state on each request.
/// ACK-only rounds validate client boundaries through the repository index and
/// need no graph work slots, read/parse scratch or structural-read budget.
/// Boundary output must fit the host's response/control budget. The streaming
/// encoder prepares these packets before exposing HTTP success metadata.
///
/// Blob filters preserve explicitly wanted blobs but do not grant permission
/// to request unadvertised promised objects. Partial clones currently need
/// `--no-checkout`. Time/exclusion deepening and other filter families
/// return `Unsupported`; malformed options and invalid shallow IDs return
/// `InvalidRequest`. Scratch/slot exhaustion returns `BufferTooSmall`, and graph
/// read exhaustion returns `LimitExceeded`. A buffered host can replace a failed
/// response with `write_upload_error`; a failed stream must be cancelled/closed.
#[cfg(feature = "read")]
pub fn serve_with_selection<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    serve_with_selection_policy(repository, request, storage, WantOptions::STRICT, output)
}

/// Serve selected wants under an explicit host grant and occurrence cap.
/// The validated repository must be the host-authorized immutable projection.
/// All wants are checked before ACKs, pack output or host cache integration.
#[cfg(feature = "read")]
pub fn serve_with_selection_policy<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    options: WantOptions,
    output: &mut impl Write,
) -> Result<Response> {
    serve_with_selected_pack_policy(
        repository,
        request,
        storage,
        options,
        output,
        |indices, read, packed| {
            pack::write_selected_with_scratch(repository, indices, read, packed).map(|_| ())
        },
    )
}

/// Serve selected requests using a trusted host's raw-pack writer. Routing,
/// authorization and bounded selection are identical to `serve_with_selection`.
/// The writer runs once only when a pack is needed, including an empty selected
/// set, and must emit exactly `indices` from this same stable repository.
/// ACK-only rounds and shallow probes never invoke it. Its sink applies the
/// current request's sideband framing; cache only the raw PACK bytes.
///
/// Use `cache::selected::write` with `Selection::Indices(indices)` to integrate
/// verified caller-owned caches. Authorization and selection precede every hit.
/// Control packets may already be written when the writer fails: discard the
/// entire output on any error. Buffer the response before sending HTTP success,
/// or close the transport on a late failure. The writer retains no references
/// after this synchronous call, and a failure emits no successful terminator.
#[cfg(feature = "read")]
pub fn serve_with_selected_pack<R: Repository + ?Sized, W: Write>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    output: &mut W,
    write_selected: impl FnOnce(&[usize], &mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
) -> Result<Response> {
    serve_with_selected_pack_policy(
        repository,
        request,
        storage,
        WantOptions::STRICT,
        output,
        write_selected,
    )
}

/// Apply an explicit host want policy before the selected-pack writer or cache.
/// Every request, including hits and ACK-only rounds, is checked independently.
/// Keep the authorized repository projection immutable for the entire call.
/// Output and writer-failure contracts match `serve_with_selected_pack`.
#[cfg(feature = "read")]
pub fn serve_with_selected_pack_policy<R: Repository + ?Sized, W: Write>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut SelectionStorage<'_>,
    options: WantOptions,
    output: &mut W,
    write_selected: impl FnOnce(&[usize], &mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
) -> Result<Response> {
    serve_selected_request(
        repository,
        request,
        storage.reborrow(),
        options,
        None,
        output,
        write_selected,
    )
}

/// Serve time/exclusion deepening with explicit caller storage and exclusion limits.
/// This also supports all existing selected fetch modes. Only this opt-in path
/// advertises v0/v1 deepen-since/deepen-not. Successful output is invalidated by
/// any later error; a streaming host must abort on provider/transport failure.
#[cfg(feature = "read")]
pub fn serve_with_revisions<R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: &mut RevisionStorage<'_>,
    output: &mut impl Write,
) -> Result<Response> {
    let storage = storage.reborrow();
    serve_selected_request(
        repository,
        request,
        storage.selection,
        WantOptions::STRICT,
        Some((storage.state, storage.limits)),
        output,
        |indices, read, packed| {
            pack::write_selected_with_scratch(repository, indices, read, packed).map(|_| ())
        },
    )
}

#[cfg(feature = "read")]
fn serve_selected_request<R: Repository + ?Sized, W: Write>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'_>,
    storage: SelectionStorage<'_>,
    options: WantOptions,
    revision: Option<(&mut [u8], git_zero_core::RevisionLimits)>,
    output: &mut W,
    write_selected: impl FnOnce(&[usize], &mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
) -> Result<Response> {
    let version = match prepare_route(repository, request, options, revision.is_some(), output)? {
        http_backend::Plan::Buffered(response) => return Ok(response),
        http_backend::Plan::Loose(index) => {
            return http_backend::write_loose(repository, index, storage.read, output);
        }
        http_backend::Plan::Upload(version) => version,
    };
    let selected = prepare_fetch(
        repository, request, version, storage, options, revision, output,
    )?;
    if let Some(indices) = selected.indices {
        let mut packed = selected.upload.output(output);
        write_selected(indices, selected.read, &mut packed)?;
        packed.finish()?;
    }
    Ok(response())
}

#[cfg(feature = "read")]
pub(crate) fn prepare_route(
    repository: &impl Repository,
    request: Request<'_>,
    options: WantOptions,
    revisions: bool,
    output: &mut impl Write,
) -> Result<http_backend::Plan> {
    http_backend::prepare_with_refs(repository, request, output, |route, output| {
        if revisions {
            advertisement::serve_revision_info_refs(repository, route, output)
        } else {
            policy::info_refs(repository, route, output, options)
        }
    })
}

#[cfg(feature = "read")]
pub(crate) struct SelectedFetch<'storage, 'body> {
    pub(crate) upload: UploadRequest<'body>,
    pub(crate) indices: Option<&'storage [usize]>,
    read: &'storage mut [u8],
}

#[cfg(feature = "read")]
fn select_fetch<'indices, R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    upload: &UploadRequest<'_>,
    wants: &[ObjectId],
    work: &mut [Option<git_zero_core::ValidationMetadata>],
    indices: &'indices mut [usize],
    scratch: (
        &mut [u8],
        &mut [u8],
        Option<&mut git_zero_core::RevisionState<'_>>,
    ),
    options: SelectionOptions,
) -> Result<&'indices [usize]> {
    if upload.extensions.revision() {
        repository.select_revision_fetch(
            wants,
            (
                upload.common(repository),
                upload.shallow(),
                upload.exclusion_ids(repository),
            ),
            work,
            indices,
            (scratch.0, scratch.1, scratch.2.ok_or(Error::Unsupported)?),
            (
                options,
                upload.extensions.options,
                git_zero_core::RevisionOptions {
                    since: upload.extensions.since,
                },
            ),
        )
    } else {
        repository.select_bounded_fetch(
            wants,
            (upload.common(repository), upload.shallow()),
            work,
            indices,
            (scratch.0, scratch.1),
            (options, upload.extensions.options),
        )
    }
}

#[cfg(feature = "read")]
pub(crate) fn prepare_fetch<'storage, 'body, R: Repository + ?Sized>(
    repository: &ValidatedRepository<'_, '_, R>,
    request: Request<'body>,
    version: Version,
    storage: SelectionStorage<'storage>,
    options: WantOptions,
    revision: Option<(&mut [u8], git_zero_core::RevisionLimits)>,
    output: &mut impl Write,
) -> Result<SelectedFetch<'storage, 'body>> {
    let (upload, count) = UploadRequest::parse_selected(
        repository,
        request.body,
        version,
        storage.wants,
        options,
        revision.is_some(),
    )?;
    let mut revision =
        revision.map(|(state, limits)| git_zero_core::RevisionState::new(state, limits));
    if upload.extensions.exclusions > revision.as_ref().map_or(0, |state| state.max_exclusions()) {
        return Err(Error::LimitExceeded);
    }
    let boundary_probe = version != Version::V2 && !upload.done && upload.extensions.history();
    let select_boundaries = boundary_probe
        && (upload.extensions.revision()
            || !upload.extensions.options.deepen_relative
            || upload.shallow().next().is_some());
    let indices = if upload.done || select_boundaries {
        let options = SelectionOptions {
            graph_read_limit: storage.graph_read_limit,
            include_tags: upload.include_tags(),
        };
        Some(select_fetch(
            repository,
            &upload,
            &storage.wants[..count],
            storage.objects,
            storage.indices,
            (storage.read, storage.parse, revision.as_mut()),
            options,
        )?)
    } else {
        upload.validate_shallow(repository)?;
        if upload.extensions.revision() {
            for id in upload.exclusion_ids(repository) {
                id?;
            }
        }
        None
    };
    if indices.is_some() {
        if upload.extensions.revision() {
            upload.revision_info(
                repository.object_format(),
                revision
                    .as_ref()
                    .ok_or(Error::Unsupported)?
                    .changes(storage.objects),
                output,
            )?;
        } else {
            upload.shallow_info(
                repository.object_format(),
                &storage.objects[..repository.object_count()],
                output,
            )?;
        }
    } else if boundary_probe {
        output.write_all(b"0000")?;
    }
    upload.acknowledge(repository, output)?;
    Ok(SelectedFetch {
        indices: indices.filter(|_| upload.done),
        upload,
        read: storage.read,
    })
}

pub(crate) fn response() -> Response {
    Response {
        status: 200,
        content_type: "application/x-git-upload-pack-result",
    }
}

#[cfg(feature = "read")]
pub(super) fn advertised(repository: &impl Repository, id: ObjectId) -> bool {
    repository.head_state().id() == Some(id)
        || (0..repository.reference_count()).any(|index| {
            repository.reference_id(index) == Some(id)
                || repository.reference_peeled(index) == Some(id)
        })
}

/// Validate the HEAD-only complete-pack contract before either output path.
pub(crate) fn prepare_complete<'a>(
    repository: &impl Repository,
    body: &'a [u8],
    version: Version,
) -> Result<UploadRequest<'a>> {
    if !uses_only_head(repository) {
        return Err(Error::Unsupported);
    }
    let head = repository.head_state().id().ok_or(Error::InvalidRequest)?;
    let upload = UploadRequest::parse(body, repository.object_format(), version, |id| {
        (id == head).then_some(()).ok_or(Error::InvalidRequest)
    })?;
    upload.require_complete()?;
    Ok(upload)
}

pub(crate) fn write_upload<W: Write>(
    repository: &impl Repository,
    request: Request<'_>,
    version: Version,
    scratch: &mut [u8],
    output: &mut W,
    write_pack: impl FnOnce(&mut [u8], &mut PackOutput<'_, W>) -> Result<()>,
) -> Result<()> {
    let upload = prepare_complete(repository, request.body, version)?;
    upload.acknowledge(repository, output)?;
    if upload.done {
        let mut packed = upload.output(output);
        write_pack(scratch, &mut packed)?;
        packed.finish()?;
    }
    Ok(())
}
