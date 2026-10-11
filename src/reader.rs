//! Bounded object reads and resumable SHA-1 hashing. State is stored inline;
//! callers own the body buffer and can retain state between individual steps.
use sha1::{Digest, Sha1};

use crate::{Error, ObjectId, ObjectInfo, Repository, Result};

/// Stack scratch used by convenience pack/HTTP writers for streamed bodies.
/// The `*_with_scratch` variants let callers choose another budget.
pub const DEFAULT_READ_BUFFER_SIZE: usize = 1024;

/// A cursor over a stable repository object. It contains only a repository
/// reference, index, metadata, and offset, with no body buffer or allocation.
/// Use `core::mem::size_of::<ObjectReader<'_, R>>()` for the host's state budget.
pub struct ObjectReader<'repo, R: Repository + ?Sized> {
    repository: &'repo R,
    index: usize,
    info: ObjectInfo,
    position: u64,
}

impl<'repo, R: Repository + ?Sized> ObjectReader<'repo, R> {
    pub fn new(repository: &'repo R, index: usize) -> Result<Self> {
        Ok(Self {
            repository,
            index,
            info: repository.object_info(index)?,
            position: 0,
        })
    }

    pub fn info(&self) -> ObjectInfo {
        self.info
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn remaining(&self) -> u64 {
        self.info.size - self.position
    }

    /// Resume or reread at an offset within the body, including EOF.
    pub fn seek(&mut self, position: u64) -> Result<()> {
        if position > self.info.size {
            return Err(Error::InvalidObject);
        }
        self.position = position;
        Ok(())
    }

    /// Read one bounded chunk. Empty scratch is an error unless at EOF. Early
    /// EOF or a provider reporting more bytes than requested is `InvalidObject`.
    /// On error the position is unchanged, so a stable provider can be retried.
    pub fn read(&mut self, storage: &mut [u8]) -> Result<usize> {
        if self.remaining() == 0 {
            return Ok(0);
        }
        if storage.is_empty() {
            return Err(Error::BufferTooSmall);
        }
        let capacity = self.remaining().min(storage.len() as u64) as usize;
        let length =
            self.repository
                .read_object(self.index, self.position, &mut storage[..capacity])?;
        if length == 0 || length > capacity {
            return Err(Error::InvalidObject);
        }
        self.position += length as u64;
        Ok(length)
    }
}

/// Resumable Git object hashing. Retain this value between `step` calls; it owns
/// a cursor and SHA-1 state but no scratch. `size_of::<ObjectHasher<'_, R>>()`
/// gives the exact inline state budget for a target. IDs include kind and size.
pub struct ObjectHasher<'repo, R: Repository + ?Sized> {
    reader: ObjectReader<'repo, R>,
    hash: Sha1,
}

impl<'repo, R: Repository + ?Sized> ObjectHasher<'repo, R> {
    pub fn new(repository: &'repo R, index: usize) -> Result<Self> {
        let reader = ObjectReader::new(repository, index)?;
        let mut hash = Sha1::new();
        let mut header = [0; 32];
        hash.update(reader.info().header(&mut header)?);
        Ok(Self { reader, hash })
    }

    pub fn position(&self) -> u64 {
        self.reader.position()
    }

    /// Perform at most one provider read. `None` means more steps are needed;
    /// `Some(id)` means the declared body is complete. Completed calls repeat
    /// the same ID without rereading. Read errors leave the hash unchanged.
    pub fn step(&mut self, scratch: &mut [u8]) -> Result<Option<ObjectId>> {
        if self.reader.remaining() != 0 {
            let length = self.reader.read(scratch)?;
            self.hash.update(&scratch[..length]);
        }
        Ok((self.reader.remaining() == 0).then(|| ObjectId(self.hash.clone().finalize().into())))
    }

    /// Hash the remaining body synchronously using only the supplied scratch.
    pub fn finish(mut self, scratch: &mut [u8]) -> Result<ObjectId> {
        loop {
            if let Some(id) = self.step(scratch)? {
                return Ok(id);
            }
        }
    }
}
