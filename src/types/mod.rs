use std::fmt::{self, Debug};

use http::{StatusCode, Version};
use tracing::debug;

use crate::{Piece, PieceStr};

mod headers;
pub use headers::*;

mod method;
pub use method::*;

/// An HTTP request
pub struct Request {
    pub method: Method,

    /// Requested entity
    pub path: PieceStr,

    /// The HTTP version used
    pub version: Version,

    /// Request headers
    pub headers: Headers,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            method: Method::Get,
            path: "/".into(),
            version: Version::HTTP_11,
            headers: Default::default(),
        }
    }
}

impl Request {
    pub(crate) fn debug_print(&self) {
        debug!(method = %self.method, path = %self.path, version = ?self.version, "got request");
        for (name, value) in &self.headers {
            debug!(%name, value = ?value.as_str(), "got header");
        }
    }
}

/// An HTTP response
pub struct Response {
    /// The 'b' in 'HTTP/1.b'
    pub version: Version,

    /// Status code (1xx-5xx)
    pub status: StatusCode,

    /// Response headers
    pub headers: Headers,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            version: Version::HTTP_11,
            status: StatusCode::OK,
            headers: Default::default(),
        }
    }
}

impl Response {
    pub(crate) fn debug_print(&self) {
        debug!(code = %self.status, version = ?self.version, "got response");
        for (name, value) in &self.headers {
            debug!(%name, value = ?value.as_str(), "got header");
        }
    }

    /// 204 and 304 responses must not have a body
    pub fn means_empty_body(&self) -> bool {
        matches!(
            self.status,
            StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
        )
    }
}

/// A body chunk
pub enum BodyChunk {
    Chunk(Piece),

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

    // `write_chunk` was called but no content-length was announced, and
    // no chunked transfer-encoding was announced
    CalledWriteBodyChunkWhenNoBodyWasExpected,
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
