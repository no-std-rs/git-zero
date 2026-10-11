//! Allocation-free, transport-independent, read-only Git HTTP primitives.
//!
//! Storage and I/O belong to the caller. There is no `std`, `alloc`, allocator,
//! global state, filesystem, or socket dependency in this crate.
#![no_std]
#![forbid(unsafe_code)]

pub mod cache;
pub mod client;
pub mod http;
pub mod io;
pub mod object;
pub mod pack;
pub mod pktline;
pub mod reader;
pub mod repository;
pub mod snapshot;
pub mod zlib;

pub use object::{Object, ObjectId, ObjectInfo, ObjectKind};
pub use reader::{ObjectHasher, ObjectReader};
pub use repository::{IndexedRepository, ObjectMetadata, Repository};
pub use snapshot::{Entry, EntryKind, File, Snapshot};

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
}

pub type Result<T> = core::result::Result<T, Error>;
