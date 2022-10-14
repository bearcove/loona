use std::net::SocketAddr;

use alt_http::{parse::h1::Request, ConnectionDriver, RequestDriver};

pub(crate) struct FixedConnDriver {
    pub(crate) upstream_addr: SocketAddr,
}

pub(crate) struct FixedReqDriver {
    upstream_addr: SocketAddr,
}

impl ConnectionDriver for FixedConnDriver {
    type RequestDriver = FixedReqDriver;

    fn steer_request(&self, _req: &Request) -> eyre::Result<Self::RequestDriver> {
        Ok(FixedReqDriver {
            upstream_addr: self.upstream_addr,
        })
    }
}

impl RequestDriver for FixedReqDriver {
    fn upstream_addr(&self) -> eyre::Result<std::net::SocketAddr> {
        Ok(self.upstream_addr)
    }

    fn keep_header(&self, _name: &str) -> bool {
        true
    }
}
