use std::fmt::{self, Debug};

use http::{StatusCode, Uri, Version};
use tracing::debug;

use buffet::Piece;

mod headers;
pub use headers::*;

mod method;
pub use method::*;

use crate::{error::NeverError, util::ReadAndParseError};

/// An HTTP request
#[derive(Clone)]
pub struct Request {
    pub method: Method,

    /// Requested entity
    pub uri: Uri,

    /// The HTTP version used
    pub version: Version,

    /// Request headers
    pub headers: Headers,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            method: Method::Get,
            uri: "/".parse().unwrap(),
            version: Version::HTTP_11,
            headers: Default::default(),
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("version", &self.version)
            .finish()?;

        for (name, value) in &self.headers {
            debug!(%name, value = ?std::str::from_utf8(value), "header");
        }

        Ok(())
    }
}

/// An HTTP response
#[derive(Clone)]
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
            debug!(%name, value = ?std::str::from_utf8(value), "got header");
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
#[non_exhaustive]
pub enum BodyError {
    /// next_chunk() was called after an error was returned
    #[error("next_chunk() was called after an error was returned")]
    CalledNextChunkAfterError,

    /// while doing chunked transfer-encoding, we expected a chunk size
    /// but the connection closed/errored in some way
    #[error("connection closed while reading chunk size")]
    ClosedWhileReadingChunkSize,

    /// while doing chunked transfer-encoding, we expected a chunk size,
    /// but what we read wasn't a hex number followed by CRLF
    #[error("invalid chunk size")]
    InvalidChunkSize,

    /// while doing chunked transfer-encoding, the connection was closed
    /// in the middle of reading a chunk's data
    #[error("connection closed while reading chunk data")]
    ClosedWhileReadingChunkData,

    /// while reading a content-length body, the connection was closed
    #[error("connection closed while reading content-length body")]
    ClosedWhileReadingContentLength,

    /// while doing chunked transfer-encoding, there was a read error
    /// in the middle of reading a chunk's data
    #[error("error while reading chunk data: {0}")]
    ErrorWhileReadingChunkData(std::io::Error),

    /// while doing chunked transfer-encoding, the connection was closed
    /// in the middle of reading the
    #[error("connection closed while reading chunk terminator")]
    ClosedWhileReadingChunkTerminator,

    /// while doing chunked transfer-encoding, we read the chunk size,
    /// then that much data, but then encountered something other than
    /// a CRLF
    #[error("invalid chunk terminator: {0}")]
    InvalidChunkTerminator(#[from] ReadAndParseError),

    /// `write_chunk` was called but no content-length was announced, and
    /// no chunked transfer-encoding was announced
    #[error("write_chunk called when no body was expected")]
    CalledWriteBodyChunkWhenNoBodyWasExpected,

    /// Allocation failed
    #[error("allocation failed: {0}")]
    Alloc(#[from] buffet::bufpool::Error),

    /// I/O error while writing
    #[error("I/O error while writing: {0}")]
    WriteError(std::io::Error),
}

impl AsRef<dyn std::error::Error> for BodyError {
    fn as_ref(&self) -> &(dyn std::error::Error + 'static) {
        self
    }
}

#[allow(async_fn_in_trait)] // we never require Send
pub trait Body: Debug
where
    Self: Sized,
{
    type Error: AsRef<dyn std::error::Error + 'static>;

    fn content_len(&self) -> Option<u64>;
    fn eof(&self) -> bool;
    async fn next_chunk(&mut self) -> Result<BodyChunk, Self::Error>;
}

impl Body for () {
    type Error = NeverError;

    fn content_len(&self) -> Option<u64> {
        Some(0)
    }

    fn eof(&self) -> bool {
        true
    }

    async fn next_chunk(&mut self) -> Result<BodyChunk, Self::Error> {
        Ok(BodyChunk::Done { trailers: None })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    /// HTTP/1.1 only: The request we handled had a `connection: close` header
    ClientRequestedConnectionClose,

    /// HTTP/1.1 only: The request we handled had a `connection: close` header
    ServerRequestedConnectionClose,

    // Client closed connection before sending a second request
    /// (without requesting connection close)
    ClientClosedConnectionBetweenRequests,

    /// HTTP/1.1 only: Client didn't speak HTTP/1.1 (missing/invalid request
    /// line)
    ClientDidntSpeakHttp11,

    // We refused to service a request because it was too large. Because it's HTTP/1.1,
    /// we had to close the entire connection.
    RequestHeadersTooLargeOnHttp1Conn,

    /// HTTP/2 only: Client didn't speak HTTP/2 (missing/invalid request line)
    ClientDidntSpeakHttp2,

    /// HTTP/2 only: Client sent a GOAWAY frame, and we've sent a response to
    /// the client
    SuccessfulHttp2GracefulShutdown,
}
