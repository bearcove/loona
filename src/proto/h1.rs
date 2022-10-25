//! HTTP/1.1 https://www.rfc-editor.org/rfc/rfc9112.
//! HTTP semantics https://www.rfc-editor.org/rfc/rfc9110

use eyre::Context;
use std::rc::Rc;
use tokio_uring::net::TcpStream;
use tracing::debug;

use crate::{
    bufpool::{AggBuf, AggSlice, Buf, IoChunkList, IoChunkable},
    io::{ReadOwned, ReadWriteOwned, WriteOwned},
    parse::h1,
    proto::{
        errors::SemanticError,
        util::{read_and_parse, write_all, write_all_list},
    },
    types::{ConnectionDriver, Headers, Request, RequestDriver, Response},
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
        let dos_req_clen = dos_req.headers.content_length().unwrap_or_default();

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

        let ups_res_clen = ups_res.headers.content_length().unwrap_or_default();

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
    encode_headers(res.headers, list)?;
    list.push("\r\n");
    Ok(())
}

fn encode_headers(headers: Headers, list: &mut IoChunkList) -> eyre::Result<()> {
    for header in headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
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

pub struct ServerConf {
    /// Max length of the request line + HTTP headers
    pub max_http_header_len: u32,

    /// Max length of a single header record, e.g. `user-agent: foobar`
    pub max_header_record_len: u32,

    /// Max number of header records
    pub max_header_records: u32,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self {
            max_http_header_len: 64 * 1024,
            max_header_record_len: 4 * 1024,
            max_header_records: 128,
        }
    }
}

pub trait ServerDriver {
    async fn handle<T, B>(
        &self,
        req: Request,
        req_body: B,
        respond: H1Responder<T, ExpectResponseHeaders>,
    ) -> eyre::Result<(B, H1Responder<T, ResponseDone>)>
    where
        // FIXME: lift the 'static restriction by not spawning send req body
        T: WriteOwned + 'static,
        B: Body + 'static;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    ClientRequestedConnectionClose,
    ServerRequestedConnectionClose,
    ClientClosedConnectionBetweenRequests,
    // TODO: return buffer there so we can see what they did write?
    ClientDidntSpeakHttp11,
}

pub async fn serve(
    // TODO: split read/write halves? so we don't have to handle `Rc` ourselves
    // TODO: life 'static requirement
    transport: impl ReadWriteOwned + 'static,
    conf: Rc<ServerConf>,
    mut client_buf: AggBuf,
    driver: impl ServerDriver,
) -> eyre::Result<ServeOutcome> {
    let transport = Rc::new(transport);

    loop {
        let req;
        (client_buf, req) = match read_and_parse(
            h1::request,
            transport.as_ref(),
            client_buf,
            conf.max_http_header_len,
        )
        .await
        {
            Ok(t) => match t {
                Some(t) => t,
                None => {
                    debug!("client went away before sending request headers");
                    return Ok(ServeOutcome::ClientClosedConnectionBetweenRequests);
                }
            },
            Err(e) => {
                if let Some(se) = e.downcast_ref::<SemanticError>() {
                    let (res, _) = transport.write_all(se.as_http_response()).await;
                    res.wrap_err("writing error response downstream")?;
                }

                debug!(?e, "error reading request header from downstream");
                return Ok(ServeOutcome::ClientDidntSpeakHttp11);
            }
        };
        debug_print_req(&req);

        let chunked = req.headers.is_chunked_transfer_encoding();
        let connection_close = req.headers.is_connection_close();
        let content_len = req.headers.content_length().unwrap_or_default();

        let req_body = H1Body {
            transport: transport.clone(),
            buf: client_buf,
            kind: if chunked {
                H1BodyKind::Chunked
            } else if content_len > 0 {
                H1BodyKind::ContentLength(content_len)
            } else {
                H1BodyKind::Empty
            },
            read: 0,
            eof: false,
        };

        let res_handle = H1Responder::new(transport.clone());

        let (req_body, res_handle) = driver
            .handle(req, req_body, res_handle)
            .await
            .wrap_err("handling request")?;

        // TODO: if we sent `connection: close` we should close now
        _ = res_handle;

        if !req_body.eof() {
            return Err(eyre::eyre!(
                "request body not drained, have to close connection"
            ));
        }

        client_buf = req_body.buf;

        if connection_close {
            debug!("client requested connection close");
            return Ok(ServeOutcome::ClientRequestedConnectionClose);
        }
    }
}

pub trait ResponseState {}

pub struct ExpectResponseHeaders;
impl ResponseState for ExpectResponseHeaders {}

pub struct ExpectResponseBody;
impl ResponseState for ExpectResponseBody {}

pub struct ResponseDone;
impl ResponseState for ResponseDone {}

pub struct H1Responder<T, S>
where
    S: ResponseState,
    T: WriteOwned,
{
    #[allow(dead_code)]
    state: S,
    transport: Rc<T>,
}

impl<T> H1Responder<T, ExpectResponseHeaders>
where
    T: WriteOwned,
{
    fn new(transport: Rc<T>) -> Self {
        Self {
            state: ExpectResponseHeaders,
            transport,
        }
    }

    /// Send an informational status code, cf. https://httpwg.org/specs/rfc9110.html#status.1xx
    /// Errors out if the response status is not 1xx
    pub async fn write_interim_response(
        self,
        res: Response,
    ) -> eyre::Result<H1Responder<T, ExpectResponseHeaders>> {
        if res.code >= 200 {
            return Err(eyre::eyre!("interim response must have status code 1xx"));
        }

        let this = self.write_response_internal(res).await?;
        Ok(this)
    }

    /// Send the final response headers
    /// Errors out if the response status is < 200.
    /// Errors out if the client sent `expect: 100-continue`
    pub async fn write_final_response(
        self,
        res: Response,
    ) -> eyre::Result<H1Responder<T, ExpectResponseBody>> {
        if res.code < 200 {
            return Err(eyre::eyre!("final response must have status code >= 200"));
        }

        let this = self.write_response_internal(res).await?;
        Ok(H1Responder {
            state: ExpectResponseBody,
            transport: this.transport,
        })
    }

    async fn write_response_internal(self, res: Response) -> eyre::Result<Self> {
        let mut list = IoChunkList::default();
        encode_response(res, &mut list)?;

        let list = write_all_list(self.transport.as_ref(), list)
            .await
            .wrap_err("writing response headers upstream")?;

        // TODO: can we re-use that list? pool it?
        drop(list);

        Ok(self)
    }
}

impl<T> H1Responder<T, ExpectResponseBody>
where
    T: WriteOwned,
{
    /// Send a response body chunk. Errors out if sending more than the
    /// announced content-length.
    pub async fn write_body_chunk(
        self,
        chunk: Buf,
    ) -> eyre::Result<H1Responder<T, ExpectResponseBody>> {
        _ = chunk;
        todo!();
    }

    /// Finish the body, with optional trailers, cf. https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/TE
    /// Errors out if the sent body doesn't match the announced content-length.
    /// Errors out if trailers that weren't announced are being sent, or if the client
    /// didn't explicitly announce it accepted trailers, or if the response is a 204,
    /// 205 or 304, or if the body wasn't sent with chunked transfer encoding.
    pub async fn finish_body(
        self,
        trailers: Option<Headers>,
    ) -> eyre::Result<H1Responder<T, ResponseDone>> {
        if let Some(trailers) = trailers {
            // TODO: check all preconditions
            let mut list = IoChunkList::default();
            encode_headers(trailers, &mut list)?;

            let list = write_all_list(self.transport.as_ref(), list)
                .await
                .wrap_err("writing response headers upstream")?;

            // TODO: can we re-use that list? pool it?
            drop(list);
        }

        // TODO: check content-length side, write empty buf is doing chunked transfer encoding, etc.

        Ok(H1Responder {
            state: ResponseDone,
            transport: self.transport,
        })
    }
}

pub struct ClientConf {}

pub trait ClientDriver {
    type Return;

    async fn on_informational_response(&self, res: Response) -> eyre::Result<()>;
    async fn on_final_response<B>(self, res: Response, body: B) -> eyre::Result<(B, Self::Return)>
    where
        B: Body;
    // async fn on_request_body_error(&self, err: eyre::Report);
}

/// Perform an HTTP/1.1 request against an HTTP/1.1 server
///
/// The transport will be returned unless the server requested connection close.
pub async fn request<T, B, D>(
    transport: Rc<T>,
    req: Request,
    body: B,
    driver: D,
) -> eyre::Result<(Option<Rc<T>>, B, D::Return)>
where
    T: ReadWriteOwned + 'static,
    B: Body + 'static,
    D: ClientDriver + 'static,
{
    // TODO: set content-length if body isn't empty / missing
    let mode = if body.content_len().is_none() {
        BodyWriteMode::Chunked
    } else {
        BodyWriteMode::ContentLength
    };

    let mut list = IoChunkList::default();
    encode_request(req, &mut list)?;
    _ = write_all_list(transport.as_ref(), list)
        .await
        .wrap_err("writing request headers")?;

    // TODO: handle `expect: 100-continue` (don't start sending body until we get a 100 response)

    let send_body_fut = {
        let transport = transport.clone();
        async move {
            match write_h1_body(transport, body, mode).await {
                Err(err) => {
                    // TODO: find way to report this error to the driver without
                    // spawning, without ref-counting the driver, etc.
                    panic!("error writing request body: {err:?}");
                }
                Ok(body) => {
                    debug!("done writing request body");
                    Ok::<_, eyre::Report>(body)
                }
            }
        }
    };
    // TODO: spawning is almost definitely not the right thing to do here.
    // I don't think we ever want to spawn anything.
    let send_body_jh = tokio_uring::spawn(send_body_fut);

    let ups_buf = AggBuf::default();
    let (ups_buf, res) =
        match read_and_parse(h1::response, transport.as_ref(), ups_buf, MAX_HEADER_LEN).await {
            Ok(t) => match t {
                Some(t) => t,
                None => {
                    return Err(eyre::eyre!(
                        "server went away before sending response headers"
                    ));
                }
            },
            Err(e) => {
                // TODO: return error instead?
                return Err(eyre::eyre!(
                    "error reading response headers from server: {e:?}"
                ));
            }
        };
    debug_print_res(&res);

    // TODO: handle informational responses

    let chunked = res.headers.is_chunked_transfer_encoding();
    // TODO: handle 204/304 separately
    let content_len = res.headers.content_length().unwrap_or_default();

    let res_body = H1Body {
        transport: transport.clone(),
        buf: ups_buf,
        kind: if chunked {
            H1BodyKind::Chunked
        } else if content_len > 0 {
            H1BodyKind::ContentLength(content_len)
        } else {
            H1BodyKind::Empty
        },
        read: 0,
        eof: false,
    };

    let conn_close = res.headers.is_connection_close();

    let (res_body, ret) = driver.on_final_response(res, res_body).await?;
    // TODO: re-use buffer for pooled connections?
    drop(res_body);

    let req_body = send_body_jh.await??;

    let transport = if conn_close { None } else { Some(transport) };
    Ok((transport, req_body, ret))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyWriteMode {
    Chunked,
    ContentLength,
}

async fn write_h1_body<B>(
    transport: Rc<impl WriteOwned>,
    mut body: B,
    mode: BodyWriteMode,
) -> eyre::Result<B>
where
    B: Body,
{
    loop {
        let chunk;
        (body, chunk) = body.next_chunk().await?;
        match chunk {
            BodyChunk::Buf(chunk) => write_h1_body_chunk(transport.as_ref(), chunk, mode).await?,
            BodyChunk::AggSlice(chunk) => {
                write_h1_body_chunk(transport.as_ref(), chunk, mode).await?
            }
            BodyChunk::Eof => {
                // TODO: check that we've sent what we announced in terms of
                // content length
                write_h1_body_end(transport.as_ref(), mode).await?;
                break;
            }
        }
    }

    Ok(body)
}

async fn write_h1_body_chunk(
    transport: &impl WriteOwned,
    chunk: impl IoChunkable,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            let mut list = IoChunkList::default();
            list.push(format!("{:x}\r\n", chunk.len()).into_bytes());
            list.push(chunk);
            list.push("\r\n");

            let list = write_all_list(transport, list).await?;
            drop(list);
        }
        BodyWriteMode::ContentLength => {
            let mut list = IoChunkList::default();
            list.push(chunk);
            let list = write_all_list(transport, list).await?;
            drop(list);
        }
    }
    Ok(())
}

async fn write_h1_body_end(transport: &impl WriteOwned, mode: BodyWriteMode) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            let mut list = IoChunkList::default();
            list.push("0\r\n\r\n");
            _ = write_all_list(transport, list).await?;
        }
        BodyWriteMode::ContentLength => {
            // nothing to do
        }
    }
    Ok(())
}

pub enum BodyChunk {
    Buf(Buf),
    AggSlice(AggSlice),
    Eof,
}

pub trait Body
where
    Self: Sized,
{
    fn content_len(&self) -> Option<u64>;
    fn eof(&self) -> bool;
    async fn next_chunk(self) -> eyre::Result<(Self, BodyChunk)>;
}

struct H1Body<T> {
    transport: Rc<T>,
    buf: AggBuf,
    kind: H1BodyKind,
    read: u64,
    eof: bool,
}

enum H1BodyKind {
    Chunked,
    ContentLength(u64),
    Empty,
}

impl<T> Body for H1Body<T>
where
    T: ReadOwned,
{
    fn content_len(&self) -> Option<u64> {
        match self.kind {
            H1BodyKind::Chunked => None,
            H1BodyKind::ContentLength(len) => Some(len),
            H1BodyKind::Empty => Some(0),
        }
    }

    async fn next_chunk(mut self) -> eyre::Result<(Self, BodyChunk)> {
        if self.eof {
            return Ok((self, BodyChunk::Eof));
        }

        match self.kind {
            H1BodyKind::Chunked => {
                const MAX_CHUNK_LENGTH: u32 = 1024 * 1024;

                debug!("reading chunk");
                let chunk;
                let mut buf = self.buf;
                buf.write().grow_if_needed()?;

                // TODO: this reads the whole chunk, but if we don't need to maintain
                // chunk size, we don't need to buffer that far. we can just read
                // whatever, skip the CRLF, know when we need to stop to read another
                // chunk length, etc. this needs to be a state machine.
                (buf, chunk) =
                    match read_and_parse(h1::chunk, self.transport.as_ref(), buf, MAX_CHUNK_LENGTH)
                        .await?
                    {
                        Some(t) => t,
                        None => {
                            return Err(eyre::eyre!("peer went away before sending final chunk"));
                        }
                    };
                debug!("read {} byte chunk", chunk.len);

                self.buf = buf;

                if chunk.len == 0 {
                    debug!("received 0-length chunk, that's EOF!");
                    self.eof = true;
                    Ok((self, BodyChunk::Eof))
                } else {
                    self.read += chunk.len;
                    Ok((self, BodyChunk::AggSlice(chunk.data)))
                }
            }
            H1BodyKind::ContentLength(len) => {
                let remain = len - self.read;
                if remain == 0 {
                    self.eof = true;
                    return Ok((self, BodyChunk::Eof));
                }

                let mut buf = self.buf;

                buf.write().grow_if_needed()?;
                let mut slice = buf.write_slice().limit(remain);
                let res;
                (res, slice) = self.transport.as_ref().read(slice).await;
                buf = slice.into_inner();
                let n = res?;

                self.read += n as u64;
                let slice = buf.read().slice(0..n as u32);
                self.buf = buf.split();

                Ok((self, BodyChunk::AggSlice(slice)))
            }
            H1BodyKind::Empty => {
                self.eof = true;
                Ok((self, BodyChunk::Eof))
            }
        }
    }

    fn eof(&self) -> bool {
        match self.kind {
            H1BodyKind::Chunked => self.eof,
            H1BodyKind::ContentLength(len) => self.read == len,
            H1BodyKind::Empty => true,
        }
    }
}

impl Body for () {
    fn content_len(&self) -> Option<u64> {
        Some(0)
    }

    fn eof(&self) -> bool {
        true
    }

    async fn next_chunk(self) -> eyre::Result<(Self, BodyChunk)> {
        Ok((self, BodyChunk::Eof))
    }
}
