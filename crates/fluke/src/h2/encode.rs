use std::fmt;

use http::{StatusCode, Version};
use tokio::sync::mpsc;
use tracing::warn;

use crate::{h1::body::BodyWriteMode, Encoder, Response};
use fluke_buffet::{Piece, Roll};

use super::parse::{KnownErrorCode, StreamId};

pub(crate) enum H2ConnEvent {
    Ping(Roll),
    ServerEvent(H2Event),
    AcknowledgeSettings {
        new_max_header_table_size: u32,
    },
    GoAway {
        error_code: KnownErrorCode,
        last_stream_id: StreamId,
        additional_debug_data: Piece,
    },
    RstStream {
        stream_id: StreamId,
        error_code: KnownErrorCode,
    },
}

#[derive(Debug)]
pub(crate) struct H2Event {
    pub(crate) stream_id: StreamId,
    pub(crate) payload: H2EventPayload,
}

pub(crate) enum H2EventPayload {
    Headers(Response),
    BodyChunk(Piece),
    BodyEnd,
}

impl fmt::Debug for H2EventPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Headers(_) => f.debug_tuple("Headers").finish(),
            Self::BodyChunk(_) => f.debug_tuple("BodyChunk").finish(),
            Self::BodyEnd => write!(f, "BodyEnd"),
        }
    }
}

pub(crate) enum EncoderState {
    ExpectResponseHeaders,
    ExpectResponseBody,
    ResponseDone,
}

pub struct H2Encoder {
    pub(crate) stream_id: StreamId,
    pub(crate) tx: mpsc::Sender<H2ConnEvent>,
    pub(crate) state: EncoderState,
}

impl H2Encoder {
    fn event(&self, payload: H2EventPayload) -> H2ConnEvent {
        H2ConnEvent::ServerEvent(H2Event {
            payload,
            stream_id: self.stream_id,
        })
    }

    async fn send(&self, payload: H2EventPayload) -> eyre::Result<()> {
        self.tx
            .send(self.event(payload))
            .await
            .map_err(|_| eyre::eyre!("could not send event to h2 connection handler"))?;
        Ok(())
    }
}

impl Encoder for H2Encoder {
    async fn write_response(&mut self, res: Response) -> eyre::Result<()> {
        // TODO: don't panic here
        assert!(matches!(self.state, EncoderState::ExpectResponseHeaders));

        self.send(H2EventPayload::Headers(res)).await?;
        self.state = EncoderState::ExpectResponseBody;

        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_chunk(
        &mut self,
        chunk: fluke_buffet::Piece,
        _mode: BodyWriteMode,
    ) -> eyre::Result<()> {
        assert!(matches!(self.state, EncoderState::ExpectResponseBody));

        self.send(H2EventPayload::BodyChunk(chunk)).await?;
        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_end(&mut self, _mode: BodyWriteMode) -> eyre::Result<()> {
        assert!(matches!(self.state, EncoderState::ExpectResponseBody));

        self.send(H2EventPayload::BodyEnd).await?;
        self.state = EncoderState::ResponseDone;

        Ok(())
    }

    // TODO: handle trailers
    async fn write_trailers(&mut self, _trailers: Box<crate::Headers>) -> eyre::Result<()> {
        assert!(matches!(self.state, EncoderState::ResponseDone));

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
            fluke_maybe_uring::spawn(async move {
                for ev in evs {
                    if tx.send(ev).await.is_err() {
                        warn!("could not send event to h2 connection handler");
                        break;
                    }
                }
            });
        }
    }
}
