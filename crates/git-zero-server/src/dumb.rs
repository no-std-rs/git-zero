//! Immutable packed dumb HTTP publication. Pack names use wire checksums, never
//! selected-cache keys. The host pins the publication's URL and repository.
use crate::{Request, Response, route};
use git_zero_core::{
    Error, ObjectId, Repository, Result,
    io::Write,
    pack::{self, IndexEntry},
};

/// Whether raw loose-object routes remain available alongside the published pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LooseObjects {
    Enabled,
    Disabled,
}

/// One complete stored pack and its matching index, generated from an immutable
/// repository. Preparation encodes every object once into a counting sink and
/// retains one caller-owned metadata slot per object, rather than pack bytes.
/// Downloads replay that deterministic encoding; index downloads use the retained
/// metadata and never read object bodies. Separate response cursors can overlap.
///
/// The repository must satisfy `Repository`'s accurate-ID, complete visible graph
/// and stability contracts. Use `ValidatedRepository` for untrusted providers.
/// Keep refs, HEAD, object enumeration, metadata and bodies unchanged throughout
/// this value's life AND the lifetime of its published URL. A new repository
/// requires a new publication/URL; replacing a live URL breaks dumb HTTP retries.
/// Preparation failures publish nothing. A failed response must be discarded or
/// its transport aborted. This API publishes the stored encoder's full graph;
/// arbitrary compressed/delta input and selected-pack reuse are separate APIs.
pub struct Publication<'a, R> {
    repository: &'a R,
    entries: &'a [IndexEntry],
    checksum: ObjectId,
    pack_size: u64,
    index_size: u64,
    loose: LooseObjects,
    listing: [u8; 80],
    listing_length: usize,
}

impl<'a, R: Repository> Publication<'a, R> {
    pub fn new(
        repository: &'a R,
        entries: &'a mut [IndexEntry],
        scratch: &mut [u8],
        loose: LooseObjects,
    ) -> Result<Self> {
        let mut pack_output = Count(0);
        let mut index_output = Count(0);
        let checksum = pack::write_indexed_with_scratch(
            repository,
            None,
            entries,
            scratch,
            &mut pack_output,
            &mut index_output,
        )?;
        let hex = checksum.to_hex();
        let mut listing = [0; 80];
        let prefix = b"P pack-";
        listing[..prefix.len()].copy_from_slice(prefix);
        listing[prefix.len()..prefix.len() + hex.len()].copy_from_slice(&hex);
        let suffix = b".pack\n\n";
        let end = prefix.len() + hex.len();
        listing[end..end + suffix.len()].copy_from_slice(suffix);
        Ok(Self {
            repository,
            entries: &entries[..repository.object_count()],
            checksum,
            pack_size: pack_output.0,
            index_size: index_output.0,
            loose,
            listing,
            listing_length: end + suffix.len(),
        })
    }

    pub fn checksum(&self) -> ObjectId {
        self.checksum
    }
    pub fn pack_size(&self) -> u64 {
        self.pack_size
    }
    pub fn index_size(&self) -> u64 {
        self.index_size
    }

    /// Dispatch publication routes before ordinary smart/loose serving. `None`
    /// means the caller must use its normal handler (including authorization).
    /// Shared route validation applies even to fallthrough requests. Discovery
    /// and refs still come from this exact immutable repository via that handler.
    /// Headers may include Content-Length from the cursor. Keep mutable refs and
    /// discovery uncached; immutable pack/index bytes may be cached by the host.
    pub fn prepare(&self, request: Request<'_>) -> Result<Option<PublishedResponse<'_, R>>> {
        let route = route::validate(request)?;
        if let Some(response) = route.rejection {
            return Ok(Some(PublishedResponse::new(
                self.repository,
                response,
                Body::Empty,
                0,
            )));
        }
        let (body, content_type, length) = match route.path {
            "objects/info/packs" => (
                Body::Bytes {
                    bytes: &self.listing[..self.listing_length],
                    position: 0,
                },
                "text/plain; charset=utf-8",
                self.listing_length as u64,
            ),
            path if path.starts_with("objects/pack/") => {
                let name = &path.as_bytes()[13..];
                let hex = self.checksum.to_hex();
                let Some(suffix) = name
                    .strip_prefix(b"pack-")
                    .and_then(|name| name.strip_prefix(hex.as_bytes()))
                else {
                    return Ok(Some(self.not_found()));
                };
                match suffix {
                    b".pack" => (
                        Body::Pack(pack::Encoder::new(self.repository, None)?),
                        "application/x-git-packed-objects",
                        self.pack_size,
                    ),
                    b".idx" => (
                        Body::Index(pack::IndexEncoder::new(self.entries, self.checksum)?),
                        "application/x-git-packed-objects-toc",
                        self.index_size,
                    ),
                    _ => return Ok(Some(self.not_found())),
                }
            }
            path if path.starts_with("objects/") && self.loose == LooseObjects::Disabled => {
                return Ok(Some(self.not_found()));
            }
            _ => return Ok(None),
        };
        Ok(Some(PublishedResponse::new(
            self.repository,
            Response {
                status: 200,
                content_type,
            },
            body,
            length,
        )))
    }

    fn not_found(&self) -> PublishedResponse<'_, R> {
        PublishedResponse::new(
            self.repository,
            Response {
                status: 404,
                content_type: "text/plain; charset=utf-8",
            },
            Body::Empty,
            0,
        )
    }

    /// Buffered-writer convenience over the same independent pull cursor. On a
    /// sink/provider error all output is invalid. No bytes are written on `None`.
    pub fn serve(
        &self,
        request: Request<'_>,
        scratch: &mut [u8],
        output: &mut impl Write,
    ) -> Result<Option<Response>> {
        let Some(mut cursor) = self.prepare(request)? else {
            return Ok(None);
        };
        loop {
            let length = cursor.read(scratch)?;
            if length == 0 {
                return Ok(Some(cursor.response()));
            }
            output.write_all(&scratch[..length])?;
        }
    }
}

struct Count(u64);
impl Write for Count {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or(Error::LimitExceeded)?;
        Ok(())
    }
}

#[allow(clippy::large_enum_variant)]
enum Body<'a> {
    Empty,
    Bytes { bytes: &'a [u8], position: usize },
    Pack(pack::Encoder<'a>),
    Index(pack::IndexEncoder<'a>),
}

/// A publication-bound response: each cursor pins the same repository and entry
/// metadata. Dropping/cancelling one cursor cannot advance or cancel another.
pub struct PublishedResponse<'a, R> {
    repository: &'a R,
    response: Response,
    body: Body<'a>,
    size: u64,
    cancelled: bool,
}
impl<'a, R: Repository> PublishedResponse<'a, R> {
    fn new(repository: &'a R, response: Response, body: Body<'a>, size: u64) -> Self {
        Self {
            repository,
            response,
            body,
            size,
            cancelled: false,
        }
    }
    pub fn response(&self) -> Response {
        self.response
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Zero is clean EOF. An empty buffer is rejected without advancing active
    /// output. Provider errors terminally cancel the cursor; abort the transport.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        if self.cancelled {
            return Err(Error::InvalidRequest);
        }
        // Empty output is a caller capacity error, not a provider failure.
        if output.is_empty() && !matches!(self.body, Body::Empty) {
            return Err(Error::BufferTooSmall);
        }
        let result = match &mut self.body {
            Body::Empty => Ok(0),
            Body::Bytes { bytes, position } => {
                let length = output.len().min(bytes.len() - *position);
                output[..length].copy_from_slice(&bytes[*position..*position + length]);
                *position += length;
                Ok(length)
            }
            Body::Pack(encoder) => encoder.read(self.repository, output),
            Body::Index(encoder) => encoder.read(output),
        };
        if result.is_err() {
            self.cancel();
        }
        result
    }
}
