#![feature(thread_local)]

use eyre::Context;
use nom::IResult;
use parse::{
    aggregate::{AggregateBuf, AggregateSlice},
    h1::{Headers, Request, Response},
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

                debug!(?e, "error reading request header from downstream");
                return Ok(());
            }
        };
        debug_print_req(&dos_req);

        if is_chunked_transfer_encoding(&dos_req.headers) {
            let (res, _) = dos
                .write_all(SemanticError::NoChunked.as_http_response())
                .await;
            res.wrap_err("writing error response downstream")?;

            return Ok(());
        }

        let req_dv = conn_dv
            .steer_request(&dos_req)
            .wrap_err("steering request")?;

        let ups_addr = req_dv.upstream_addr()?;

        debug!("connecting to upstream at {ups_addr}...");
        let ups = TcpStream::connect(ups_addr).await?;
        // TODO: set no_delay

        debug!("writing request header to upstream");
        let ups_buf = AggregateBuf::default();
        encode_request(&dos_req, &ups_buf)?;
        let ups_buf = write_all(&ups, ups_buf)
            .await
            .wrap_err("writing request headers upstream")?;

        debug!("reading response headers from upstream");
        let (mut ups_buf, ups_res) = parse(h1::response, &ups, ups_buf, MAX_HEADER_LEN).await?;
        debug_print_res(&ups_res);

        if is_chunked_transfer_encoding(&ups_res.headers) {
            let (res, _) = dos
                .write_all(SemanticError::NoChunked.as_http_response())
                .await;
            res.wrap_err("writing error response downstream")?;

            return Ok(());
        }

        // at this point, `dos_buf` has the start of the request body,
        // and `ups_buf` has the start of the response body

        debug!("writing response headers downstream");
        let resh_buf = AggregateBuf::default();
        encode_response(&ups_res, &resh_buf)?;

        _ = write_all(&dos, resh_buf)
            .await
            .wrap_err("writing response headers to downstream");

        let connection_close = has_connection_close(&dos_req.headers);

        // now let's proxy bodies!
        let dos_req_clen = get_content_len(&dos_req.headers);
        let ups_res_clen = get_content_len(&ups_res.headers);

        let to_dos = copy(ups_buf, ups_res_clen, &ups, &dos);
        let to_ups = copy(dos_buf, dos_req_clen, &dos, &ups);

        (dos_buf, ups_buf) = tokio::try_join!(to_dos, to_ups)?;

        if connection_close {
            debug!("downstream requested connection close, fine by us!");
            return Ok(());
        }

        // TODO: re-use upstream connection - this needs a pool. more
        // importantly, this function is right now assuming we're always
        // proxying to somewhere. but what if we're not? need a more flexible
        // API, one that allows retrying, too, and also handling `expect`, `100
        // Continue`, etc.
    }
}

/// Copies what's left of `buf` (filled portion), then repeatedly reads into
/// it and writes that
async fn copy(
    mut buf: AggregateBuf,
    max_len: u64,
    src: &TcpStream,
    dst: &TcpStream,
) -> eyre::Result<AggregateBuf> {
    let mut remain = max_len;
    while remain > 0 {
        let slice = buf.read().read_slice();
        remain -= slice.len() as u64;
        // FIXME: protect against upstream writing too much. this is the wrong
        // level of abstraction.
        buf = write_all(dst, buf).await?;

        if remain == 0 {
            break;
        }

        let (res, slice);
        (res, slice) = src.read(buf.write_slice().limit(remain as _)).await;
        res?;
        buf = slice.into_inner();
    }

    Ok(buf)
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

    #[error("chunked transfer encoding is not supported")]
    NoChunked,
}

impl SemanticError {
    fn as_http_response(&self) -> &'static [u8] {
        match self {
            Self::HeadersTooLong => b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n",
            Self::NoChunked => b"HTTP/1.1 501 Chunked Transfer Encoding Not Implemented\r\n\r\n",
        }
    }
}

fn encode_request(req: &Request, buf: &AggregateBuf) -> eyre::Result<()> {
    let mut buf = buf.write();
    buf.put_agg(&req.method)?;
    buf.put(" ")?;
    buf.put_agg(&req.path)?;
    match req.version {
        1 => buf.put(" HTTP/1.1\r\n")?,
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", req.version)),
    }
    for header in &req.headers {
        buf.put_agg(&header.name)?;
        buf.put(": ")?;
        buf.put_agg(&header.value)?;
        buf.put("\r\n")?;
    }
    buf.put("\r\n")?;
    Ok(())
}

fn encode_response(res: &Response, buf: &AggregateBuf) -> eyre::Result<()> {
    let mut buf = buf.write();
    match res.version {
        1 => buf.put(b"HTTP/1.1 ")?,
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", res.version)),
    }
    // TODO: implement `std::fmt::Write` for `AggregateBuf`?
    // FIXME: wasteful
    let code = res.code.to_string();
    buf.put(code)?;
    buf.put(" ")?;
    buf.put_agg(&res.reason)?;
    buf.put("\r\n")?;
    for header in &res.headers {
        buf.put_agg(&header.name)?;
        buf.put(": ")?;
        buf.put_agg(&header.value)?;
        buf.put("\r\n")?;
    }
    buf.put("\r\n")?;

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

fn get_content_len(headers: &Headers) -> u64 {
    let mut content_len = 0;
    for h in headers {
        if h.name.eq_ignore_ascii_case("content-length") {
            // FIXME: this is really wasteful. maybe there's something to be
            // done where: 1) it's probably contiguous aynway, so just use that,
            // or: 2) if it's not, just copy it to a stack-allocated slice.
            //
            // this could be a method of AggregateSlice that lends a `&[u8]`
            // to a closure and errors out if it's too big. it could take
            // const generics.
            let value = h.value.to_vec();
            if let Ok(s) = std::str::from_utf8(&value[..]) {
                if let Ok(l) = s.parse() {
                    content_len = l;
                }
            }
        }
    }
    content_len
}

fn has_header_kv(headers: &Headers, k: impl AsRef<[u8]>, v: impl AsRef<[u8]>) -> bool {
    let k = k.as_ref();
    let v = v.as_ref();

    for h in headers {
        if h.name.eq_ignore_ascii_case(k) && h.value.eq_ignore_ascii_case(v) {
            return true;
        }
    }
    false
}

fn has_connection_close(headers: &Headers) -> bool {
    has_header_kv(headers, "connection", "close")
}

fn is_chunked_transfer_encoding(headers: &Headers) -> bool {
    has_header_kv(headers, "transfer-encoding", "chunked")
}
