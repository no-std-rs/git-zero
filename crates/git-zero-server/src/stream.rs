//! Host-driven request intake and pull-based responses. HTTP transfer framing,
//! socket readiness and staging unsent output remain the host's responsibility.
#[cfg(feature = "read")]
pub mod preparation;

use crate::{
    Request, Response,
    http_backend::{self, Plan},
    upload_pack,
};
use git_zero_core::{Error, Repository, Result, io::SliceWriter, pack};

/// Incrementally collect a bounded request body in caller-owned storage. No
/// protocol output is possible until `finish` returns a complete body to parse.
/// Pass a validated Content-Length when known; otherwise the host supplies EOF.
pub struct RequestBody<'a> {
    storage: &'a mut [u8],
    length: usize,
    expected: Option<usize>,
    cancelled: bool,
}

impl<'a> RequestBody<'a> {
    pub fn new(storage: &'a mut [u8], expected: Option<usize>) -> Result<Self> {
        if expected.is_some_and(|length| length > storage.len()) {
            return Err(Error::BufferTooSmall);
        }
        Ok(Self {
            storage,
            length: 0,
            expected,
            cancelled: false,
        })
    }

    /// Append all bytes or none. A rejected fragment leaves intake resumable.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if self.cancelled {
            return Err(Error::InvalidRequest);
        }
        let end = self
            .length
            .checked_add(bytes.len())
            .ok_or(Error::BufferTooSmall)?;
        if self.expected.is_some_and(|length| end > length) {
            return Err(Error::InvalidRequest);
        }
        self.storage
            .get_mut(self.length..end)
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }

    pub fn received(&self) -> usize {
        self.length
    }

    /// Terminal cancellation. Drop this value to reuse its caller-owned storage.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn finish(self) -> Result<&'a [u8]> {
        if self.cancelled || self.expected.is_some_and(|length| length != self.length) {
            return Err(Error::InvalidRequest);
        }
        Ok(&self.storage[..self.length])
    }
}

// Inline variants keep the cursor allocation-free with a fixed size budget.
#[allow(clippy::large_enum_variant)]
enum Body<'a> {
    Buffered,
    Pack(pack::Encoder<'a>),
    Loose(pack::LooseEncoder),
    #[cfg(feature = "read")]
    Cached {
        bytes: &'a [u8],
        position: usize,
    },
}

/// An independent response cursor with bounded inline framing state. The host
/// owns the repository, request and control buffers. Keep the same stable
/// repository for every `read`; it may be recreated from immutable inputs.
/// Discovery/ref/ACK bytes must fit `control`; object bodies never use it.
/// A completed prepare validates routes, commands, wants and selection budgets
/// before output. Provider failures after headers require aborting the transport.
pub struct ResponseEncoder<'a> {
    response: Response,
    prefix: &'a [u8],
    position: usize,
    body: Body<'a>,
    band: u8,
    packet: [u8; 1029],
    packet_position: usize,
    packet_length: usize,
    terminated: bool,
    cancelled: bool,
    started: bool,
}

impl<'a> ResponseEncoder<'a> {
    /// Prepare a HEAD-only repository. Independent roots need `with_selection`.
    pub fn new(
        repository: &impl Repository,
        request: Request<'_>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        let mut output = SliceWriter::new(control);
        let plan = http_backend::prepare(repository, request, &mut output, false)?;
        let Plan::Upload(version) = plan else {
            return Self::ordinary_response(repository, plan, output.finish());
        };
        let upload = upload_pack::prepare_complete(repository, request.body, version)?;
        upload.acknowledge(repository, &mut output)?;
        let mut encoder = Self::buffered(upload_pack::response(), output.finish());
        if upload.done {
            encoder.body = Body::Pack(pack::Encoder::new(repository, None)?);
            encoder.band = upload.band();
        }
        Ok(encoder)
    }

    /// Prepare the exact requested closure with the same limits as
    /// `serve_with_selection`. Selection slots stay borrowed until this cursor
    /// is dropped; separate storage permits independent overlapping responses.
    #[cfg(feature = "read")]
    pub fn with_selection<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: &'a mut crate::SelectionStorage<'_>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::with_selection_storage(repository, request, storage.reborrow(), control)
    }

    /// Move a request's storage descriptor into preparation. Only its selected
    /// index array and control bytes remain borrowed by the resulting cursor.
    #[cfg(feature = "read")]
    pub fn with_selection_storage<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: crate::SelectionStorage<'a>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::with_selection_storage_policy(
            repository,
            request,
            storage,
            crate::WantOptions::STRICT,
            control,
        )
    }

    /// Additive host-granted selection. Only selected indices/control are pinned;
    /// the policy cannot be inherited by another cursor or strict constructor.
    #[cfg(feature = "read")]
    pub fn with_selection_policy<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: &'a mut crate::SelectionStorage<'_>,
        options: crate::WantOptions,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::with_selection_storage_policy(
            repository,
            request,
            storage.reborrow(),
            options,
            control,
        )
    }

    #[cfg(feature = "read")]
    pub fn with_selection_storage_policy<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: crate::SelectionStorage<'a>,
        options: crate::WantOptions,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::selected_storage(repository, request, storage, options, None, control)
    }

    /// Opt in to time/exclusion deepening. Boundary state is temporary; only
    /// selected indices and prepared control bytes survive in the response.
    #[cfg(feature = "read")]
    pub fn with_revisions<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: &'a mut crate::RevisionStorage<'_>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::with_revision_storage(repository, request, storage.reborrow(), control)
    }

    #[cfg(feature = "read")]
    pub fn with_revision_storage<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: crate::RevisionStorage<'a>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::with_revision_storage_policy(
            repository,
            request,
            storage,
            crate::WantOptions::STRICT,
            control,
        )
    }

    /// Combine explicit revision budgets with a request-local want grant. Neither
    /// capability can be inherited by another response or a strict constructor.
    #[cfg(feature = "read")]
    pub fn with_revision_storage_policy<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: crate::RevisionStorage<'a>,
        options: crate::WantOptions,
        control: &'a mut [u8],
    ) -> Result<Self> {
        Self::selected_storage(
            repository,
            request,
            storage.selection,
            options,
            Some((storage.state, storage.limits)),
            control,
        )
    }

    #[cfg(feature = "read")]
    fn selected_storage<R: Repository + ?Sized>(
        repository: &git_zero_core::ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        storage: crate::SelectionStorage<'a>,
        options: crate::WantOptions,
        revision: Option<(&mut [u8], git_zero_core::RevisionLimits)>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        let mut output = SliceWriter::new(control);
        let plan = upload_pack::prepare_route(
            repository,
            request,
            options,
            revision.is_some(),
            &mut output,
        )?;
        let Plan::Upload(version) = plan else {
            return Self::ordinary_response(repository, plan, output.finish());
        };
        let selected = upload_pack::prepare_fetch(
            repository,
            request,
            version,
            storage,
            options,
            revision,
            &mut output,
        )?;
        let mut encoder = Self::buffered(upload_pack::response(), output.finish());
        if let Some(indices) = selected.indices {
            encoder.body = Body::Pack(pack::Encoder::new(repository, Some(indices))?);
            encoder.band = selected.upload.band();
        }
        Ok(encoder)
    }

    fn buffered(response: Response, prefix: &'a [u8]) -> Self {
        Self {
            response,
            prefix,
            position: 0,
            body: Body::Buffered,
            band: 0,
            packet: [0; 1029],
            packet_position: 0,
            packet_length: 0,
            terminated: false,
            cancelled: false,
            started: false,
        }
    }

    fn ordinary_response(
        repository: &impl Repository,
        plan: Plan,
        prefix: &'a [u8],
    ) -> Result<Self> {
        match plan {
            Plan::Buffered(response) => Ok(Self::buffered(response, prefix)),
            Plan::Loose(index) => {
                let mut encoder = Self::buffered(
                    Response {
                        status: 200,
                        content_type: "application/x-git-loose-object",
                    },
                    prefix,
                );
                encoder.body = Body::Loose(pack::LooseEncoder::new(repository, index)?);
                Ok(encoder)
            }
            Plan::Upload(_) => unreachable!(),
        }
    }

    pub fn response(&self) -> Response {
        self.response
    }

    /// Terminal cancellation never emits a successful terminator. Close/reset
    /// the transport; no subsequent read can access repository objects.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Pull into host staging only when ready. Retain unsent bytes across short
    /// writes. Empty output is rejected while active. Zero denotes clean EOF.
    pub fn read(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        if self.cancelled {
            return Err(Error::InvalidRequest);
        }
        self.started = true;
        if self.position < self.prefix.len() {
            return copy(self.prefix, &mut self.position, output);
        }
        if self.band == 0 {
            return self.read_body(repository, output);
        }
        if self.packet_position == self.packet_length && !self.terminated {
            if output.is_empty() {
                return Err(Error::BufferTooSmall);
            }
            self.next_packet(repository)?;
        }

        if self.packet_position == self.packet_length {
            return Ok(0);
        }
        copy(
            &self.packet[..self.packet_length],
            &mut self.packet_position,
            output,
        )
    }

    fn next_packet(&mut self, repository: &impl Repository) -> Result<()> {
        let capacity = if self.band == 1 { 995 } else { 1024 };
        let length = match &mut self.body {
            Body::Pack(pack) => pack.read(repository, &mut self.packet[5..5 + capacity])?,
            #[cfg(feature = "read")]
            Body::Cached { bytes, position } => {
                if *position == bytes.len() {
                    0
                } else {
                    copy(bytes, position, &mut self.packet[5..5 + capacity])?
                }
            }
            _ => unreachable!(),
        };
        self.packet_position = 0;
        if length == 0 {
            self.packet[..4].copy_from_slice(b"0000");
            self.packet_length = 4;
            self.terminated = true;
        } else {
            let size = length + 5;
            for (index, byte) in self.packet[..4].iter_mut().enumerate() {
                *byte = b"0123456789abcdef"[(size >> ((3 - index) * 4)) & 15];
            }
            self.packet[4] = 1;
            self.packet_length = size;
        }
        Ok(())
    }

    fn read_body(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        match &mut self.body {
            Body::Buffered => Ok(0),
            Body::Pack(pack) => pack.read(repository, output),
            Body::Loose(loose) => loose.read(repository, output),
            #[cfg(feature = "read")]
            Body::Cached { bytes, position } => {
                if *position == bytes.len() {
                    Ok(0)
                } else {
                    copy(bytes, position, output)
                }
            }
        }
    }
}

fn copy(bytes: &[u8], position: &mut usize, output: &mut [u8]) -> Result<usize> {
    if output.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let count = output.len().min(bytes.len() - *position);
    output[..count].copy_from_slice(&bytes[*position..*position + count]);
    *position += count;
    Ok(count)
}
