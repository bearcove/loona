use super::Request;
use std::net::SocketAddr;

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
