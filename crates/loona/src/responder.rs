use std::fmt;

use buffet::Piece;
use http::{header, StatusCode};

use crate::{Body, BodyChunk, Headers, HeadersExt, Response};

pub trait ResponseState {}

pub struct ExpectResponseHeaders;
impl ResponseState for ExpectResponseHeaders {}

pub struct ExpectResponseBody {
    pub announced_content_length: Option<u64>,
    pub bytes_written: u64,
}
impl ResponseState for ExpectResponseBody {}

pub struct ResponseDone;
impl ResponseState for ResponseDone {}

#[derive(Debug)]
#[non_exhaustive]
pub enum ResponderError<EncoderError> {
    /// The response status code was not 1xx
    InterimResponseMustHaveStatusCode1xx {
        actual: StatusCode,
    },

    /// The response status code was not >= 200
    FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 {
        actual: StatusCode,
    },

    BodyLengthDoesNotMatchAnnouncedContentLength {
        actual: u64,
        expected: u64,
    },

    /// Got an encoder error
    EncoderError(EncoderError),
}

impl<E> fmt::Display for ResponderError<E>
where
    E: std::error::Error,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InterimResponseMustHaveStatusCode1xx { actual } => {
                write!(
                    f,
                    "interim response must have status code 1xx, got {actual}"
                )
            }
            Self::FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 { actual } => {
                write!(
                    f,
                    "final response must have status code >= 200, got {actual}"
                )
            }
            Self::BodyLengthDoesNotMatchAnnouncedContentLength { actual, expected } => {
                write!(
                    f,
                    "body length does not match announced content length: actual {actual}, expected {expected}"
                )
            }
            Self::EncoderError(e) => write!(f, "encoder error: {e}"),
        }
    }
}

impl<E> std::error::Error for ResponderError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EncoderError(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ResponderOrBodyError<EncoderError, BodyError> {
    Responder(ResponderError<EncoderError>),
    Body(BodyError),
}

impl<EncoderError, BodyError> fmt::Display for ResponderOrBodyError<EncoderError, BodyError>
where
    EncoderError: fmt::Debug,
    BodyError: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Responder(e) => write!(f, "{e:?}"),
            Self::Body(e) => write!(f, "{e:?}"),
        }
    }
}

impl<EncoderError, BodyError> std::error::Error for ResponderOrBodyError<EncoderError, BodyError>
where
    EncoderError: std::error::Error + 'static,
    BodyError: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Responder(e) => Some(e),
            Self::Body(e) => Some(e),
        }
    }
}

pub struct Responder<TheirEncoder, OurResponseState>
where
    TheirEncoder: Encoder,
    OurResponseState: ResponseState,
{
    encoder: TheirEncoder,
    state: OurResponseState,
}

impl<TheirEncoder> Responder<TheirEncoder, ExpectResponseHeaders>
where
    TheirEncoder: Encoder,
{
    pub fn new(encoder: TheirEncoder) -> Self {
        Self {
            encoder,
            state: ExpectResponseHeaders,
        }
    }

    /// Send an informational status code, cf. <https://httpwg.org/specs/rfc9110.html#status.1xx>
    /// Errors out if the response status is not 1xx
    pub async fn write_interim_response(
        &mut self,
        res: Response,
    ) -> Result<(), ResponderError<TheirEncoder::Error>> {
        if !res.status.is_informational() {
            return Err(ResponderError::InterimResponseMustHaveStatusCode1xx {
                actual: res.status,
            });
        }

        self.encoder
            .write_response(res)
            .await
            .map_err(ResponderError::EncoderError)?;
        Ok(())
    }

    async fn write_final_response_internal(
        mut self,
        res: Response,
        announced_content_length: Option<u64>,
    ) -> Result<Responder<TheirEncoder, ExpectResponseBody>, ResponderError<TheirEncoder::Error>>
    {
        if res.status.is_informational() {
            return Err(
                ResponderError::FinalResponseMustHaveStatusCodeGreaterThanOrEqualTo200 {
                    actual: res.status,
                },
            );
        }
        self.encoder
            .write_response(res)
            .await
            .map_err(ResponderError::EncoderError)?;
        Ok(Responder {
            state: ExpectResponseBody {
                announced_content_length,
                bytes_written: 0,
            },
            encoder: self.encoder,
        })
    }

    /// Send the final response headers
    /// Errors out if the response status is < 200.
    /// Errors out if the client sent `expect: 100-continue`
    pub async fn write_final_response(
        self,
        res: Response,
    ) -> ResponderResult<Responder<TheirEncoder, ExpectResponseBody>, TheirEncoder::Error> {
        let announced_content_length = res.headers.content_length();
        self.write_final_response_internal(res, announced_content_length)
            .await
    }

    /// Writes a response with the given body. Sets `content-length` or
    /// `transfer-encoding` as needed.
    pub async fn write_final_response_with_body<TheirBody>(
        self,
        mut res: Response,
        body: &mut TheirBody,
    ) -> Result<
        Responder<TheirEncoder, ResponseDone>,
        ResponderOrBodyError<TheirEncoder::Error, TheirBody::Error>,
    >
    where
        TheirBody: Body,
    {
        if let Some(clen) = body.content_len() {
            res.headers
                .entry(header::CONTENT_LENGTH)
                .or_insert_with(|| {
                    // TODO: can probably get rid of this heap allocation, also
                    // use `itoa`, cf. https://docs.rs/itoa/latest/itoa/
                    // TODO: also, for `write_final_response`, we could avoid
                    // parsing the content-length header, since we have the
                    // content-length already.
                    // TODO: also, instead of doing a heap allocation with a `Vec<u8>`, we
                    // could probably take a tiny bit of `RollMut` which is cheaper to
                    // allocate (or at least was, last I measured).
                    format!("{clen}").into_bytes().into()
                });
        }

        let mut this = self
            .write_final_response(res)
            .await
            .map_err(ResponderOrBodyError::Responder)?;

        loop {
            match body
                .next_chunk()
                .await
                .map_err(ResponderOrBodyError::Body)?
            {
                BodyChunk::Chunk(chunk) => {
                    this.write_chunk(chunk)
                        .await
                        .map_err(ResponderOrBodyError::Responder)?;
                }
                BodyChunk::Done { trailers } => {
                    return this
                        .finish_body(trailers)
                        .await
                        .map_err(ResponderOrBodyError::Responder);
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
    #[inline]
    pub async fn write_chunk(&mut self, chunk: Piece) -> ResponderResult<(), E::Error> {
        self.state.bytes_written += chunk.len() as u64;
        self.encoder
            .write_body_chunk(chunk)
            .await
            .map_err(ResponderError::EncoderError)
    }

    /// Finish the body, with optional trailers, cf. <https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/TE>
    /// Errors out if the sent body doesn't match the announced content-length.
    /// Errors out if trailers that weren't announced are being sent, or if the
    /// client didn't explicitly announce it accepted trailers, or if the
    /// response is a 204, 205 or 304, or if the body wasn't sent with
    /// chunked transfer encoding.
    pub async fn finish_body(
        mut self,
        trailers: Option<Box<Headers>>,
    ) -> ResponderResult<Responder<E, ResponseDone>, E::Error> {
        if let Some(announced_content_length) = self.state.announced_content_length {
            if self.state.bytes_written != announced_content_length {
                return Err(
                    ResponderError::BodyLengthDoesNotMatchAnnouncedContentLength {
                        actual: self.state.bytes_written,
                        expected: announced_content_length,
                    },
                );
            }
        }
        self.encoder
            .write_body_end()
            .await
            .map_err(ResponderError::EncoderError)?;

        if let Some(trailers) = trailers {
            self.encoder
                .write_trailers(trailers)
                .await
                .map_err(ResponderError::EncoderError)?;
        }

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

pub type ResponderResult<T, EncoderError> = Result<T, ResponderError<EncoderError>>;

#[allow(async_fn_in_trait)] // we never require Send
pub trait Encoder {
    type Error: std::error::Error;

    async fn write_response(&mut self, res: Response) -> Result<(), Self::Error>;
    /// Note: encoders do not have a duty to check for matching content-length:
    /// the responder takes care of that for HTTP/1.1 and HTTP/2
    async fn write_body_chunk(&mut self, chunk: Piece) -> Result<(), Self::Error>;
    async fn write_body_end(&mut self) -> Result<(), Self::Error>;
    async fn write_trailers(&mut self, trailers: Box<Headers>) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use buffet::Piece;
    use http::{StatusCode, Version};

    #[derive(Clone, Copy)]
    struct MockEncoder;

    impl Encoder for MockEncoder {
        type Error = std::convert::Infallible;

        async fn write_response(&mut self, _: Response) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn write_body_chunk(&mut self, _: Piece) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn write_body_end(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn write_trailers(&mut self, _: Box<Headers>) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_content_length_mismatch() {
        let encoder = MockEncoder;
        for version in [Version::HTTP_11, Version::HTTP_2] {
            let mut res = Response {
                status: StatusCode::OK,
                version,
                headers: Default::default(),
            };
            res.headers.insert(header::CONTENT_LENGTH, "10".into());

            // Test writing fewer bytes than announced
            let mut responder = Responder::new(encoder)
                .write_final_response(res.clone())
                .await
                .unwrap();
            responder.write_chunk(b"12345".into()).await.unwrap();
            let result = responder.finish_body(None).await;
            assert!(result.is_err());
            assert!(matches!(result, Err(e) if e.to_string().contains("content-length mismatch")));

            // Test writing more bytes than announced
            let encoder = MockEncoder;
            let mut responder = Responder::new(encoder)
                .write_final_response(res)
                .await
                .unwrap();
            responder.write_chunk(b"12345678901".into()).await.unwrap();
            let result = responder.finish_body(None).await;
            assert!(result.is_err());
            assert!(matches!(result, Err(e) if e.to_string().contains("content-length mismatch")));
        }
    }
}
