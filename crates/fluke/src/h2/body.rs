use core::fmt;

use tokio::sync::mpsc;

use crate::{Body, BodyChunk, Headers};
use fluke_buffet::Piece;

pub(crate) enum PieceOrTrailers {
    Piece(Piece),
    Trailers(Box<Headers>),
}

pub(crate) struct StreamIncoming {
    // TODO: don't allow access to tx, check against capacity first?
    pub(crate) tx: mpsc::Sender<StreamIncomingItem>,

    // incoming capacity (that we decide, we get to tell
    // the peer how much we can handle with window updates)
    pub(crate) capacity: i64,
}

// FIXME: don't use eyre, do proper error handling
pub(crate) type StreamIncomingItem = eyre::Result<PieceOrTrailers>;

#[derive(Debug)]
pub(crate) struct H2Body {
    pub(crate) content_length: Option<u64>,
    pub(crate) eof: bool,
    // TODO: more specific error handling
    pub(crate) rx: mpsc::Receiver<StreamIncomingItem>,
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

pub(crate) struct SinglePieceBody {
    content_len: u64,
    piece: Option<Piece>,
}

impl fmt::Debug for SinglePieceBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("SinglePieceBody");
        debug_struct.field("content_len", &self.content_len);

        if let Some(piece) = &self.piece {
            match std::str::from_utf8(piece.as_ref()) {
                Ok(utf8_str) => debug_struct.field("piece", &utf8_str),
                Err(_) => debug_struct.field("piece", &"(non-utf8 string)"),
            };
        } else {
            debug_struct.field("piece", &"(none)");
        }

        debug_struct.finish()
    }
}

impl SinglePieceBody {
    pub(crate) fn new(piece: Piece) -> Self {
        let content_len = piece.len() as u64;
        Self {
            content_len,
            piece: Some(piece),
        }
    }
}

impl Body for SinglePieceBody {
    fn content_len(&self) -> Option<u64> {
        self.piece.as_ref().map(|piece| piece.len() as u64)
    }

    fn eof(&self) -> bool {
        self.piece.is_none()
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        tracing::trace!( has_piece = %self.piece.is_some(), "SinglePieceBody::next_chunk");
        if let Some(piece) = self.piece.take() {
            Ok(BodyChunk::Chunk(piece))
        } else {
            Ok(BodyChunk::Done { trailers: None })
        }
    }
}
