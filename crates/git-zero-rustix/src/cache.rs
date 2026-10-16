//! Synchronous selected-pack cache in a caller-owned directory and byte buffer.
//!
//! Authorize and select before every lookup using `cache::selected::write`.
//! Choose a private directory for each authorization namespace. Published files
//! are replaced atomically, never modified in place. Directory durability and
//! distributed producer coordination belong to the host.

use core::ffi::CStr;
use git_zero::{
    Error, Result,
    cache::selected::{Key, PackCache, Statistics},
};
use rustix::{
    fd::{AsFd, BorrowedFd},
    fs::{self, AtFlags, Mode, OFlags},
    io::{self, Errno},
};

/// One bounded resident buffer backed by immutable cache files. Each `get`
/// reloads its file so eviction and corrupt entries remain visible. The core
/// selected writer validates every returned pack before output. Oversized files
/// fail before reading into the buffer; no pack-sized allocation or mapping is
/// performed here. A host may open independent instances for concurrent readers.
pub struct DiskCache<'directory, 'buffer> {
    directory: BorrowedFd<'directory>,
    buffer: &'buffer mut [u8],
    nonce: [u8; 16],
    sequence: u64,
    enabled: bool,
    statistics: Statistics,
}

impl<'directory, 'buffer> DiskCache<'directory, 'buffer> {
    /// Supply a trusted directory descriptor, fixed resident bytes, and a
    /// host-chosen nonce unique among concurrent producers in that directory.
    /// Exclusive staging-file creation safely rejects nonce/name collisions.
    pub fn new(
        directory: BorrowedFd<'directory>,
        buffer: &'buffer mut [u8],
        nonce: [u8; 16],
    ) -> Self {
        Self {
            directory,
            buffer,
            nonce,
            sequence: 0,
            enabled: true,
            statistics: Statistics::default(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn statistics(&self) -> Statistics {
        self.statistics
    }

    /// Unlink only this exact key. Readers that already copied a verified pack
    /// remain independent. Missing entries return false. No directory scan or
    /// automatic eviction of other authorization namespaces is performed.
    pub fn evict(&mut self, key: Key) -> Result<bool> {
        let name = key_name(key);
        match fs::unlinkat(self.directory, cstr(&name), AtFlags::empty()) {
            Ok(()) => {
                self.statistics.evictions = self.statistics.evictions.saturating_add(1);
                Ok(true)
            }
            Err(Errno::NOENT) => Ok(false),
            Err(_) => Err(Error::Backend),
        }
    }

    fn publish(&mut self, key: Key, bytes: &[u8]) -> Result<()> {
        // Check every recoverable condition before publication. In particular,
        // report directory durability separately: a failed post-rename fsync
        // could not promise preservation of the previous cache entry.
        if bytes.len() > self.buffer.len() {
            return Err(Error::BufferTooSmall);
        }
        let sequence = self.sequence;
        self.sequence = sequence.checked_add(1).ok_or(Error::LimitExceeded)?;
        let name = temporary_name(self.nonce, sequence);
        let file = fs::openat(
            self.directory,
            cstr(&name),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Backend)?;
        let mut staging = Staging {
            directory: self.directory,
            name: &name,
            armed: true,
        };
        write_file(&file, bytes)?;
        fs::fsync(&file).map_err(|_| Error::Backend)?;
        let target = key_name(key);
        // Last fallible publication operation: failed rename preserves the old
        // target. Successful rename exposes a complete already-synced file.
        fs::renameat(self.directory, cstr(&name), self.directory, cstr(&target))
            .map_err(|_| Error::Backend)?;
        staging.armed = false;
        Ok(())
    }
}

impl PackCache for DiskCache<'_, '_> {
    fn get(&mut self, key: Key) -> Result<Option<&[u8]>> {
        if !self.enabled {
            self.statistics.misses = self.statistics.misses.saturating_add(1);
            return Ok(None);
        }
        let name = key_name(key);
        let file = match fs::openat(
            self.directory,
            cstr(&name),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(file) => file,
            Err(Errno::NOENT) => {
                self.statistics.misses = self.statistics.misses.saturating_add(1);
                return Ok(None);
            }
            Err(_) => return Err(Error::Backend),
        };
        let length = crate::storage::file_size(&file)?;
        let length = usize::try_from(length).map_err(|_| Error::LimitExceeded)?;
        let bytes = self.buffer.get_mut(..length).ok_or(Error::BufferTooSmall)?;
        read_file(&file, bytes)?;
        self.statistics.hits = self.statistics.hits.saturating_add(1);
        Ok(Some(bytes))
    }

    fn put(&mut self, key: Key, bytes: &[u8]) -> Result<()> {
        if self.enabled {
            self.publish(key, bytes)?;
            self.statistics.insertions = self.statistics.insertions.saturating_add(1);
        }
        Ok(())
    }
}

fn read_file(file: &impl AsFd, bytes: &mut [u8]) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        match io::pread(file, &mut bytes[offset..], offset as u64) {
            Ok(0) => return Err(Error::CorruptPack),
            Ok(count) => offset += count,
            Err(Errno::INTR) => continue,
            Err(_) => return Err(Error::Backend),
        }
    }
    // Catch a file that grew while being copied. Afterwards the private buffer
    // is immutable for the core verifier regardless of later file replacement.
    loop {
        match io::pread(file, &mut [0], offset as u64) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(Error::CorruptPack),
            Err(Errno::INTR) => continue,
            Err(_) => return Err(Error::Backend),
        }
    }
}

fn write_file(file: &impl AsFd, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        match io::write(file, bytes) {
            Ok(0) => return Err(Error::Backend),
            Ok(count) => bytes = &bytes[count..],
            Err(Errno::INTR) => continue,
            Err(_) => return Err(Error::Backend),
        }
    }
    Ok(())
}

struct Staging<'directory, 'name> {
    directory: BorrowedFd<'directory>,
    name: &'name [u8; 59],
    armed: bool,
}
impl Drop for Staging<'_, '_> {
    fn drop(&mut self) {
        // Before rename this removes the incomplete private file. Afterwards
        // the staging name is absent. Never unlink another producer's file.
        if self.armed {
            let _ = fs::unlinkat(self.directory, cstr(self.name), AtFlags::empty());
        }
    }
}

fn key_name(key: Key) -> [u8; 102] {
    let mut name = [0; 102];
    hexadecimal(key.as_bytes(), &mut name[..96]);
    name[96..101].copy_from_slice(b".pack");
    name
}
fn temporary_name(nonce: [u8; 16], sequence: u64) -> [u8; 59] {
    let mut name = [0; 59];
    name[..5].copy_from_slice(b".tmp-");
    hexadecimal(&nonce, &mut name[5..37]);
    hexadecimal(&sequence.to_be_bytes(), &mut name[37..53]);
    name[53..58].copy_from_slice(b".pack");
    name
}
fn hexadecimal(bytes: &[u8], output: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (byte, pair) in bytes.iter().zip(output.chunks_exact_mut(2)) {
        pair[0] = HEX[(byte >> 4) as usize];
        pair[1] = HEX[(byte & 15) as usize];
    }
}
fn cstr(bytes: &[u8]) -> &CStr {
    CStr::from_bytes_with_nul(bytes).unwrap()
}
