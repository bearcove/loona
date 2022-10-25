mod headers;
pub use headers::*;
use tracing::debug;

use crate::bufpool::{AggSlice, Buf};

/// An HTTP request
pub struct Request {
    pub method: AggSlice,

    /// Requested entity
    pub path: AggSlice,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    /// Request headers
    pub headers: Headers,
}

impl Request {
    pub(crate) fn debug_print(&self) {
        debug!(method = %self.method.to_string_lossy(), path = %self.path.to_string_lossy(), version = %self.version, "got request");
        for h in &self.headers {
            debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
        }
    }
}

/// An HTTP response
pub struct Response {
    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    /// Status code (1xx-5xx)
    pub code: u16,

    /// Human-readable string following the status code
    pub reason: AggSlice,

    /// Response headers
    pub headers: Headers,
}

impl Response {
    pub(crate) fn debug_print(&self) {
        debug!(code = %self.code, reason = %self.reason.to_string_lossy(), version = %self.version, "got response");
        for h in &self.headers {
            debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
        }
    }
}

/// A body chunk
pub enum BodyChunk {
    Buf(Buf),
    AggSlice(AggSlice),
    Eof,
}

pub trait Body
where
    Self: Sized,
{
    fn content_len(&self) -> Option<u64>;
    fn eof(&self) -> bool;
    async fn next_chunk(self) -> eyre::Result<(Self, BodyChunk)>;
}

impl Body for () {
    fn content_len(&self) -> Option<u64> {
        Some(0)
    }

    fn eof(&self) -> bool {
        true
    }

    async fn next_chunk(self) -> eyre::Result<(Self, BodyChunk)> {
        Ok((self, BodyChunk::Eof))
    }
}
