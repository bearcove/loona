use b_x::{BxForResults, BX};
use httpwg_harness::SAMPLE_4K_BLOCK;

use buffet::Piece;
use loona::{
    error::NeverError, http::StatusCode, Body, BodyChunk, Encoder, ExpectResponseHeaders,
    HeadersExt, Responder, Response, ResponseDone, ServerDriver, SinglePieceBody,
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
        if req.headers.expects_100_continue() {
            res.write_interim_response(Response {
                status: StatusCode::CONTINUE,
                ..Default::default()
            })
            .await?;
        }

        let parts = req
            .uri
            .path()
            .trim_start_matches('/')
            .split('/')
            .collect::<Vec<_>>();

        let res = match parts.as_slice() {
            ["echo-body"] => res
                .write_final_response_with_body(
                    Response {
                        status: StatusCode::OK,
                        ..Default::default()
                    },
                    req_body,
                )
                .await
                .bx()?,
            ["status", code] => {
                drain_body(req_body).await?;

                let code = code.parse::<u16>().bx()?;
                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::from_u16(code).bx()?,
                        ..Default::default()
                    },
                    &mut (),
                )
                .await
                .bx()?
            }
            ["repeat-4k-blocks", repeat] => {
                drain_body(req_body).await?;

                let repeat = repeat.parse::<usize>().bx()?;
                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::OK,
                        ..Default::default()
                    },
                    &mut RepeatBody {
                        piece: SAMPLE_4K_BLOCK.into(),
                        n: repeat,
                        written: 0,
                    },
                )
                .await
                .bx()?
            }
            ["stream-file", name] => {
                drain_body(req_body).await?;
                let _ = name;

                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        ..Default::default()
                    },
                    &mut SinglePieceBody::from("/stream-file: not implemented yet"),
                )
                .await
                .bx()?
            }
            _ => {
                drain_body(req_body).await?;

                // return a 404
                res.write_final_response_with_body(
                    Response {
                        status: StatusCode::NOT_FOUND,
                        ..Default::default()
                    },
                    &mut SinglePieceBody::from("404 Not Found"),
                )
                .await
                .bx()?
            }
        };
        Ok(res)
    }
}

async fn drain_body(body: &mut impl Body) -> Result<(), BX> {
    let mut req_body_len = 0;
    loop {
        let chunk = body.next_chunk().await.bx()?;
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
    Ok(())
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
