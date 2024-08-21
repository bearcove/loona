use std::error::Error as StdError;
use std::fmt;

use crate::h2::types::H2ConnectionError;

pub type BoxError = Box<dyn StdError + 'static>;

/// Any error that can occur when servicing a connection
#[non_exhaustive]
#[derive(Debug)]
pub enum ServeError<DriverError> {
    /// An error occurred while writing to the downstream
    DownstreamWrite(std::io::Error),

    /// The server driver errored out
    Driver(DriverError),

    /// HTTP/1.1 response body was not drained by response
    /// handler before the client closed the connection
    ResponseHandlerBodyNotDrained,

    /// An error occurred while handling an HTTP/2 connection
    H2ConnectionError(H2ConnectionError),

    /// An error occurred during memory allocation
    Alloc(buffet::bufpool::Error),
}

impl<DriverError> From<H2ConnectionError> for ServeError<DriverError> {
    fn from(error: H2ConnectionError) -> Self {
        ServeError::H2ConnectionError(error)
    }
}

impl<DriverError> fmt::Display for ServeError<DriverError>
where
    DriverError: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<DriverError> StdError for ServeError<DriverError>
where
    DriverError: StdError + 'static,
{
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::DownstreamWrite(e) => Some(e),
            Self::Driver(e) => Some(e),
            Self::Alloc(e) => Some(e),
            _ => None,
        }
    }
}

impl<DriverError> From<buffet::bufpool::Error> for ServeError<DriverError> {
    fn from(error: buffet::bufpool::Error) -> Self {
        ServeError::Alloc(error)
    }
}
