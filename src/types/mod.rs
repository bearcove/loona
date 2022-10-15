mod headers;
pub use headers::*;

mod driver;
pub use driver::*;

use crate::bufpool::AggregateSlice;

/// An HTTP request
pub struct Request {
    pub method: AggregateSlice,

    /// Requested entity
    pub path: AggregateSlice,

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
    pub reason: AggregateSlice,

    /// Response headers
    pub headers: Headers,
}
