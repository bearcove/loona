mod headers;
pub use headers::*;

use crate::bufpool::AggSlice;

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
