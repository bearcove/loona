use std::rc::Rc;

use eyre::Context;
use http::header;
use tracing::debug;

use crate::{
    buffet::PieceList,
    h1::{
        body::BodyWriteMode,
        encode::{encode_headers, encode_response},
    },
    io::WriteOwned,
    util::write_all_list,
    Body, BodyChunk, Headers, HeadersExt, Piece, Response,
};

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
    pub(crate) fn new(transport: Rc<T>) -> Self {
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
        mut res: Response,
    ) -> eyre::Result<Responder<T, ExpectResponseBody>> {
        if res.status.is_informational() {
            return Err(eyre::eyre!("final response must have status code >= 200"));
        }

        let mode = if res.means_empty_body() {
            // do nothing
            BodyWriteMode::Empty
        } else {
            match res.headers.content_length() {
                Some(0) => BodyWriteMode::Empty,
                Some(len) => {
                    // TODO: can probably save that heap allocation
                    res.headers
                        .insert(header::CONTENT_LENGTH, format!("{len}").into_bytes().into());
                    BodyWriteMode::ContentLength
                }
                None => {
                    res.headers
                        .insert(header::TRANSFER_ENCODING, "chunked".into());
                    BodyWriteMode::Chunked
                }
            }
        };
        self.write_response_internal(res).await?;

        Ok(Responder {
            state: ExpectResponseBody { mode },
            transport: self.transport,
        })
    }

    /// Writes a response with the given body. Sets `content-length` or
    /// `transfer-encoding` as needed.
    pub async fn write_final_response_with_body(
        self,
        mut res: Response,
        body: &mut impl Body,
    ) -> eyre::Result<Responder<T, ResponseDone>> {
        if let Some(clen) = body.content_len() {
            res.headers
                .entry(header::CONTENT_LENGTH)
                .or_insert_with(|| {
                    // TODO: can probably get rid of this heap allocation, also
                    // use `itoa`
                    format!("{clen}").into_bytes().into()
                });
        }

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
        crate::h1::body::write_h1_body_chunk(self.transport.as_ref(), chunk, self.state.mode)
            .await?;
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
        crate::h1::body::write_h1_body_end(self.transport.as_ref(), self.state.mode).await?;

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
