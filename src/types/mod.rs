use smallvec::SmallVec;

use crate::{bufpool::aggregate::AggregateSlice, parse::h1::Header};

// TODO: this could/should be a multimap. http's multimap is neat but doesn't
// support `AggregateSlice`. The `HeaderName` type should probably have three
// variants:
//   WellKnown (TransferEncoding, Connection, etc.)
//   &'static [u8] (custom)
//   AggregateSlice (proxied)
pub type Headers = SmallVec<[Header; 32]>;

/// An HTTP request
pub struct Request {
    pub method: AggregateSlice,

    pub path: AggregateSlice,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    pub headers: Headers,
}

/// An HTTP response
pub struct Response {
    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    /// Status code (1xx-5xx)
    pub code: u16,

    pub reason: AggregateSlice,

    pub headers: Headers,
}
