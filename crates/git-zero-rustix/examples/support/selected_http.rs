//! Synchronous bounded response staging shared by the cache example and oracle.
use git_zero::{
    Error, ObjectId, Repository, Result, ValidatedRepository,
    cache::selected::{self, Buffers, Outcome, PackCache, Selection},
    http::{self, Response, SelectionBuffers, WantOptions},
    io::SliceWriter,
};
use rustix::fd::AsFd;

pub const MAX_PACK_BYTES: usize = 16 * 1024 * 1024;

pub struct Storage {
    request: Vec<u8>,
    response: Vec<u8>,
    pack: Vec<u8>,
    ids: [ObjectId; 256],
    selection: SelectionBuffers<64, 256, 512, 512>,
}

impl Storage {
    pub fn new(pack_bytes: usize) -> Result<Self> {
        if pack_bytes > MAX_PACK_BYTES {
            return Err(Error::LimitExceeded);
        }
        let response_bytes = pack_bytes
            .checked_add(131_072)
            .ok_or(Error::LimitExceeded)?;
        Ok(Self {
            request: vec![0; 65_536],
            response: vec![0; response_bytes],
            pack: vec![0; pack_bytes],
            ids: [ObjectId::Sha1([0; 20]); 256],
            selection: SelectionBuffers::new(),
        })
    }

    /// A failed request replaces its provisional body before any HTTP success.
    /// The caller owns/closes the socket, including after transport errors.
    pub fn respond<R: Repository + ?Sized>(
        &mut self,
        socket: &impl AsFd,
        repository: &ValidatedRepository<'_, '_, R>,
        cache: &mut impl PackCache,
        options: WantOptions,
    ) -> Result<(u16, Option<Outcome>)> {
        let mut outcome = None;
        let byte_limit = self.pack.len();
        let mut output = SliceWriter::new(&mut self.response);
        let prepared = (|| {
            let mut request = git_zero_rustix::read_request(socket, &mut self.request)?;
            let Some(path) = request.path.strip_prefix("/repo.git/") else {
                return Ok(Response {
                    status: 404,
                    content_type: "text/plain",
                });
            };
            request.path = path;
            http::serve_with_selected_pack_policy(
                repository,
                request,
                &mut self.selection.storage(16_777_216),
                options,
                &mut output,
                |indices, read, framed| {
                    outcome = Some(selected::write(
                        repository,
                        Selection::Indices(indices),
                        cache,
                        Buffers {
                            ids: &mut self.ids,
                            pack: &mut self.pack,
                            read,
                            byte_limit,
                        },
                        framed,
                    )?);
                    Ok(())
                },
            )
        })();
        let response = match prepared {
            Ok(response) => response,
            Err(error) => {
                // This replaces ACKs or partial pack bytes, never appends an ERR.
                output = SliceWriter::new(&mut self.response);
                http::write_upload_error(error, &mut output)?;
                Response {
                    status: match error {
                        Error::BufferTooSmall | Error::LimitExceeded => 413,
                        Error::Backend => 500,
                        _ => 400,
                    },
                    content_type: "application/x-git-upload-pack-result",
                }
            }
        };
        git_zero_rustix::respond(socket, response, output.as_bytes())?;
        Ok((response.status, outcome))
    }
}
