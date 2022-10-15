#![feature(thread_local)]

use eyre::Context;
use nom::IResult;
use parse::{
    aggregate::{AggregateBuf, AggregateSlice},
    h1::{Request, Response},
};
use std::{net::SocketAddr, rc::Rc};
use tokio_uring::net::TcpStream;
use tracing::debug;

pub mod bufpool;
pub mod parse;
pub use httparse;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;

use crate::parse::h1;

/// maximum HTTP/1.1 header length (includes request-line/response-line etc.)
const MAX_HEADER_LEN: u32 = 64 * 1024;

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

/// Handle a plaintext HTTP/1.1 connection
pub async fn serve_h1(conn_dv: Rc<impl ConnectionDriver>, dos: TcpStream) -> eyre::Result<()> {
    let mut dos_buf = AggregateBuf::default();

    loop {
        let dos_req;
        (dos_buf, dos_req) = match parse(h1::request, &dos, dos_buf, MAX_HEADER_LEN).await {
            Ok(t) => t,
            Err(e) => {
                if let Some(se) = e.downcast_ref::<SemanticError>() {
                    let (res, _) = dos.write_all(se.as_http_response()).await;
                    res.wrap_err("writing error response downstream")?;
                }

                debug!(?e, "error reading request header");
                return Ok(());
            }
        };
        debug_print_req(&dos_req);

        let req_dv = conn_dv
            .steer_request(&dos_req)
            .wrap_err("steering request")?;

        let ups_addr = req_dv.upstream_addr()?;

        debug!("connecting to upstream at {ups_addr}...");
        let ups = TcpStream::connect(ups_addr).await?;
        // TODO: set no_delay

        debug!("writing request header");
        let ups_buf = AggregateBuf::default();
        encode_request(&dos_req, &ups_buf)?;
        let ups_buf = write_all(&ups, ups_buf)
            .await
            .wrap_err("writing request headers")?;

        debug!("reading response headers from upstream");
        let (ups_buf, ups_res) = parse(h1::response, &ups, ups_buf, MAX_HEADER_LEN).await?;

        debug_print_res(&ups_res);

        todo!("the rest of the owl");
    }
}

async fn parse<Parser, Output>(
    parser: Parser,
    stream: &TcpStream,
    mut buf: AggregateBuf,
    max_len: u32,
) -> eyre::Result<(AggregateBuf, Output)>
where
    Parser: Fn(AggregateSlice) -> IResult<AggregateSlice, Output>,
{
    loop {
        if buf.write().capacity() >= max_len {
            return Err(SemanticError::HeadersTooLong.into());
        }
        buf.write().grow_if_needed()?;

        let (res, buf_s) = stream.read(buf.write_slice()).await;
        res.wrap_err("reading request headers from downstream")?;
        buf = buf_s.into_inner();
        debug!("reading headers ({} bytes so far)", buf.read().len());
        let slice = buf.read().slice(0..buf.read().len());

        let (rest, req) = match parser(slice) {
            Ok(t) => t,
            Err(err) => {
                if err.is_incomplete() {
                    debug!("incomplete request, need more data");
                    continue;
                } else {
                    if let nom::Err::Error(e) = &err {
                        debug!(?err, "parsing error");
                        debug!(input = %e.input.to_string_lossy(), "input was");
                    }
                    return Err(eyre::eyre!("parsing error: {err}"));
                }
            }
        };

        return Ok((buf.split_at(rest), req));
    }
}

#[derive(thiserror::Error, Debug)]
enum SemanticError {
    #[error("the headers are too long")]
    HeadersTooLong,
}

impl SemanticError {
    fn as_http_response(&self) -> &'static [u8] {
        match self {
            Self::HeadersTooLong => b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n",
        }
    }
}

fn encode_request(req: &Request, buf: &AggregateBuf) -> eyre::Result<()> {
    let mut buf = buf.write();
    buf.put_agg(&req.method)?;
    buf.put(b" ")?;
    buf.put_agg(&req.path)?;
    match req.version {
        1 => buf.put(b" HTTP/1.1\r\n")?,
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", req.version)),
    }
    for header in &req.headers {
        buf.put_agg(&header.name)?;
        buf.put(b": ")?;
        buf.put_agg(&header.value)?;
        buf.put(b"\r\n")?;
    }
    buf.put(b"\r\n")?;
    Ok(())
}

async fn write_all(stream: &TcpStream, buf: AggregateBuf) -> eyre::Result<AggregateBuf> {
    let slice = buf.read().read_slice();

    let mut offset = 0;
    while let Some(slice) = slice.next_slice(offset) {
        offset += slice.len() as u32;
        let (res, _) = stream.write_all(slice).await;
        res?;
    }

    Ok(buf.split())
}

fn debug_print_req(req: &Request) {
    debug!(method = %req.method.to_string_lossy(), path = %req.path.to_string_lossy(), version = %req.version, "got request");
    for h in &req.headers {
        debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
    }
}

fn debug_print_res(res: &Response) {
    debug!(code = %res.code, reason = %res.reason.to_string_lossy(), version = %res.version, "got response");
    for h in &res.headers {
        debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
    }
}
