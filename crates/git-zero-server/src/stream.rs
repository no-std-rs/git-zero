//! Host-driven request intake and pull-based responses. HTTP transfer framing,
//! socket readiness and staging unsent output remain the host's responsibility.
use crate::{Request, Response, route, upload::UploadRequest};
use git_zero_core::{Error, Repository, Result, io::SliceWriter, pack, repository::uses_only_head};

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
}

impl<'a> ResponseEncoder<'a> {
    /// Prepare a HEAD-only repository. Independent roots need `with_selection`.
    pub fn new(
        repository: &impl Repository,
        request: Request<'_>,
        control: &'a mut [u8],
    ) -> Result<Self> {
        let mut output = SliceWriter::new(control);
        if let Some(response) = Self::ordinary(repository, request, &mut output)? {
            return Self::ordinary_response(repository, request, response, output.finish());
        }
        if !uses_only_head(repository) {
            return Err(Error::Unsupported);
        }
        let head = repository.head_state().id().ok_or(Error::InvalidRequest)?;
        let upload = UploadRequest::parse(
            request.body,
            repository.object_format(),
            route::validate(request)?.version,
            |id| (id == head).then_some(()).ok_or(Error::InvalidRequest),
        )?;
        upload.acknowledge(repository, &mut output)?;
        let mut encoder = Self::buffered(upload_response(), output.finish());
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
        let mut output = SliceWriter::new(control);
        if let Some(response) = Self::ordinary(repository, request, &mut output)? {
            return Self::ordinary_response(repository, request, response, output.finish());
        }
        let mut count = 0;
        let upload = UploadRequest::parse(
            request.body,
            repository.object_format(),
            route::validate(request)?.version,
            |id| {
                if !crate::advertised(repository, id) {
                    return Err(Error::InvalidRequest);
                }
                if !storage.wants[..count].contains(&id) {
                    *storage.wants.get_mut(count).ok_or(Error::BufferTooSmall)? = id;
                    count += 1;
                }
                Ok(())
            },
        )?;
        let indices = if upload.done {
            Some(repository.select_fetch_objects(
                &storage.wants[..count],
                upload.common(repository),
                storage.objects,
                storage.indices,
                (storage.read, storage.parse),
                git_zero_core::SelectionOptions {
                    graph_read_limit: storage.graph_read_limit,
                    include_tags: upload.include_tags(),
                },
            )?)
        } else {
            None
        };
        upload.acknowledge(repository, &mut output)?;
        let mut encoder = Self::buffered(upload_response(), output.finish());
        if let Some(indices) = indices {
            encoder.body = Body::Pack(pack::Encoder::new(repository, Some(indices))?);
            encoder.band = upload.band();
        }
        Ok(encoder)
    }

    fn ordinary(
        repository: &impl Repository,
        request: Request<'_>,
        output: &mut SliceWriter<'_>,
    ) -> Result<Option<Response>> {
        let route = route::validate(request)?;
        if let Some(response) = route.rejection {
            return Ok(Some(response));
        }
        if request.method == "POST" && route.path == "git-upload-pack" {
            if route.version == route::Version::V2
                && crate::v2::serve_refs(repository, request.body, output)?
            {
                return Ok(Some(upload_response()));
            }
            return Ok(None);
        }
        // Loose bodies are prepared separately, without buffering their bytes.
        if request.method == "GET"
            && route.path.starts_with("objects/")
            && crate::loose_index(repository, &route.path.as_bytes()[8..]).is_some()
        {
            return Ok(Some(Response {
                status: 200,
                content_type: "application/x-git-loose-object",
            }));
        }
        crate::serve_with_scratch(repository, request, &mut [], output).map(Some)
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
        }
    }

    fn ordinary_response(
        repository: &impl Repository,
        request: Request<'_>,
        response: Response,
        prefix: &'a [u8],
    ) -> Result<Self> {
        let mut encoder = Self::buffered(response, prefix);
        if response.content_type == "application/x-git-loose-object" {
            let route = route::validate(request)?;
            let index = crate::loose_index(repository, &route.path.as_bytes()[8..])
                .ok_or(Error::ObjectNotFound)?;
            encoder.body = Body::Loose(pack::LooseEncoder::new(repository, index)?);
        }
        Ok(encoder)
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
            let capacity = if self.band == 1 { 995 } else { 1024 };
            let Body::Pack(pack) = &mut self.body else {
                unreachable!()
            };
            let length = pack.read(repository, &mut self.packet[5..5 + capacity])?;
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

    fn read_body(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        match &mut self.body {
            Body::Buffered => Ok(0),
            Body::Pack(pack) => pack.read(repository, output),
            Body::Loose(loose) => loose.read(repository, output),
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

fn upload_response() -> Response {
    Response {
        status: 200,
        content_type: "application/x-git-upload-pack-result",
    }
}
