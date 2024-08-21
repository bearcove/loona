use core::fmt;

use tokio::sync::mpsc;

use crate::{Body, BodyChunk, Headers};
use buffet::Piece;

use super::types::H2StreamError;

/// Something we receive from an http/2 peer: pieces of the request
/// body, the final trailers, or perhaps an error! if the client doesn't
/// end up sending exactly the number of bytes they promised.
pub(crate) enum IncomingMessage {
    Piece(Piece),
    Trailers(Box<Headers>),
}

pub(crate) enum ChunkPosition {
    NotLast,
    Last,
}

pub(crate) struct StreamIncoming {
    tx: mpsc::Sender<IncomingMessageResult>,

    // total bytes received, which we keep track of, because if the client
    // announces a content-length and sends fewer or more bytes, we will
    // error out.
    pub(crate) total_received: u64,
    pub(crate) content_length: Option<u64>,

    // incoming capacity (that we decide, we get to tell
    // the peer how much we can handle with window updates)
    pub(crate) capacity: i64,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum StreamIncomingError {
    StreamReset,
}

impl StreamIncoming {
    pub(crate) fn new(
        initial_window_size: u32,
        content_length: Option<u64>,
        tx: mpsc::Sender<IncomingMessageResult>,
    ) -> Self {
        Self {
            tx,
            total_received: 0,
            content_length,
            capacity: initial_window_size as i64,
        }
    }

    pub(crate) async fn write_chunk(
        &mut self,
        chunk: Piece,
        which: ChunkPosition,
    ) -> Result<(), H2StreamError> {
        match self.total_received.checked_add(chunk.len() as u64) {
            Some(new_total) => {
                self.total_received = new_total;
            }
            None => return Err(H2StreamError::OverflowWhileCalculatingContentLength),
        }

        if let Some(content_length) = self.content_length {
            if self.total_received > content_length {
                return Err(H2StreamError::DataLengthDoesNotMatchContentLength {
                    data_length: self.total_received,
                    content_length,
                });
            }

            if matches!(which, ChunkPosition::Last) && self.total_received != content_length {
                return Err(H2StreamError::DataLengthDoesNotMatchContentLength {
                    data_length: self.total_received,
                    content_length,
                });
            }
        }

        if self
            .tx
            .send(Ok(IncomingMessage::Piece(chunk)))
            .await
            .is_err()
        {
            // the stream is being ignored, so let's reset it
            return Err(H2StreamError::Cancel);
        }
        Ok(())
    }

    pub(crate) async fn write_trailers(&mut self, trailers: Headers) -> Result<(), H2StreamError> {
        if let Some(content_length) = self.content_length {
            if self.total_received != content_length {
                return Err(H2StreamError::DataLengthDoesNotMatchContentLength {
                    data_length: self.total_received,
                    content_length,
                });
            }
        }

        let _ = self
            .tx
            .send(Ok(IncomingMessage::Trailers(Box::new(trailers))))
            .await;

        // TODO: keep track of what we've sent, panic if we're not in the right state.

        Ok(())
    }

    pub(crate) async fn send_error(&mut self, err: StreamIncomingError) {
        let _ = self.tx.send(Err(err)).await;
    }
}

pub(crate) type IncomingMessageResult = Result<IncomingMessage, StreamIncomingError>;

#[derive(Debug)]
pub(crate) struct H2Body {
    pub(crate) content_length: Option<u64>,
    pub(crate) eof: bool,
    pub(crate) rx: mpsc::Receiver<IncomingMessageResult>,
}

#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum H2BodyError {
    StreamReset,

    UnexpectedEof,
}

impl fmt::Display for H2BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for H2BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl Body for H2Body {
    type Error = H2BodyError;

    fn content_len(&self) -> Option<u64> {
        self.content_length
    }

    fn eof(&self) -> bool {
        self.eof
    }

    async fn next_chunk(&mut self) -> Result<BodyChunk, H2BodyError> {
        let chunk = if self.eof {
            BodyChunk::Done { trailers: None }
        } else {
            match self.rx.recv().await {
                Some(msg) => match msg {
                    Ok(IncomingMessage::Piece(piece)) => BodyChunk::Chunk(piece),
                    Ok(IncomingMessage::Trailers(trailers)) => {
                        self.eof = true;
                        BodyChunk::Done {
                            trailers: Some(trailers),
                        }
                    }
                    Err(StreamIncomingError::StreamReset) => return Err(H2BodyError::StreamReset),
                },
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
    type Error = std::convert::Infallible;

    fn content_len(&self) -> Option<u64> {
        Some(self.content_len)
    }

    fn eof(&self) -> bool {
        self.piece.is_none()
    }

    async fn next_chunk(&mut self) -> Result<BodyChunk, Self::Error> {
        tracing::trace!( has_piece = %self.piece.is_some(), "SinglePieceBody::next_chunk");
        if let Some(piece) = self.piece.take() {
            Ok(BodyChunk::Chunk(piece))
        } else {
            Ok(BodyChunk::Done { trailers: None })
        }
    }
}
