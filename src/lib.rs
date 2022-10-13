#![feature(thread_local)]

use bufpool::BufMut;
use eyre::Context;
use futures::FutureExt;
use httparse::{Request, Response, Status, EMPTY_HEADER};
use parse::aggregate::AggregateBuf;
use std::{
    net::{Shutdown, SocketAddr},
    rc::Rc,
};
use tokio_uring::{buf::IoBuf, net::TcpStream};
use tracing::debug;

const MAX_HEADERS_LEN: usize = 64 * 1024;

pub mod bufpool;
pub mod parse;
pub use httparse;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;

use crate::parse::h1;

/// A connection driver maintains per-connection state and steers requests
pub trait ConnectionDriver {
    type RequestDriver: RequestDriver;

    fn build_request_context(&self, req: &Request) -> eyre::Result<Self::RequestDriver>;
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
    let mut ups_rd_buf = BufMut::alloc()?;
    let mut dos_rd_buf = BufMut::alloc()?;

    // try to read a complete request
    let mut dos_header_buf = Vec::with_capacity(MAX_HEADERS_LEN);
    let mut ups_header_buf = Vec::with_capacity(MAX_HEADERS_LEN);

    loop {
        let res;
        (res, dos_rd_buf) = dos.read(dos_rd_buf).await;
        let n = res?;
        if n == 0 {
            debug!("client went away (EOF) between requests, that's fine");
            return Ok(());
        }
        let prev_dos_header_buf_len = dos_header_buf.len();
        dos_header_buf.extend_from_slice(&dos_rd_buf[..n]);

        let mut dos_headers = vec![httparse::EMPTY_HEADER; 128];
        let mut dos_req = httparse::Request::new(&mut dos_headers);
        let dos_status = dos_req
            .parse(&dos_header_buf[..])
            .wrap_err("parsing downstream request header")?;
        match dos_status {
            Status::Partial => {
                if dos_header_buf.len() >= MAX_HEADERS_LEN {
                    dos.shutdown(Shutdown::Read)?;

                    debug!("downstream request headers too large");
                    let err_payload = b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n";

                    let mut slice = dos_rd_buf.slice(..err_payload.len());
                    slice[..].copy_from_slice(err_payload);

                    let (res, _) = dos.write_all(slice).await;
                    res?;

                    return Ok(());
                } else {
                    debug!("partial request (read of size {n}), continuing to read");
                    continue;
                }
            }
            Status::Complete(req_body_offset) => {
                let req_body_offset = req_body_offset - prev_dos_header_buf_len;
                let req_body_first_part = req_body_offset..n;
                debug!(
                    "downstream request body starts with {req_body_first_part:?} into dos_read_buf"
                );

                let req_method = dos_req
                    .method
                    .ok_or_else(|| eyre::eyre!("missing method"))?;
                let req_path = dos_req
                    .path
                    .ok_or_else(|| eyre::eyre!("missing http path"))?;

                let mut dos_conn_close = false;
                let mut req_content_length: Option<u64> = None;

                for header in dos_req.headers.iter() {
                    debug!(?header, "downstream req header");

                    if header.name.eq_ignore_ascii_case("content-length") {
                        req_content_length = Some(
                            std::str::from_utf8(header.value)
                                .wrap_err("content-length is not valid utf-8")?
                                .parse()
                                .wrap_err("could not parse content-length")?,
                        );
                    } else if header.name.eq_ignore_ascii_case("connection") {
                        #[allow(clippy::collapsible_if)]
                        if header.value.eq_ignore_ascii_case(b"close") {
                            dos_conn_close = true;
                        }
                    } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
                        if header.value.eq_ignore_ascii_case(b"chunked") {
                            return Err(eyre::eyre!("chunked transfer encoding not supported"));
                        } else {
                            return Err(eyre::eyre!(
                                "transfer-encoding not supported: {:?}",
                                std::str::from_utf8(header.value)
                            ));
                        }
                    }
                }

                let req_dv = conn_dv.build_request_context(&dos_req)?;
                let ups_addr = req_dv.upstream_addr()?;

                debug!("connecting to upstream at {ups_addr}...");
                let ups = TcpStream::connect(ups_addr).await?;

                debug!("writing request header to upstream...");
                ups_header_buf.clear();
                ups_header_buf.extend_from_slice(req_method.as_bytes());
                ups_header_buf.push(b' ');
                ups_header_buf.extend_from_slice(req_path.as_bytes());
                ups_header_buf.push(b' ');
                ups_header_buf.extend_from_slice(b"HTTP/1.1\r\n");

                for header in dos_req.headers.iter() {
                    if !req_dv.keep_header(header.name) {
                        continue;
                    }

                    ups_header_buf.extend_from_slice(header.name.as_bytes());
                    ups_header_buf.extend_from_slice(b": ");
                    ups_header_buf.extend_from_slice(header.value);
                    ups_header_buf.extend_from_slice(b"\r\n");
                }

                req_dv.add_extra_headers(&mut |name, value| {
                    ups_header_buf.extend_from_slice(name.as_bytes());
                    ups_header_buf.extend_from_slice(b": ");
                    ups_header_buf.extend_from_slice(value);
                    ups_header_buf.extend_from_slice(b"\r\n");
                });

                ups_header_buf.extend_from_slice(b"\r\n");

                let res;
                (res, ups_header_buf) = ups.write_all(ups_header_buf).await;
                res.wrap_err("writing request header upstream")?;

                let mut res_content_length: Option<u64> = None;
                let res_body_first_part;

                debug!("reading response header from upstream...");

                let mut ups_chunked = false;
                let mut ups_conn_close = false;

                'read_response: loop {
                    ups_header_buf.clear();

                    let res;
                    (res, ups_rd_buf) = ups.read(ups_rd_buf).await;
                    let n = res?;
                    if n == 0 {
                        return Err(eyre::eyre!("app closed connection before sending response"));
                    }
                    let prev_ups_header_buf_len = ups_header_buf.len();
                    ups_header_buf.extend_from_slice(&ups_rd_buf[..n]);

                    let mut ups_headers = vec![EMPTY_HEADER; 128];
                    let mut ups_res = Response::new(&mut ups_headers);
                    let ups_status = ups_res
                        .parse(&ups_header_buf[..])
                        .wrap_err("parsing downstream response header")?;
                    match ups_status {
                        Status::Partial => {
                            if ups_header_buf.len() >= MAX_HEADERS_LEN {
                                return Err(eyre::eyre!("upstream response headers too large"));
                            } else {
                                debug!("partial response (read of size {n}), continuing to read");
                                continue;
                            }
                        }
                        Status::Complete(res_body_offset) => {
                            let res_body_offset = res_body_offset - prev_ups_header_buf_len;
                            res_body_first_part = res_body_offset..n;
                            debug!("upstream response body starts with {res_body_first_part:?} into ups_read_buf");

                            let res_status = ups_res
                                .code
                                .ok_or_else(|| eyre::eyre!("missing http status"))?;
                            debug!("writing response header to downstream (HTTP {res_status})");

                            dos_header_buf.clear();
                            dos_header_buf.extend_from_slice(b"HTTP/1.1 ");
                            dos_header_buf.extend_from_slice(format!("{} ", res_status).as_bytes());
                            if let Some(reason) = ups_res.reason {
                                dos_header_buf.extend_from_slice(reason.as_bytes());
                            }
                            dos_header_buf.extend_from_slice(b"\r\n");

                            for header in ups_res.headers.iter() {
                                debug!(?header, "upstream res header");

                                if header.name.eq_ignore_ascii_case("content-length") {
                                    res_content_length = Some(
                                        std::str::from_utf8(header.value)
                                            .wrap_err("content-length is not valid utf-8")?
                                            .parse()
                                            .wrap_err("could not parse content-length")?,
                                    );
                                } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
                                    if header.value.eq_ignore_ascii_case(b"chunked") {
                                        ups_chunked = true;
                                        debug!("upstream is doing chunked transfer encoding");
                                    } else {
                                        return Err(eyre::eyre!(
                                            "transfer-encoding not supported: {:?}",
                                            std::str::from_utf8(header.value)
                                        ));
                                    }
                                } else if header.name.eq_ignore_ascii_case("connection") {
                                    #[allow(clippy::collapsible_if)]
                                    if header.value.eq_ignore_ascii_case(b"close") {
                                        ups_conn_close = true;
                                    }
                                }

                                dos_header_buf.extend_from_slice(header.name.as_bytes());
                                dos_header_buf.extend_from_slice(b": ");
                                dos_header_buf.extend_from_slice(header.value);
                                dos_header_buf.extend_from_slice(b"\r\n");
                            }

                            dos_header_buf.extend_from_slice(b"\r\n");

                            if ups_chunked {
                                // TODO: add support for chunked transfer-encoding
                                let err_payload = b"HTTP/1.1 500 Internal Server Error\r\n\r\n";

                                let mut slice = dos_rd_buf.slice(..err_payload.len());
                                slice[..].copy_from_slice(err_payload);

                                let (res, _) = dos.write_all(slice).await;
                                res?;

                                // FIXME: that's wrong, what if the client established a
                                // persistent connection with us?
                                return Ok(());
                            }

                            let res;
                            (res, dos_header_buf) = dos.write_all(dos_header_buf).await;
                            res.wrap_err("writing response header to downstream")?;

                            break 'read_response;
                        }
                    }
                }

                let mut req_content_length = req_content_length.unwrap_or(0);
                let mut res_content_length = res_content_length.unwrap_or(0);

                debug!("writing first request body part to upstream ({req_body_first_part:?})...");
                req_content_length -= req_body_first_part.len() as u64;
                let res;
                let slice;
                (res, slice) = ups.write_all(dos_rd_buf.slice(req_body_first_part)).await;
                res.wrap_err("writing request body (first chunk) to upstream")?;
                dos_rd_buf = slice.into_inner();
                debug!("{req_content_length} req body left");

                debug!(
                    "writing first response body part to downstream ({res_body_first_part:?})..."
                );
                // FIXME: this can overflow if content-length is smaller than
                // what upstream actually sent.
                res_content_length -= res_body_first_part.len() as u64;
                let res;
                let slice;
                (res, slice) = dos.write_all(ups_rd_buf.slice(res_body_first_part)).await;
                res.wrap_err("writing response body (first chunk) to downstream")?;
                ups_rd_buf = slice.into_inner();
                debug!("{res_content_length} res body left");

                debug!("proxying bodies in both directions");

                let req_body_fut =
                    copy("req body", &dos, &ups, dos_rd_buf, &mut req_content_length)
                        .map(|r| r.wrap_err("copying request body to upstream"));
                let res_body_fut =
                    copy("res body", &ups, &dos, ups_rd_buf, &mut res_content_length)
                        .map(|r| r.wrap_err("copying response body to downstream"));

                (dos_rd_buf, ups_rd_buf) = tokio::try_join!(res_body_fut, req_body_fut)?;

                if ups_conn_close {
                    debug!("upstream requested connection close, that's fine we don't re-use connections yet");
                    return Ok(());
                }

                if dos_conn_close {
                    debug!("downstream requested connection close");
                    dos.shutdown(Shutdown::Both)?;
                    debug!("downstream shutdown");
                    return Ok(());
                }

                dos_header_buf.clear();
            }
        };
    }
}

// Copy `content_length` bytes from `src` to `dst` using the provided buffer.
async fn copy(
    role: &str,
    src: &TcpStream,
    dst: &TcpStream,
    buf: BufMut,
    content_length: &mut u64,
) -> eyre::Result<BufMut> {
    let mut buf = buf;

    while *content_length > 0 {
        debug!(%role, "{content_length} left");

        let res;
        (res, buf) = src.read(buf).await;
        let n = res.wrap_err("reading")?;
        *content_length -= n as u64;
        debug!(%role, "read {n} bytes, {content_length} left");

        if n == 0 {
            debug!(%role, "end of stream");
            return Ok(buf);
        }

        let res;
        let slice;
        (res, slice) = dst.write_all(buf.slice(..n)).await;
        res.wrap_err("writing")?;
        buf = slice.into_inner();
        debug!(%role, "wrote {n} bytes, {content_length} left");
    }

    Ok(buf)
}

/// Handle a plaintext HTTP/1.1 connection
pub async fn serve_h1_b(_conn_dv: Rc<impl ConnectionDriver>, dos: TcpStream) -> eyre::Result<()> {
    let mut buf = AggregateBuf::new()?;

    loop {
        let (new_buf, req) = read_req_header(&dos, buf).await?;
        buf = new_buf;

        debug!(method = %req.method.to_string_lossy(), path = %req.path.to_string_lossy(), version = %req.version, "got request");
        for h in &req.headers {
            debug!(name = %h.name.to_string_lossy(), value = %h.value.to_string_lossy(), "got header");
        }

        todo!("send request upstream");
    }

    Ok(())
}

async fn read_req_header(dos: &TcpStream, mut buf: AggregateBuf) -> eyre::Result<(AggregateBuf, h1::Request)> {
    loop {
        let (res, buf_s) = dos.read(buf.write_slice()).await;
        res?;
        buf = buf_s.into_inner();
        let slice = buf.read().slice(0..buf.read().len());

        let (rest, req) = match h1::request(slice) {
            Ok(t) => t
            Err(err) => {
                if err.is_incomplete() {
                    debug!("incomplete request, need more data");
                    continue;
                } else {
                    return Err(eyre::eyre!("parsing error: {err}"));
                }
            }
        };

        return Ok((buf, req));
    }
}
