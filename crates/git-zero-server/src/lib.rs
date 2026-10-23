//! Git HTTP backend, upload-pack and opt-in receive-pack building blocks.
//! The host supplies repository storage, authentication and HTTP transfer framing.
#![no_std]
#![forbid(unsafe_code)]

pub mod dumb;
mod http_backend;
pub mod receive;
mod route;
#[cfg(feature = "read")]
pub mod stored;
pub mod stream;
mod upload_pack;
mod v2;

pub use git_zero_core::http::{Request, Response};
pub use http_backend::{HEAD, serve, serve_with_pack, serve_with_scratch};
pub use upload_pack::{PackOutput, WantOptions, WantPolicy, write_upload_error};
#[cfg(feature = "read")]
pub use upload_pack::{
    RevisionBuffers, RevisionStorage, SelectionBuffers, SelectionStorage, serve_with_revisions,
    serve_with_selected_pack, serve_with_selected_pack_policy, serve_with_selection,
    serve_with_selection_policy,
};
