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

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StreamIncomingError {
    #[error("stream reset")]
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

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum H2BodyError {
    #[error("Stream reset")]
    StreamReset,
}

impl AsRef<dyn std::error::Error> for H2BodyError {
    fn as_ref(&self) -> &(dyn std::error::Error + 'static) {
        self
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
