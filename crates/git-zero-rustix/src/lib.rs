//! Allocation-free rustix file storage and blocking IPv4 HTTP/1.0 adapters.
#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "storage")]
pub mod storage;
#[cfg(feature = "storage")]
pub use storage::{FileObject, FileRepository};

#[cfg(any(feature = "client", feature = "server"))]
mod network;
#[cfg(any(feature = "client", feature = "server"))]
pub use network::*;
