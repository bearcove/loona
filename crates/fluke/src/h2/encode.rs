use fluke_buffet::Piece;
use http::{StatusCode, Version};
use tokio::sync::mpsc;
use tracing::debug;

use super::types::{H2Event, H2EventPayload};
use crate::{Encoder, Response};
use fluke_h2_parse::StreamId;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncoderState {
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
        assert!(
            !res.status.is_informational(),
            "http/2 does not support informational responses"
        );

        // TODO: don't panic here
        assert_eq!(self.state, EncoderState::ExpectResponseHeaders);

        self.send(H2EventPayload::Headers(res)).await?;
        self.state = EncoderState::ExpectResponseBody;

        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_chunk(&mut self, chunk: Piece) -> eyre::Result<()> {
        assert!(matches!(self.state, EncoderState::ExpectResponseBody));

        self.send(H2EventPayload::BodyChunk(chunk)).await?;
        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_end(&mut self) -> eyre::Result<()> {
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
            fluke_buffet::spawn(async move {
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
