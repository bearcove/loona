use std::net::SocketAddr;

use smallvec::SmallVec;

use crate::{bufpool::aggregate::AggregateSlice, parse::h1::Header};

#[derive(Default)]
pub struct Headers {
    // TODO: this could/should be a multimap. http's multimap is neat but doesn't
    // support `AggregateSlice`. The `HeaderName` type should probably have three
    // variants:
    //   WellKnown (TransferEncoding, Connection, etc.)
    //   &'static [u8] (custom)
    //   AggregateSlice (proxied)
    headers: SmallVec<[Header; 32]>,
}

impl Headers {
    pub fn push(&mut self, header: Header) {
        self.headers.push(header);
    }
}

impl<'a> IntoIterator for &'a Headers {
    type Item = &'a Header;
    type IntoIter = std::slice::Iter<'a, Header>;

    fn into_iter(self) -> Self::IntoIter {
        self.headers.iter()
    }
}

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

/// A connection driver maintains per-connection state and steers requests
pub trait ConnectionDriver {
    type RequestDriver: RequestDriver;

    fn steer_request(&self, req: &Request) -> eyre::Result<Self::RequestDriver>;
}

/// A request driver knows where a request should go, how to modify headers, etc.
pub trait RequestDriver {
    /// Determine which upstream address to use for this request
    fn upstream_addr(&self) -> eyre::Result<SocketAddr>;

    /// Returns true if this header must be kept when proxying the request upstream
    fn keep_header(&self, name: &str) -> bool;

    /// Called when extra headers should be added to the request
    fn add_extra_headers(&self, add_header: &mut dyn FnMut(&str, &[u8])) {
        _ = add_header;
    }
}
