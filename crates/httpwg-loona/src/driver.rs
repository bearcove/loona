use b_x::{BxForResults, BX};
use std::io::Write;

use buffet::{Piece, RollMut};
use loona::{
    error::NeverError,
    http::{self, StatusCode},
    Body, BodyChunk, Encoder, ExpectResponseHeaders, Responder, Response, ResponseDone,
    ServerDriver, SinglePieceBody,
};

pub(super) struct TestDriver;

impl<OurEncoder> ServerDriver<OurEncoder> for TestDriver
where
    OurEncoder: Encoder,
{
    type Error = BX;

    async fn handle(
        &self,
        req: loona::Request,
        req_body: &mut impl Body,
        mut res: Responder<OurEncoder, ExpectResponseHeaders>,
    ) -> Result<Responder<OurEncoder, ResponseDone>, Self::Error> {
        // if the client sent `expect: 100-continue`, we must send a 100 status code
        if let Some(h) = req.headers.get(http::header::EXPECT) {
            if &h[..] == b"100-continue" {
                res.write_interim_response(Response {
                    status: StatusCode::CONTINUE,
                    ..Default::default()
                })
                .await?;
            }
        }

        let res = match req.uri.path() {
            "/echo-body" => res
                .write_final_response_with_body(
                    Response {
                        status: StatusCode::OK,
                        ..Default::default()
                    },
                    req_body,
                )
                .await
                .bx()?,
            "/stream-big-body" => {
                // then read the full request body
                let mut req_body_len = 0;
                loop {
                    let chunk = req_body.next_chunk().await.bx()?;
                    match chunk {
                        BodyChunk::Done { trailers } => {
                            // yey
                            if let Some(trailers) = trailers {
                                tracing::debug!(trailers_len = %trailers.len(), "received trailers");
                            }
                            break;
                        }
                        BodyChunk::Chunk(chunk) => {
                            req_body_len += chunk.len();
                        }
                    }
                }
                tracing::debug!(%req_body_len, "read request body");

                let mut roll = RollMut::alloc().bx()?;
                for _ in 0..256 {
                    roll.write_all("this is a big chunk".as_bytes()).bx()?;
                }

                struct RepeatBody {
                    piece: Piece,
                    n: usize,
                    written: usize,
                }

                impl std::fmt::Debug for RepeatBody {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.debug_struct("RepeatBody")
                            .field("piece_len", &self.piece.len())
                            .field("n", &self.n)
                            .field("written", &self.written)
                            .finish()
                    }
                }

                impl Body for RepeatBody {
                    type Error = NeverError;

                    fn content_len(&self) -> Option<u64> {
                        Some(self.n as u64 * self.piece.len() as u64)
                    }

                    fn eof(&self) -> bool {
                        self.written == self.n
                    }

                    async fn next_chunk(&mut self) -> Result<BodyChunk, Self::Error> {
                        if self.eof() {
                            return Ok(BodyChunk::Done { trailers: None });
                        }

                        let chunk = self.piece.clone();
                        self.written += 1;
                        Ok(BodyChunk::Chunk(chunk))
                    }
                }

                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::OK,
                        ..Default::default()
                    },
                    &mut RepeatBody {
                        piece: roll.take_all().into(),
                        n: 128,
                        written: 0,
                    },
                )
                .await
                .bx()?
            }
            _ => {
                // then read the full request body
                let mut req_body_len = 0;
                loop {
                    let chunk = req_body.next_chunk().await.bx()?;
                    match chunk {
                        BodyChunk::Done { trailers } => {
                            // yey
                            if let Some(trailers) = trailers {
                                tracing::debug!(trailers_len = %trailers.len(), "received trailers");
                            }
                            break;
                        }
                        BodyChunk::Chunk(chunk) => {
                            req_body_len += chunk.len();
                        }
                    }
                }
                tracing::debug!(%req_body_len, "read request body");

                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::OK,
                        ..Default::default()
                    },
                    &mut SinglePieceBody::from("it's less dire to lose, than to lose oneself"),
                )
                .await
                .bx()?
            }
        };
        Ok(res)
    }
}
