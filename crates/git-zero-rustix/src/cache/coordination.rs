//! Cooperative cold-miss coordination through nonblocking advisory file locks.
//!
//! All producers for a namespace must use the same stable lock files. Do not
//! unlink or replace `.lock` entries while any producer can use the directory:
//! doing so would allow independent locks on different inodes for one key.
//! Hosts choose retry, cancellation and waiting policy; no request blocks here.
//! Filesystem and mount settings determine the scope of advisory locking.
//! See <https://man7.org/linux/man-pages/man2/flock.2.html>.

use super::{DiskCache, cstr, key_name};
use git_zero::{Error, Result, cache::selected::Key};
use rustix::{
    fd::OwnedFd,
    fs::{self, FlockOperation, Mode, OFlags},
    io::Errno,
};

/// An exclusive producer guard. Dropping it explicitly unlocks and closes its
/// descriptor. The empty lock file remains for future producers.
#[must_use = "keep the guard alive while checking and publishing the cache entry"]
pub struct Lease {
    file: OwnedFd,
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Release before closing even if a concurrent host fork temporarily
        // inherited the descriptor before exec applied close-on-exec.
        let _ = fs::flock(&self.file, FlockOperation::Unlock);
    }
}

impl DiskCache<'_, '_> {
    /// Attempt one nonblocking exclusive lock for a selected key. `None` means
    /// the cache is disabled or another cooperative producer holds the lock.
    /// After acquisition, recheck the cache before generating a pack: another
    /// producer may have completed while this request waited to retry.
    /// Authorization and selection remain mandatory before every lookup.
    ///
    /// Guards own independent close-on-exec descriptors and do not borrow the
    /// backend, so the host can call the selected writer while holding one.
    /// Eviction affects `.pack` files only and never removes the lock identity.
    pub fn try_lease(&self, key: Key) -> Result<Option<Lease>> {
        if !self.enabled {
            return Ok(None);
        }
        let mut name = key_name(key);
        name[96..101].copy_from_slice(b".lock");
        let file = fs::openat(
            self.directory,
            cstr(&name),
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| Error::Backend)?;
        if crate::storage::file_size(&file)? != 0 {
            return Err(Error::InvalidObject);
        }
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Lease { file })),
            Err(Errno::WOULDBLOCK) => Ok(None),
            Err(_) => Err(Error::Backend),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use rustix::fd::AsFd;

    #[test]
    fn dropping_a_guard_unlocks_even_while_a_duplicate_descriptor_remains() {
        let path = std::env::temp_dir().join(std::format!(
            "git-zero-coordination-dup-{}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        let directory = fs::openat(
            fs::CWD,
            path.as_os_str().as_encoded_bytes(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let repository = git_zero::EmptyRepository::new("refs/heads/main").unwrap();
        let key = Key::derive(
            &repository,
            git_zero::cache::selected::Selection::All,
            &mut [],
        )
        .unwrap();
        let mut resident = [0; 1];
        let cache = DiskCache::new(directory.as_fd(), &mut resident, [7; 16]);
        let guard = cache.try_lease(key).unwrap().unwrap();
        assert!(
            rustix::io::fcntl_getfd(&guard.file)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        // A descriptor inherited between fork and exec has the same open file
        // description. A safe duplicate deterministically reproduces its hold.
        let inherited = rustix::io::dup(&guard.file).unwrap();
        assert!(cache.try_lease(key).unwrap().is_none());
        drop(guard);
        let retry = cache.try_lease(key).unwrap().unwrap();
        drop(retry);
        drop(inherited);
        drop(directory);
        std::fs::remove_dir_all(path).unwrap();
    }
}
