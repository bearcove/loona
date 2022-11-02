use std::rc::Rc;

use eyre::Context;
use tracing::debug;

use crate::{
    buffet::PieceList,
    io::WriteOwned,
    util::{read_and_parse, write_all_list, SemanticError},
    Body, BodyChunk, Headers, HeadersExt, Piece, ReadWriteOwned, Request, Response, RollMut,
};

use super::{
    body::{BodyWriteMode, H1Body, H1BodyKind},
    encode::{encode_headers, encode_response},
};

pub struct ServerConf {
    /// Max length of the request line + HTTP headers
    pub max_http_header_len: usize,

    /// Max length of a single header record, e.g. `user-agent: foobar`
    pub max_header_record_len: usize,

    /// Max number of header records
    pub max_header_records: usize,
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
    async fn handle<T: WriteOwned>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<T, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<T, ResponseDone>>;
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
    transport: impl ReadWriteOwned,
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: impl ServerDriver,
) -> eyre::Result<ServeOutcome> {
    let transport = Rc::new(transport);

    loop {
        let req;
        (client_buf, req) = match read_and_parse(
            super::parse::request,
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
        req.debug_print();

        let chunked = req.headers.is_chunked_transfer_encoding();
        let connection_close = req.headers.is_connection_close();
        let content_len = req.headers.content_length().unwrap_or_default();

        let mut req_body = H1Body::new(
            transport.clone(),
            client_buf,
            if chunked {
                H1BodyKind::Chunked
            } else {
                H1BodyKind::ContentLength(content_len)
            },
        );

        let res_handle = Responder::new(transport.clone());

        let resp = driver
            .handle(req, &mut req_body, res_handle)
            .await
            .wrap_err("handling request")?;

        // TODO: if we sent `connection: close` we should close now
        _ = resp;

        client_buf = req_body
            .into_buf()
            .ok_or_else(|| eyre::eyre!("request body not drained, have to close connection"))?;

        if connection_close {
            debug!("client requested connection close");
            return Ok(ServeOutcome::ClientRequestedConnectionClose);
        }
    }
}

pub trait ResponseState {}

pub struct ExpectResponseHeaders;
impl ResponseState for ExpectResponseHeaders {}

pub struct ExpectResponseBody {
    mode: BodyWriteMode,
}
impl ResponseState for ExpectResponseBody {}

pub struct ResponseDone;
impl ResponseState for ResponseDone {}

pub struct Responder<T, S>
where
    S: ResponseState,
    T: WriteOwned,
{
    #[allow(dead_code)]
    state: S,
    transport: Rc<T>,
}

impl<T> Responder<T, ExpectResponseHeaders>
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
    pub async fn write_interim_response(&mut self, res: Response) -> eyre::Result<()> {
        if !res.status.is_informational() {
            return Err(eyre::eyre!("interim response must have status code 1xx"));
        }

        self.write_response_internal(res).await?;
        Ok(())
    }

    /// Send the final response headers
    /// Errors out if the response status is < 200.
    /// Errors out if the client sent `expect: 100-continue`
    pub async fn write_final_response(
        mut self,
        res: Response,
    ) -> eyre::Result<Responder<T, ExpectResponseBody>> {
        let mode = if res.headers.content_length().is_some() {
            if !res.headers.is_chunked_transfer_encoding() {
                BodyWriteMode::ContentLength
            } else {
                BodyWriteMode::Chunked
            }
        } else {
            BodyWriteMode::Chunked
        };

        if res.status.is_informational() {
            return Err(eyre::eyre!("final response must have status code >= 200"));
        }

        self.write_response_internal(res).await?;
        Ok(Responder {
            state: ExpectResponseBody { mode },
            transport: self.transport,
        })
    }

    /// Writes a response with the given body
    pub async fn write_final_response_with_body(
        self,
        res: Response,
        body: &mut impl Body,
    ) -> eyre::Result<Responder<T, ResponseDone>> {
        let mut this = self.write_final_response(res).await?;

        loop {
            match body.next_chunk().await? {
                BodyChunk::Chunk(chunk) => {
                    debug!("proxying chunk of length {}", chunk.len());
                    this.write_chunk(chunk).await?;
                }
                BodyChunk::Done { trailers } => {
                    debug!("done proxying body");
                    // TODO: should we do something here in case of
                    // content-length mismatches?
                    return this.finish_body(trailers).await;
                }
            }
        }
    }

    async fn write_response_internal(&mut self, res: Response) -> eyre::Result<()> {
        let mut list = PieceList::default();
        encode_response(res, &mut list)?;

        let list = write_all_list(self.transport.as_ref(), list)
            .await
            .wrap_err("writing response headers upstream")?;

        // TODO: can we re-use that list? pool it?
        drop(list);

        Ok(())
    }
}

impl<T> Responder<T, ExpectResponseBody>
where
    T: WriteOwned,
{
    /// Send a response body chunk. Errors out if sending more than the
    /// announced content-length.
    pub async fn write_chunk(&mut self, chunk: Piece) -> eyre::Result<()> {
        super::body::write_h1_body_chunk(self.transport.as_ref(), chunk, self.state.mode).await?;
        Ok(())
    }

    /// Finish the body, with optional trailers, cf. https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/TE
    /// Errors out if the sent body doesn't match the announced content-length.
    /// Errors out if trailers that weren't announced are being sent, or if the client
    /// didn't explicitly announce it accepted trailers, or if the response is a 204,
    /// 205 or 304, or if the body wasn't sent with chunked transfer encoding.
    pub async fn finish_body(
        self,
        trailers: Option<Box<Headers>>,
    ) -> eyre::Result<Responder<T, ResponseDone>> {
        super::body::write_h1_body_end(self.transport.as_ref(), self.state.mode).await?;

        if let Some(trailers) = trailers {
            // TODO: check all preconditions
            let mut list = PieceList::default();
            encode_headers(*trailers, &mut list)?;

            let list = write_all_list(self.transport.as_ref(), list)
                .await
                .wrap_err("writing response headers upstream")?;

            // TODO: can we re-use that list? pool it?
            drop(list);
        }

        // TODO: check announced content-length size vs actual, etc.

        Ok(Responder {
            state: ResponseDone,
            transport: self.transport,
        })
    }
}
