//! Streaming strict-receive checks for a designated attributes blob.
use git_zero_core::{Error, Result};

/// Match Git's 100 MiB file bound and 2048-byte line bound. As in Git's
/// attributes fsck, NUL terminates line inspection, while the complete blob
/// length remains subject to the file bound and canonical authentication.
pub struct FsckAttributes {
    remaining: u64,
    line: u16,
    stopped: bool,
    failure: Option<Error>,
}

impl FsckAttributes {
    pub fn new(size: u64) -> Result<Self> {
        if size > 100 * 1024 * 1024 {
            return Err(Error::InvalidObject);
        }
        Ok(Self {
            remaining: size,
            line: 0,
            stopped: false,
            failure: None,
        })
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.feed(bytes);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or(Error::InvalidObject)?;
        if self.stopped {
            return Ok(());
        }
        for &byte in bytes {
            if byte == 0 {
                self.stopped = true;
                break;
            }
            if byte == b'\n' {
                self.line = 0;
            } else {
                self.line += 1;
                if self.line >= 2048 {
                    return Err(Error::InvalidObject);
                }
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(Error::InvalidObject)
        }
    }
}
