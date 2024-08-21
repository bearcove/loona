use std::error::Error as StdError;
use std::fmt;

use crate::h2::types::H2ConnectionError;

pub type BoxError = Box<dyn StdError>;

/// Any error that can occur when servicing a connection
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ServeError<DriverError> {
    /// An error occurred while writing to the downstream
    #[error("Error writing to downstream: {0}")]
    DownstreamWrite(#[from] std::io::Error),

    /// The server driver errored out
    #[error("Server driver error: {0:?}")]
    Driver(DriverError),

    /// HTTP/1.1 response body was not drained by response
    /// handler before the client closed the connection
    #[error("HTTP/1.1 response body was not drained before client closed connection")]
    ResponseHandlerBodyNotDrained,

    /// An error occurred while handling an HTTP/2 connection
    #[error("HTTP/2 connection error: {0}")]
    H2ConnectionError(#[from] H2ConnectionError),

    /// An error occurred during memory allocation
    #[error("Memory allocation error: {0}")]
    Alloc(#[from] buffet::bufpool::Error),
}

impl<DriverError> AsRef<dyn StdError> for ServeError<DriverError>
where
    DriverError: AsRef<dyn StdError> + fmt::Debug + 'static,
{
    fn as_ref(&self) -> &(dyn StdError + 'static) {
        self
    }
}

pub struct NeverError;

impl AsRef<dyn StdError> for NeverError {
    fn as_ref(&self) -> &(dyn StdError + 'static) {
        self
    }
}

impl fmt::Debug for NeverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NeverError")
    }
}

impl fmt::Display for NeverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NeverError")
    }
}

impl StdError for NeverError {}
