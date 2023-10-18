use http::header;

use crate::{h1::body::BodyWriteMode, Body, BodyChunk, Headers, HeadersExt, Response};
use fluke_buffet::Piece;

pub trait ResponseState {}

pub struct ExpectResponseHeaders;
impl ResponseState for ExpectResponseHeaders {}

pub struct ExpectResponseBody {
    mode: BodyWriteMode,
}
impl ResponseState for ExpectResponseBody {}

pub struct ResponseDone;
impl ResponseState for ResponseDone {}

pub struct Responder<E, S>
where
    E: Encoder,
    S: ResponseState,
{
    pub(crate) encoder: E,
    pub(crate) state: S,
}

impl<E> Responder<E, ExpectResponseHeaders>
where
    E: Encoder,
{
    /// Send an informational status code, cf. https://httpwg.org/specs/rfc9110.html#status.1xx
    /// Errors out if the response status is not 1xx
    pub async fn write_interim_response(&mut self, res: Response) -> eyre::Result<()> {
        if !res.status.is_informational() {
            return Err(eyre::eyre!("interim response must have status code 1xx"));
        }

        self.encoder.write_response(res).await?;
        Ok(())
    }

    /// Send the final response headers
    /// Errors out if the response status is < 200.
    /// Errors out if the client sent `expect: 100-continue`
    pub async fn write_final_response(
        mut self,
        mut res: Response,
    ) -> eyre::Result<Responder<E, ExpectResponseBody>> {
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
        self.encoder.write_response(res).await?;

        Ok(Responder {
            state: ExpectResponseBody { mode },
            encoder: self.encoder,
        })
    }

    /// Writes a response with the given body. Sets `content-length` or
    /// `transfer-encoding` as needed.
    pub async fn write_final_response_with_body(
        self,
        mut res: Response,
        body: &mut impl Body,
    ) -> eyre::Result<Responder<E, ResponseDone>> {
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
                    this.write_chunk(chunk).await?;
                }
                BodyChunk::Done { trailers } => {
                    // TODO: should we do something here in case of
                    // content-length mismatches?
                    return this.finish_body(trailers).await;
                }
            }
        }
    }
}

impl<E> Responder<E, ExpectResponseBody>
where
    E: Encoder,
{
    /// Send a response body chunk. Errors out if sending more than the
    /// announced content-length.
    pub async fn write_chunk(&mut self, chunk: Piece) -> eyre::Result<()> {
        self.encoder.write_body_chunk(chunk, self.state.mode).await
    }

    /// Finish the body, with optional trailers, cf. https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/TE
    /// Errors out if the sent body doesn't match the announced content-length.
    /// Errors out if trailers that weren't announced are being sent, or if the client
    /// didn't explicitly announce it accepted trailers, or if the response is a 204,
    /// 205 or 304, or if the body wasn't sent with chunked transfer encoding.
    pub async fn finish_body(
        mut self,
        trailers: Option<Box<Headers>>,
    ) -> eyre::Result<Responder<E, ResponseDone>> {
        self.encoder.write_body_end(self.state.mode).await?;

        if let Some(trailers) = trailers {
            self.encoder.write_trailers(trailers).await?;
        }

        // TODO: check announced content-length size vs actual, etc.

        Ok(Responder {
            state: ResponseDone,
            encoder: self.encoder,
        })
    }
}

impl<E> Responder<E, ResponseDone>
where
    E: Encoder,
{
    pub fn into_inner(self) -> E {
        self.encoder
    }
}

#[allow(async_fn_in_trait)] // we never require Send
pub trait Encoder {
    async fn write_response(&mut self, res: Response) -> eyre::Result<()>;
    async fn write_body_chunk(&mut self, chunk: Piece, mode: BodyWriteMode) -> eyre::Result<()>;
    async fn write_body_end(&mut self, mode: BodyWriteMode) -> eyre::Result<()>;
    async fn write_trailers(&mut self, trailers: Box<Headers>) -> eyre::Result<()>;
}
