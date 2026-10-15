//! One concrete provider type keeps both ABI paths on the same core encoders.
use crate::raw::Imported;
use git_zero::{Head, Object, ObjectFormat, ObjectId, Repository, Snapshot};

pub(crate) enum WorkerRepository<'a> {
    Snapshot(&'a Snapshot<'a>),
    Imported(&'a Imported),
}

impl Repository for WorkerRepository<'_> {
    fn object_format(&self) -> ObjectFormat {
        match self {
            Self::Snapshot(value) => value.object_format(),
            Self::Imported(value) => value.object_format(),
        }
    }
    fn head(&self) -> ObjectId {
        match self {
            Self::Snapshot(value) => value.head(),
            Self::Imported(value) => value.head(),
        }
    }
    fn head_state(&self) -> Head<'_> {
        match self {
            Self::Snapshot(value) => value.head_state(),
            Self::Imported(value) => value.head_state(),
        }
    }
    fn head_ref(&self) -> &str {
        match self {
            Self::Snapshot(value) => value.head_ref(),
            Self::Imported(value) => value.head_ref(),
        }
    }
    fn object_count(&self) -> usize {
        match self {
            Self::Snapshot(value) => value.object_count(),
            Self::Imported(value) => value.object_count(),
        }
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        match self {
            Self::Snapshot(value) => value.object(index),
            Self::Imported(value) => value.object(index),
        }
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        match self {
            Self::Snapshot(value) => value.object_id(index),
            Self::Imported(value) => value.object_id(index),
        }
    }
    fn reference_count(&self) -> usize {
        match self {
            Self::Snapshot(value) => value.reference_count(),
            Self::Imported(value) => value.reference_count(),
        }
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        match self {
            Self::Snapshot(value) => value.reference_name(index),
            Self::Imported(value) => value.reference_name(index),
        }
    }
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        match self {
            Self::Snapshot(value) => value.reference_id(index),
            Self::Imported(value) => value.reference_id(index),
        }
    }
    fn reference_symbolic_target(&self, index: usize) -> Option<&str> {
        match self {
            Self::Snapshot(value) => value.reference_symbolic_target(index),
            Self::Imported(value) => value.reference_symbolic_target(index),
        }
    }
    fn reference_peeled(&self, index: usize) -> Option<ObjectId> {
        match self {
            Self::Snapshot(value) => value.reference_peeled(index),
            Self::Imported(value) => value.reference_peeled(index),
        }
    }
}
