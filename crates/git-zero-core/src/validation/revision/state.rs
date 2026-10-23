//! Caller-owned revision flags and independent shallow-boundary changes.
use super::{ObjectId, ValidationMetadata};

pub(super) const EXCLUDED: u8 = 1;
pub(super) const CANDIDATE: u8 = 2;
pub(super) const PENDING: u8 = 4;
pub(super) const DONE: u8 = 8;
pub(super) const BORDER: u8 = 16;
pub(super) const UNSHALLOW: u8 = 32;
pub(super) const CLIENT_SHALLOW: u8 = 64;

/// Additional limits for an explicitly enabled revision-history request.
#[derive(Clone, Copy, Debug)]
pub struct RevisionLimits {
    /// Maximum exclusion arguments, counting duplicate arguments too.
    pub max_exclusions: usize,
}

/// Revision history, separate from absolute/relative depth. Zero is an active
/// unrestricted revision walk. HTTP resolves exclusion refs to the iterator
/// passed to `select_revision_fetch`; non-commit endpoints have no ancestry effect.
#[derive(Clone, Copy, Debug, Default)]
pub struct RevisionOptions {
    pub since: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShallowChange {
    Shallow(ObjectId),
    Unshallow(ObjectId),
}

/// One caller-owned byte per repository object. Existing metadata/storage layouts
/// remain unchanged. Recreate this descriptor for every request/retry; arrays are
/// reset by selection, and failures invalidate all results.
pub struct RevisionState<'a> {
    pub(super) states: &'a mut [u8],
    pub(super) limits: RevisionLimits,
    pub(super) count: usize,
}

impl<'a> RevisionState<'a> {
    pub fn new(states: &'a mut [u8], limits: RevisionLimits) -> Self {
        Self {
            states,
            limits,
            count: 0,
        }
    }

    pub fn max_exclusions(&self) -> usize {
        self.limits.max_exclusions
    }

    /// Changes may include commits absent from the returned PACK. Pass the same
    /// metadata work array used by the successful selection, without modifying it.
    pub fn changes<'b>(
        &'b self,
        objects: &'b [Option<ValidationMetadata>],
    ) -> impl Iterator<Item = ShallowChange> + 'b {
        self.states
            .iter()
            .zip(objects)
            .take(self.count)
            .filter_map(|(&state, object)| revision_change(state, object.as_ref()?.id))
    }
}

fn revision_change(state: u8, id: ObjectId) -> Option<ShallowChange> {
    if state & (BORDER | CLIENT_SHALLOW) == BORDER {
        Some(ShallowChange::Shallow(id))
    } else if state & UNSHALLOW != 0 {
        Some(ShallowChange::Unshallow(id))
    } else {
        None
    }
}

// Candidate membership is frozen before this pure transition classifies which
// existing client boundaries now have their parents available.
pub(super) fn mark_unshallow(states: &mut [u8]) {
    for state in states {
        if *state & (CLIENT_SHALLOW | CANDIDATE | BORDER) == (CLIENT_SHALLOW | CANDIDATE) {
            *state |= UNSHALLOW;
        }
    }
}
