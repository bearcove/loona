use buffet::Piece;
use http::{StatusCode, Version};
use tokio::sync::mpsc;
use tracing::debug;

use super::types::{H2Event, H2EventPayload};
use crate::{Encoder, Response};
use loona_h2::StreamId;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum EncoderState {
    ExpectResponseHeaders,
    ExpectResponseBody,
    ResponseDone,
}

/// Encodes HTTP/2 responses and bodies
pub(crate) struct H2Encoder {
    stream_id: StreamId,
    tx: mpsc::Sender<H2Event>,
    state: EncoderState,
}

impl H2Encoder {
    pub(crate) fn new(stream_id: StreamId, tx: mpsc::Sender<H2Event>) -> Self {
        Self {
            stream_id,
            tx,
            state: EncoderState::ExpectResponseHeaders,
        }
    }

    fn event(&self, payload: H2EventPayload) -> H2Event {
        H2Event {
            payload,
            stream_id: self.stream_id,
        }
    }

    async fn send(&self, payload: H2EventPayload) -> Result<(), H2EncoderError> {
        self.tx
            .send(self.event(payload))
            .await
            .map_err(|_| H2EncoderError::StreamReset)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum H2EncoderError {
    /// The encoder is in the wrong state
    #[error("Wrong state: expected {expected:?}, actual {actual:?}")]
    WrongState {
        expected: EncoderState,
        actual: EncoderState,
    },

    #[error("Stream reset")]
    StreamReset,
}

impl AsRef<dyn std::error::Error> for H2EncoderError {
    fn as_ref(&self) -> &(dyn std::error::Error + 'static) {
        self
    }
}

impl Encoder for H2Encoder {
    type Error = H2EncoderError;

    async fn write_response(&mut self, res: Response) -> Result<(), Self::Error> {
        // FIXME: HTTP/2 _does_ support informational responses, cf. https://github.com/bearcove/loona/issues/190
        assert!(
            !res.status.is_informational(),
            "http/2 does not support informational responses"
        );

        if self.state != EncoderState::ExpectResponseHeaders {
            return Err(H2EncoderError::WrongState {
                expected: EncoderState::ExpectResponseHeaders,
                actual: self.state,
            });
        }

        self.send(H2EventPayload::Headers(res)).await?;
        self.state = EncoderState::ExpectResponseBody;

        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_chunk(&mut self, chunk: Piece) -> Result<(), Self::Error> {
        if self.state != EncoderState::ExpectResponseBody {
            return Err(H2EncoderError::WrongState {
                expected: EncoderState::ExpectResponseBody,
                actual: self.state,
            });
        }

        self.send(H2EventPayload::BodyChunk(chunk)).await?;
        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_end(&mut self) -> Result<(), Self::Error> {
        if self.state != EncoderState::ExpectResponseBody {
            return Err(H2EncoderError::WrongState {
                expected: EncoderState::ExpectResponseBody,
                actual: self.state,
            });
        }

        self.send(H2EventPayload::BodyEnd).await?;
        self.state = EncoderState::ResponseDone;

        Ok(())
    }

    // TODO: handle trailers
    async fn write_trailers(&mut self, _trailers: Box<crate::Headers>) -> Result<(), Self::Error> {
        if self.state != EncoderState::ResponseDone {
            return Err(H2EncoderError::WrongState {
                expected: EncoderState::ResponseDone,
                actual: self.state,
            });
        }

        todo!("write trailers")
    }
}

impl Drop for H2Encoder {
    fn drop(&mut self) {
        let mut evs = vec![];

        match self.state {
            EncoderState::ExpectResponseHeaders => {
                evs.push(self.event(H2EventPayload::Headers(Response {
                    version: Version::HTTP_11,
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    headers: Default::default(),
                })));
                evs.push(self.event(H2EventPayload::BodyEnd));
            }
            EncoderState::ExpectResponseBody => {
                // TODO: this should probably be RST_STREAM instead
                evs.push(self.event(H2EventPayload::BodyEnd));
            }
            EncoderState::ResponseDone => {
                // ah, good.
            }
        }

        if !evs.is_empty() {
            let tx = self.tx.clone();
            buffet::spawn(async move {
                for ev in evs {
                    if tx.send(ev).await.is_err() {
                        debug!("could not send event to h2 connection handler");
                        break;
                    }
                }
            });
        }
    }
}
