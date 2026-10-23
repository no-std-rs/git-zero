//! Allocation-free Git objects, repositories and wire primitives.
//!
//! `read` enables inflation, pack decoding and repository validation.
//! `write` enables snapshots, pack/zlib encoding and pack caching.
//!
//! Storage and I/O belong to the caller. There is no `std`, `alloc`, allocator,
//! global state, filesystem, or socket dependency in this crate.
#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "write")]
pub mod cache;
#[cfg(feature = "read")]
pub mod checkout;
pub mod http;
pub mod io;
#[cfg(any(feature = "read", feature = "write"))]
mod names;
pub mod object;
#[cfg(any(feature = "read", feature = "write"))]
pub mod pack;
pub mod pktline;
pub mod reader;
pub mod repository;
#[cfg(feature = "write")]
pub mod snapshot;
#[cfg(feature = "read")]
pub mod validation;
#[cfg(any(feature = "read", feature = "write"))]
pub mod zlib;

pub use object::{HexId, Object, ObjectDigest, ObjectFormat, ObjectId, ObjectInfo, ObjectKind};
pub use reader::{ObjectHasher, ObjectReader};
pub use repository::{
    EmptyRepository, Head, IndexedRepository, ObjectMetadata, RefRepository, Reference, Repository,
};
#[cfg(feature = "write")]
pub use snapshot::{
    CommitIdentity, CommitOptions, Entry, EntryKind, File, Snapshot, SnapshotBuilder,
};
#[cfg(feature = "read")]
pub use validation::{
    RevisionLimits, RevisionOptions, RevisionState, SelectionOptions, ShallowChange,
    ValidatedRepository, ValidationMetadata,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    BufferTooSmall,
    InvalidObjectId,
    InvalidObject,
    InvalidPacket,
    InvalidRequest,
    InvalidFileName,
    UnsortedFiles,
    InvalidReference,
    ObjectNotFound,
    Unsupported,
    Backend,
    CorruptPack,
    InvalidZlib,
    LimitExceeded,
}

pub type Result<T> = core::result::Result<T, Error>;

mod fetch;
pub use fetch::{BlobFilter, FetchOptions, HistoryPolicy};
