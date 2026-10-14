//! Allocation-free Git objects and HTTP client/server building blocks.
//!
//! Select `read`, `write`, `client` and `server` independently. The defaults
//! preserve the complete API. All library operations remain `no_std`/no-alloc.
#![no_std]
#![forbid(unsafe_code)]

pub use git_zero_core::*;

#[cfg(feature = "client")]
pub use git_zero_client as client;

/// HTTP values and, with `server`, repository routing.
pub mod http {
    pub use git_zero_core::http::*;
    #[cfg(feature = "server")]
    pub use git_zero_server::*;
}
