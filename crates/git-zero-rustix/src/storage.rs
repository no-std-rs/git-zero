//! Raw object bodies in immutable regular files, read by offset with rustix.
use core::ffi::CStr;

use git_zero::{
    Error, ObjectId, ObjectInfo, ObjectKind, Repository, Result,
    http::{self, Request, Response},
    io::Write,
    pack,
    repository::{MAX_HEAD_REF_LENGTH, validate_ref_name},
};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{self, FileType, Mode, OFlags},
    io::{self, Errno},
};

/// One object's uncompressed body in an owned file descriptor.
///
/// The body may occupy the whole file or a byte range in a shared data file.
/// Git loose zlib files and compressed pack entries require decoding first.
/// IDs and kinds come from the caller's manifest; use ValidatedRepository to
/// verify them and the complete graph. An open descriptor pins the file across
/// renames, replacement and unlinking, but the host must prevent in-place writes
/// for the lifetime of the repository and its URL. This is not a disk snapshot.
pub struct FileObject {
    file: Region,
    id: ObjectId,
    kind: ObjectKind,
}

impl FileObject {
    /// Open relative to a directory without allocating a path. The final path
    /// component must be a regular file rather than a symlink. This is ordinary
    /// openat, not confinement of arbitrary paths to the supplied directory.
    pub fn open_at(
        directory: impl AsFd,
        path: &CStr,
        kind: ObjectKind,
        id: ObjectId,
    ) -> Result<Self> {
        let fd = fs::openat(
            directory,
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| {
            if error == Errno::NOENT {
                Error::ObjectNotFound
            } else {
                Error::Backend
            }
        })?;
        Self::from_fd(fd, kind, id)
    }

    /// Take ownership of an open regular file; its whole current body is used.
    pub fn from_fd(fd: OwnedFd, kind: ObjectKind, id: ObjectId) -> Result<Self> {
        let size = file_size(&fd)?;
        Self::from_range(fd, 0, ObjectInfo { kind, size }, id)
    }

    /// Take ownership of an open descriptor and a raw body range. Ranges must
    /// fit the current file length and signed 64-bit native file offsets.
    pub fn from_range(fd: OwnedFd, offset: u64, info: ObjectInfo, id: ObjectId) -> Result<Self> {
        let end = offset.checked_add(info.size).ok_or(Error::InvalidObject)?;
        if end > i64::MAX as u64 || end > file_size(&fd)? {
            return Err(Error::InvalidObject);
        }
        Ok(Self {
            file: Region {
                fd,
                offset,
                size: info.size,
            },
            id,
            kind: info.kind,
        })
    }

    pub fn id(&self) -> ObjectId {
        self.id
    }
    pub fn info(&self) -> ObjectInfo {
        ObjectInfo {
            kind: self.kind,
            size: self.file.size,
        }
    }
}

/// A caller-owned manifest of immutable file-backed objects and commit aliases.
///
/// No filesystem scan, allocation, or object-sized buffer is needed. Keep the
/// FileObject array alive; each element owns one descriptor. Object enumeration
/// follows manifest order; lookup is linear unless wrapped in ValidatedRepository.
/// Construction does not check IDs or graph closure. Those remain Repository
/// preconditions until checked with the core validator.
pub struct FileRepository<'a> {
    objects: &'a [FileObject],
    head: ObjectId,
    head_ref: &'a str,
    refs: &'a [&'a str],
    prepacked: Option<Region>,
}

impl<'a> FileRepository<'a> {
    pub fn new(head: ObjectId, objects: &'a [FileObject]) -> Self {
        Self {
            objects,
            head,
            head_ref: "refs/heads/main",
            refs: &["refs/heads/main"],
            prepacked: None,
        }
    }

    /// Advertise sorted, unique refs that all alias head, including head_ref.
    pub fn with_refs(mut self, head_ref: &'a str, refs: &'a [&'a str]) -> Result<Self> {
        validate_ref_name(head_ref)?;
        if !head_ref.starts_with("refs/heads/")
            || head_ref.len() > MAX_HEAD_REF_LENGTH
            || !refs.contains(&head_ref)
        {
            return Err(Error::InvalidReference);
        }
        for (index, name) in refs.iter().enumerate() {
            validate_ref_name(name)?;
            if refs[..index].iter().any(|previous| {
                previous >= name
                    || name
                        .strip_prefix(previous)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }) {
                return Err(Error::InvalidReference);
            }
        }
        self.head_ref = head_ref;
        self.refs = refs;
        Ok(self)
    }

    /// Attach an immutable, already-generated complete pack in a regular file.
    /// Compare every byte with pack::write_with_scratch, including framing,
    /// object closure/order, encoder bytes and checksum, before accepting it.
    /// Only this encoder's exact output is accepted; arbitrary compressed/delta
    /// packs remain future work. No pack-sized buffer is needed. Both scratch
    /// buffers need at least one byte for nonempty streamed data. A mismatch,
    /// extra/truncated bytes or a nonregular file returns CorruptPack.
    /// Verification costs one encoding pass; subsequent writes only read the
    /// pack file. Refs may change afterwards without changing pack identity.
    pub fn with_prepacked(
        mut self,
        fd: OwnedFd,
        object_scratch: &mut [u8],
        compare_scratch: &mut [u8],
    ) -> Result<Self> {
        let size = file_size(&fd).map_err(|error| {
            if error == Error::InvalidObject {
                Error::CorruptPack
            } else {
                error
            }
        })?;
        let file = Region {
            fd,
            offset: 0,
            size,
        };
        let mut compare = Compare {
            file: &file,
            position: 0,
            scratch: compare_scratch,
        };
        pack::write_with_scratch(&self, object_scratch, &mut compare)?;
        if compare.position != file.size {
            return Err(Error::CorruptPack);
        }
        self.prepacked = Some(file);
        Ok(self)
    }

    pub fn has_prepacked(&self) -> bool {
        self.prepacked.is_some()
    }

    /// Stream the verified stored pack, or generate one from the raw files.
    /// Scratch needs at least one byte. Discard partial output on error.
    pub fn write_pack_with_scratch(
        &self,
        scratch: &mut [u8],
        output: &mut impl Write,
    ) -> Result<()> {
        if let Some(file) = &self.prepacked {
            let mut offset = 0;
            while offset < file.size {
                if scratch.is_empty() {
                    return Err(Error::BufferTooSmall);
                }
                let length = file.read(offset, scratch)?;
                output.write_all(&scratch[..length])?;
                offset += length as u64;
            }
            Ok(())
        } else {
            pack::write_with_scratch(self, scratch, output).map(|_| ())
        }
    }

    /// Core HTTP routes with verified prepacked reuse for upload-pack responses.
    /// Dumb loose objects continue to be encoded from their raw file bodies.
    /// Use core http::serve_with_pack to pair this writer with a validated index.
    pub fn serve_with_scratch(
        &self,
        request: Request<'_>,
        scratch: &mut [u8],
        output: &mut impl Write,
    ) -> Result<Response> {
        http::serve_with_pack(self, request, scratch, output, |scratch, output| {
            self.write_pack_with_scratch(scratch, output)
        })
    }
}

impl Repository for FileRepository<'_> {
    fn head(&self) -> ObjectId {
        self.head
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.objects
            .get(index)
            .map(FileObject::info)
            .ok_or(Error::ObjectNotFound)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects.get(index).map(FileObject::id)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        self.objects
            .get(index)
            .ok_or(Error::ObjectNotFound)?
            .file
            .read(offset, storage)
    }
    fn head_ref(&self) -> &str {
        self.head_ref
    }
    fn reference_count(&self) -> usize {
        self.refs.len()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.refs.get(index).copied()
    }
}

fn file_size(fd: &impl AsFd) -> Result<u64> {
    let stat = fs::fstat(fd).map_err(|_| Error::Backend)?;
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(Error::InvalidObject);
    }
    u64::try_from(stat.st_size).map_err(|_| Error::InvalidObject)
}

struct Region {
    fd: OwnedFd,
    offset: u64,
    size: u64,
}

impl Region {
    fn read(&self, offset: u64, storage: &mut [u8]) -> Result<usize> {
        if offset >= self.size || storage.is_empty() {
            return Ok(0);
        }
        let capacity = (self.size - offset).min(storage.len() as u64) as usize;
        loop {
            match io::pread(&self.fd, &mut storage[..capacity], self.offset + offset) {
                Ok(0) => return Err(Error::InvalidObject), // File was truncated after construction.
                Ok(length) => return Ok(length),
                Err(Errno::INTR) => continue,
                Err(_) => return Err(Error::Backend),
            }
        }
    }
}

struct Compare<'a> {
    file: &'a Region,
    position: u64,
    scratch: &'a mut [u8],
}
impl Write for Compare<'_> {
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            if self.scratch.is_empty() {
                return Err(Error::BufferTooSmall);
            }
            let capacity = self.scratch.len().min(bytes.len());
            let length = self
                .file
                .read(self.position, &mut self.scratch[..capacity])
                .map_err(|error| {
                    if error == Error::InvalidObject {
                        Error::CorruptPack
                    } else {
                        error
                    }
                })?;
            if length == 0 || self.scratch[..length] != bytes[..length] {
                return Err(Error::CorruptPack);
            }
            self.position += length as u64;
            bytes = &bytes[length..];
        }
        Ok(())
    }
}
