//! As of June 2022, the authoritative document for HTTP/1.1
//! is https://www.rfc-editor.org/rfc/rfc9110

use eyre::Context;
use std::rc::Rc;
use tokio_uring::net::TcpStream;
use tracing::debug;

use crate::{
    bufpool::{AggBuf, IoChunkList},
    io::{ReadOwned, ReadWriteOwned, WriteOwned},
    parse::h1,
    proto::{
        errors::SemanticError,
        util::{read_and_parse, write_all, write_all_list},
    },
    types::{ConnectionDriver, Request, RequestDriver, Response},
};

/// maximum HTTP/1.1 header length (includes request-line/response-line etc.)
const MAX_HEADER_LEN: u32 = 64 * 1024;

/// Proxy incoming HTTP/1.1 requests to some upstream.
pub async fn proxy(
    conn_dv: Rc<impl ConnectionDriver>,
    dos: impl ReadWriteOwned,
    mut dos_buf: AggBuf,
) -> eyre::Result<()> {
    loop {
        let dos_req;
        (dos_buf, dos_req) = match read_and_parse(h1::request, &dos, dos_buf, MAX_HEADER_LEN).await
        {
            Ok(t) => match t {
                Some(t) => t,
                None => {
                    debug!("client went away before sending request headers");
                    return Ok(());
                }
            },
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

        let dos_chunked = dos_req.headers.is_chunked_transfer_encoding();
        let connection_close = dos_req.headers.is_connection_close();
        let dos_req_clen = dos_req.headers.content_len().unwrap_or_default();

        let req_dv = conn_dv
            .steer_request(&dos_req)
            .wrap_err("steering request")?;

        let ups_addr = req_dv.upstream_addr()?;

        debug!("connecting to upstream at {ups_addr}...");
        let ups = TcpStream::connect(ups_addr).await?;
        // TODO: set no_delay

        debug!("writing request header to upstream");
        let mut list = IoChunkList::default();
        encode_request(dos_req, &mut list)?;
        debug!("encoded...");
        let mut list = write_all_list(&ups, list)
            .await
            .wrap_err("writing request headers upstream")?;

        debug!("reading response headers from upstream");
        let ups_buf = AggBuf::default();
        let (mut ups_buf, ups_res) =
            match read_and_parse(h1::response, &ups, ups_buf, MAX_HEADER_LEN).await {
                Ok(t) => match t {
                    Some(t) => t,
                    None => {
                        // TODO: reply with 502 or something
                        debug!("server went away before sending response headers");
                        return Ok(());
                    }
                },
                Err(e) => {
                    if let Some(se) = e.downcast_ref::<SemanticError>() {
                        let (res, _) = dos.write_all(se.as_http_response()).await;
                        res.wrap_err("writing error response downstream")?;
                    }

                    debug!(?e, "error reading request header from downstream");
                    return Ok(());
                }
            };
        debug_print_res(&ups_res);

        let ups_chunked = ups_res.headers.is_chunked_transfer_encoding();

        // at this point, `dos_buf` has the start of the request body,
        // and `ups_buf` has the start of the response body

        let ups_res_clen = ups_res.headers.content_len().unwrap_or_default();

        debug!("writing response headers downstream");
        encode_response(ups_res, &mut list)?;

        _ = write_all_list(&dos, list)
            .await
            .wrap_err("writing response headers to downstream");

        // now let's proxy bodies!
        let to_dos = async {
            if ups_chunked {
                copy_chunked(ups_buf, &ups, &dos).await
            } else {
                copy(ups_buf, ups_res_clen, &ups, &dos).await
            }
        };
        let to_ups = async {
            if dos_chunked {
                copy_chunked(dos_buf, &dos, &ups).await
            } else {
                copy(dos_buf, dos_req_clen, &dos, &ups).await
            }
        };
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
        _ = ups_buf;
    }
}

/// Copies what's left of `buf` (filled portion), then repeatedly reads into
/// it and writes that
async fn copy(
    mut buf: AggBuf,
    max_len: u64,
    src: &impl ReadOwned,
    dst: &impl WriteOwned,
) -> eyre::Result<AggBuf> {
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

        buf.write().grow_if_needed()?;

        let (res, slice);
        (res, slice) = src.read(buf.write_slice().limit(remain as _)).await;
        res?;
        buf = slice.into_inner();
    }

    Ok(buf)
}

async fn copy_chunked(
    mut buf: AggBuf,
    src: &impl ReadOwned,
    dst: &impl WriteOwned,
) -> eyre::Result<AggBuf> {
    const MAX_CHUNK_LENGTH: u32 = 1024 * 1024;

    loop {
        debug!("reading chunk");
        let chunk;

        // TODO: this reads the whole chunk, but if we don't need to maintain
        // chunk size, we don't need to buffer that far. we can just read
        // whatever, skip the CRLF, know when we need to stop to read another
        // chunk length, etc. this needs to be a state machine.
        (buf, chunk) = match read_and_parse(h1::chunk, src, buf, MAX_CHUNK_LENGTH).await? {
            Some(t) => t,
            None => {
                return Err(eyre::eyre!("peer went away before sending final chunk"));
            }
        };
        debug!("read {} byte chunk", chunk.len);

        if chunk.len == 0 {
            debug!("received 0-length chunk, that's EOF!");
            let mut list = IoChunkList::default();
            list.push("0\r\n\r\n");
            _ = write_all_list(dst, list).await?;

            break;
        }

        {
            let mut list = IoChunkList::default();
            list.push(format!("{:x}\r\n", chunk.len).into_bytes());
            list.push(chunk.data);
            list.push("\r\n");

            _ = write_all_list(dst, list).await?;
        }
    }

    Ok(buf)
}

fn encode_request(req: Request, list: &mut IoChunkList) -> eyre::Result<()> {
    list.push(req.method);
    list.push(" ");
    list.push(req.path);
    match req.version {
        1 => list.push(" HTTP/1.1\r\n"),
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", req.version)),
    }
    for header in req.headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
    list.push("\r\n");
    Ok(())
}

fn encode_response(res: Response, list: &mut IoChunkList) -> eyre::Result<()> {
    match res.version {
        1 => list.push(&b"HTTP/1.1 "[..]),
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", res.version)),
    }
    // FIXME: wasteful
    let code = res.code.to_string();
    list.push(code.into_bytes());
    list.push(" ");
    list.push(res.reason);
    list.push("\r\n");
    for header in res.headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
    list.push("\r\n");
    Ok(())
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
