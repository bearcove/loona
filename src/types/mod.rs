use std::fmt::{self, Debug};

use tracing::debug;

use crate::{IoChunk, Roll};

mod headers;
pub use headers::*;

mod method;
pub use method::*;

/// An HTTP request
pub struct Request {
    pub method: Method,

    /// Requested entity
    pub path: Roll,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    /// Request headers
    pub headers: Headers,
}

impl Request {
    pub(crate) fn debug_print(&self) {
        debug!(method = %self.method, path = %self.path.to_string_lossy(), version = %self.version, "got request");
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
    pub reason: Roll,

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
    Chunk(IoChunk),

    /// The body finished, and it matched the announced content-length,
    /// or we were using a framed protocol
    Done {
        trailers: Option<Box<Headers>>,
    },
}

#[derive(Debug, thiserror::Error)]
pub struct BodyError {
    reason: BodyErrorReason,
    context: Option<Box<dyn Debug + Send + Sync>>,
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "body error: {:?}", self.reason)?;
        if let Some(context) = &self.context {
            write!(f, " ({context:?})")
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyErrorReason {
    // next_chunk() was called after an error was returned
    CalledNextChunkAfterError,

    // while doing chunked transfer-encoding, we expected a chunk size
    // but the connection closed/errored in some way
    ClosedWhileReadingChunkSize,

    // while doing chunked transfer-encoding, we expected a chunk size,
    // but what we read wasn't a hex number followed by CRLF
    InvalidChunkSize,

    // while doing chunked transfer-encoding, the connection was closed
    // in the middle of reading a chunk's data
    ClosedWhileReadingChunkData,

    // while reading a content-length body, the connection was closed
    ClosedWhileReadingContentLength,

    // while doing chunked transfer-encoding, there was a read error
    // in the middle of reading a chunk's data
    ErrorWhileReadingChunkData,

    // while doing chunked transfer-encoding, the connection was closed
    // in the middle of reading the
    ClosedWhileReadingChunkTerminator,

    // while doing chunked transfer-encoding, we read the chunk size,
    // then that much data, but then encountered something other than
    // a CRLF
    InvalidChunkTerminator,
}

impl BodyErrorReason {
    pub fn as_err(self) -> BodyError {
        BodyError {
            reason: self,
            context: None,
        }
    }

    pub fn with_cx(self, context: impl Debug + Send + Sync + 'static) -> BodyError {
        BodyError {
            reason: self,
            context: Some(Box::new(context)),
        }
    }
}

pub trait Body: Debug
where
    Self: Sized,
{
    fn content_len(&self) -> Option<u64>;
    fn eof(&self) -> bool;
    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk>;
}

impl Body for () {
    fn content_len(&self) -> Option<u64> {
        Some(0)
    }

    fn eof(&self) -> bool {
        true
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        Ok(BodyChunk::Done { trailers: None })
    }
}
