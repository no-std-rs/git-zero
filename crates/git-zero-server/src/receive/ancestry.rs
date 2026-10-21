//! Bounded commit-ancestry decisions over a host's external graph catalog.
use git_zero_core::{Error, ObjectFormat, ObjectId, ObjectKind, Result};

/// The host queues distinct commits, starting at the proposed tip, and follows
/// only parent edges. Queue storage can be paged; no history lives in this value.
/// One visit budget covers every branch update in the receive operation.
pub struct Ancestry {
    format: ObjectFormat,
    target: Option<ObjectId>,
    remaining: u64,
}

impl Ancestry {
    pub const fn new(format: ObjectFormat, max_visits: u64) -> Self {
        Self {
            format,
            target: None,
            remaining: max_visits,
        }
    }

    /// Start searching for the old tip without replenishing the request budget.
    pub fn start(&mut self, old: ObjectId) -> Result<()> {
        self.target = None;
        if self.remaining == 0 {
            return Err(Error::LimitExceeded);
        }
        if old.format() != self.format || old.is_zero() {
            return Err(Error::InvalidObjectId);
        }
        self.target = Some(old);
        Ok(())
    }

    /// Return true only when a visited commit is the old tip. Exhausting the
    /// host's queue without a match means the update is not a fast-forward.
    pub fn visit(&mut self, id: ObjectId, kind: ObjectKind) -> Result<bool> {
        let target = self.target.ok_or(Error::InvalidRequest)?;
        if self.remaining == 0 {
            return Err(Error::LimitExceeded);
        }
        if id.format() != self.format || id.is_zero() {
            return Err(Error::InvalidObjectId);
        }
        if kind != ObjectKind::Commit {
            return Err(Error::InvalidObject);
        }
        self.remaining -= 1;
        let found = target == id;
        if found {
            self.target = None;
        }
        Ok(found)
    }
}
