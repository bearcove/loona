#![feature(thread_local)]

use eyre::Context;
use parse::{aggregate::AggregateBuf, h1::Request};
use std::{net::SocketAddr, rc::Rc};
use tokio_uring::net::TcpStream;
use tracing::debug;

pub mod bufpool;
pub mod parse;
pub use httparse;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;

use crate::parse::h1;

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
        // sigh
        let _ = add_header;
    }
}

/// Handle a plaintext HTTP/1.1 connection
pub async fn serve_h1(conn_dv: Rc<impl ConnectionDriver>, dos: TcpStream) -> eyre::Result<()> {
    let mut buf = AggregateBuf::default();

    loop {
        buf.write().grow_if_needed()?;
        let req;
        (buf, req) = read_req_header(&dos, buf).await?;

        debug!(method = %req.method.to_string_lossy(), path = %req.path.to_string_lossy(), version = %req.version, "got request");
        for h in &req.headers {
            debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
        }

        let req_cv = conn_dv.steer_request(&req).wrap_err("steering request")?;

        _ = (buf, req_cv);
        todo!("send request upstream");
    }
}

async fn read_req_header(
    dos: &TcpStream,
    mut buf: AggregateBuf,
) -> eyre::Result<(AggregateBuf, h1::Request)> {
    loop {
        let (res, buf_s) = dos.read(buf.write_slice()).await;
        res.wrap_err("reading request headers from downstream")?;
        buf = buf_s.into_inner();
        let slice = buf.read().slice(0..buf.read().len());

        let (rest, req) = match h1::request(slice) {
            Ok(t) => t,
            Err(err) => {
                if err.is_incomplete() {
                    debug!("incomplete request, need more data");
                    continue;
                } else {
                    return Err(eyre::eyre!("parsing error: {err}"));
                }
            }
        };

        return Ok((buf.split_keeping_rest(rest), req));
    }
}
