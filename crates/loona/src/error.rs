use std::error::Error as StdError;
use std::fmt;

pub type BoxError = Box<dyn StdError + 'static>;

/// Any error that can occur when servicing a connection
#[non_exhaustive]
#[derive(Debug)]
pub enum ServeError<HE> {
    /// An error occurred while writing to the downstream
    DownstreamWrite(std::io::Error),

    /// A response handler errored out
    ResponseHandler(HE),

    /// HTTP/1.1 response body was not drained by response
    /// handler before the client closed the connection
    ResponseHandlerBodyNotDrained,
}
impl<HE> fmt::Display for ServeError<HE>
where
    HE: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<HE> StdError for ServeError<HE>
where
    HE: StdError + 'static,
{
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::DownstreamWrite(e) => Some(e),
            Self::ResponseHandler(e) => Some(e),
            _ => None,
        }
    }
}
