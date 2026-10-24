//! A transport-silent handoff between selection and asynchronous pack storage.
//!
//! The host owns this value and its caller buffers across awaits. Neither the
//! request body nor the temporary validation index is borrowed after preparation.
//! Keep the authorized repository and selected/control buffers immutable until
//! attachment or cancellation. Independent requests require independent storage.
use super::{Body, ResponseEncoder};
use git_zero_core::{
    Error, ObjectId, Repository, Result,
    cache::selected::{Key, Selection},
    pack,
};

/// A prepared response whose raw pack has not yet been exposed to transport.
/// Only raw PACK bytes may be persisted; request-specific ACKs, boundaries and
/// sideband framing stay in the retained response encoder.
pub struct PreparedResponse<'a> {
    encoder: ResponseEncoder<'a>,
    pack: Option<pack::Encoder<'a>>,
    key: Option<Key>,
}

impl<'a> PreparedResponse<'a> {
    /// Consume a freshly prepared, unread response. Authorization, selection,
    /// graph limits and control-buffer admission must already have succeeded.
    /// A response without a pack (including negotiation/probes) has no cache key.
    pub fn new(
        mut encoder: ResponseEncoder<'a>,
        repository: &impl Repository,
        ids: &mut [ObjectId],
    ) -> Result<Self> {
        if encoder.started || encoder.cancelled {
            return Err(Error::InvalidRequest);
        }
        let (pack, key) = match core::mem::replace(&mut encoder.body, Body::Buffered) {
            Body::Pack(pack) => {
                let selection = pack.selection().map_or(Selection::All, Selection::Indices);
                let key = Key::derive(repository, selection, ids)?;
                (Some(pack), Some(key))
            }
            body => {
                encoder.body = body;
                (None, None)
            }
        };
        Ok(Self { encoder, pack, key })
    }

    pub fn key(&self) -> Option<Key> {
        self.key
    }

    pub fn response(&self) -> crate::Response {
        self.encoder.response()
    }

    /// Generate raw bytes on a miss without exposing any HTTP/protocol output.
    /// Pulls remain bounded by `output`; zero denotes complete PACK EOF. The
    /// immutable repository must be the one used to prepare this response.
    /// Discard staging on error. A fresh preparation is needed to restart it.
    pub fn read_pack(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        self.pack
            .as_mut()
            .ok_or(Error::InvalidRequest)?
            .read(repository, output)
    }

    /// Verify framing, checksums, canonical IDs and the exact selected set before
    /// returning an ordinary response cursor. Works for hits and generated misses.
    /// Cache bytes are borrowed and must remain immutable through transport EOF.
    /// Publication must be awaited separately before exposing the returned cursor.
    pub fn attach(
        mut self,
        bytes: &'a [u8],
        ids: &mut [ObjectId],
        byte_limit: usize,
    ) -> Result<ResponseEncoder<'a>> {
        self.key
            .ok_or(Error::InvalidRequest)?
            .verify_pack(bytes, ids, byte_limit)?;
        self.encoder.body = Body::Cached { bytes, position: 0 };
        Ok(self.encoder)
    }

    /// Finish a non-pack response without consulting storage. Pack responses
    /// require authenticated attachment, even after complete miss generation.
    pub fn without_pack(self) -> Result<ResponseEncoder<'a>> {
        if self.key.is_some() {
            return Err(Error::InvalidRequest);
        }
        Ok(self.encoder)
    }
}
