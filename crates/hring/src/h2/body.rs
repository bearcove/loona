use tokio::sync::mpsc;

use crate::{Body, BodyChunk, Headers};
use hring_buffet::Piece;

pub(crate) enum PieceOrTrailers {
    Piece(Piece),
    Trailers(Box<Headers>),
}

pub(crate) type H2BodySender = mpsc::Sender<H2BodyItem>;
pub(crate) type H2BodyItem = eyre::Result<PieceOrTrailers>;

#[derive(Debug)]
pub(crate) struct H2Body {
    pub(crate) content_length: Option<u64>,
    pub(crate) eof: bool,
    // TODO: more specific error handling
    pub(crate) rx: mpsc::Receiver<H2BodyItem>,
}

impl Body for H2Body {
    fn content_len(&self) -> Option<u64> {
        self.content_length
    }

    fn eof(&self) -> bool {
        self.eof
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        let chunk = if self.eof {
            BodyChunk::Done { trailers: None }
        } else {
            match self.rx.recv().await {
                Some(maybe_piece_or_trailers) => match maybe_piece_or_trailers? {
                    PieceOrTrailers::Piece(piece) => BodyChunk::Chunk(piece),
                    PieceOrTrailers::Trailers(trailers) => {
                        self.eof = true;
                        BodyChunk::Done {
                            trailers: Some(trailers),
                        }
                    }
                },
                // TODO: handle trailers
                None => {
                    self.eof = true;
                    BodyChunk::Done { trailers: None }
                }
            }
        };
        Ok(chunk)
    }
}
