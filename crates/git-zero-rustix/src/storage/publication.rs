//! Verified immutable file downloads for packed dumb HTTP publication.
use super::{Compare, Region, file_size, pack_error};
use git_zero::{
    Error, Repository, Result,
    http::{
        Request, Response,
        dumb::{LooseObjects, Publication, PublishedResponse},
    },
    io::{SliceWriter, Write},
    pack::IndexEntry,
};
use rustix::fd::OwnedFd;

/// A complete publication backed by pinned regular-file descriptors. Unlike a
/// selected-cache entry, its names are the actual checksums of complete pack
/// bytes. Neither mutable pathnames nor a shared file cursor are retained.
///
/// Construction compares BOTH files byte-for-byte with a fresh publication of
/// this exact repository, including index offsets/CRCs and both trailers. The
/// supplied files must be the stored encoder's output; arbitrary foreign delta
/// packs are not accepted here. IDs/graph validation remain the provider's
/// contract; validate an untrusted repository first.
///
/// Open descriptors survive rename/unlink but do not prevent in-place writes.
/// The host must keep the raw repository AND both files immutable for the URL's
/// lifetime, publish refs/HEAD and pack/index together only after success, and
/// isolate each new generation under its own URL. This is not a disk quota,
/// staging cleanup, filesystem durability or distributed-lock implementation.
pub struct FilePublication<'a, R> {
    publication: Publication<'a, R>,
    pack: Region,
    index: Region,
}

impl<'a, R: Repository> FilePublication<'a, R> {
    pub fn from_fds(
        repository: &'a R,
        entries: &'a mut [IndexEntry],
        pack: OwnedFd,
        index: OwnedFd,
        encode_scratch: &mut [u8],
        compare_scratch: &mut [u8],
        loose: LooseObjects,
    ) -> Result<Self> {
        let pack = Region {
            size: file_size(&pack).map_err(pack_error)?,
            fd: pack,
            offset: 0,
        };
        let index = Region {
            size: file_size(&index).map_err(pack_error)?,
            fd: index,
            offset: 0,
        };
        let publication = Publication::new(repository, entries, encode_scratch, loose)?;
        if pack.size != publication.pack_size() || index.size != publication.index_size() {
            return Err(Error::CorruptPack);
        }
        for (file, extension) in [(&pack, b"pack".as_slice()), (&index, b"idx".as_slice())] {
            let mut bytes = [0; 96];
            let mut path = SliceWriter::new(&mut bytes);
            path.write_all(b"objects/pack/pack-")?;
            path.write_all(publication.checksum().to_hex().as_bytes())?;
            path.write_all(b".")?;
            path.write_all(extension)?;
            let path = core::str::from_utf8(path.as_bytes()).map_err(|_| Error::InvalidRequest)?;
            let request = Request {
                method: "GET",
                path,
                query: None,
                body: b"",
                git_protocol: None,
                content_type: None,
            };
            let mut compare = Compare {
                file,
                position: 0,
                scratch: compare_scratch,
            };
            publication.serve(request, encode_scratch, &mut compare)?;
            if compare.position != file.size {
                return Err(Error::CorruptPack);
            }
        }
        Ok(Self {
            publication,
            pack,
            index,
        })
    }

    pub fn checksum(&self) -> git_zero::ObjectId {
        self.publication.checksum()
    }
    pub fn pack_size(&self) -> u64 {
        self.pack.size
    }
    pub fn index_size(&self) -> u64 {
        self.index.size
    }

    /// Same routing/fallthrough as `Publication::prepare`. Successful pack/index
    /// downloads use only positional reads of the verified files; listing bytes
    /// remain generated. Every cursor owns its own offset and cancellation state.
    pub fn prepare(&self, request: Request<'_>) -> Result<Option<FilePublishedResponse<'_, R>>> {
        let Some(cursor) = self.publication.prepare(request)? else {
            return Ok(None);
        };
        let response = cursor.response();
        let size = cursor.size();
        let body = match response.content_type {
            "application/x-git-packed-objects" => FileBody::File {
                file: &self.pack,
                position: 0,
            },
            "application/x-git-packed-objects-toc" => FileBody::File {
                file: &self.index,
                position: 0,
            },
            _ => FileBody::Generated(cursor),
        };
        Ok(Some(FilePublishedResponse {
            response,
            size,
            body,
            cancelled: false,
        }))
    }
}

#[allow(clippy::large_enum_variant)]
enum FileBody<'a, R> {
    File { file: &'a Region, position: u64 },
    Generated(PublishedResponse<'a, R>),
}

/// A verified file response with independent positional I/O. Keep the publication
/// alive until completion; never resume after an error or reissue response headers.
pub struct FilePublishedResponse<'a, R> {
    response: Response,
    size: u64,
    body: FileBody<'a, R>,
    cancelled: bool,
}
impl<R: Repository> FilePublishedResponse<'_, R> {
    pub fn response(&self) -> Response {
        self.response
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        if self.cancelled {
            return Err(Error::InvalidRequest);
        }
        if output.is_empty() {
            return Err(Error::BufferTooSmall);
        }
        let result = match &mut self.body {
            FileBody::Generated(cursor) => cursor.read(output),
            FileBody::File { file, position } => file.read(*position, output).inspect(|&count| {
                *position += count as u64;
            }),
        };
        if result.is_err() {
            self.cancel();
        }
        result
    }
}
