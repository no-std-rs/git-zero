//! Allocation-free streaming zlib encoding and inflation (RFC 1950/1951).
//! The encoder uses stored blocks; the decoder also accepts fixed and dynamic
//! Huffman blocks, with caller-owned history and explicit expansion budgets.
//! The decoder is implemented from [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950)
//! and [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951).
//!
//! ```
//! # #[cfg(feature = "read")] {
//! use git_zero_core::{io::{SliceWriter, Write}, zlib::{Inflater, InflateLimits, InflateStatus, WINDOW_SIZE}};
//! # fn example() -> git_zero_core::Result<()> {
//! let mut input: &[u8] = &[120, 156, 203, 72, 205, 201, 201, 7, 0, 6, 44, 2, 21];
//! let mut window = [0; WINDOW_SIZE];
//! let limits = InflateLimits { max_output: 5, max_ratio: 16, ratio_slack: 0 };
//! let mut decoder = Inflater::new(&mut window, limits);
//! let mut scratch = [0; 2];
//! let mut storage = [0; 5];
//! let mut staged = SliceWriter::new(&mut storage);
//! loop {
//!     let progress = decoder.update(input, &mut scratch, true)?;
//!     staged.write_all(&scratch[..progress.written])?;
//!     input = &input[progress.consumed..];
//!     if progress.status == InflateStatus::Finished { break; }
//! }
//! assert_eq!(staged.as_bytes(), b"hello");
//! assert!(input.is_empty());
//! # Ok(()) }
//! # example().unwrap();
//! # }
//! ```
mod adler;
#[cfg(feature = "read")]
mod inflate;
#[cfg(feature = "read")]
pub use inflate::{InflateLimits, InflateProgress, InflateStatus, Inflater, WINDOW_SIZE};

#[cfg(feature = "write")]
mod encode;
#[cfg(feature = "write")]
pub use encode::write_parts;
#[cfg(feature = "write")]
pub(crate) use encode::write_reader;
