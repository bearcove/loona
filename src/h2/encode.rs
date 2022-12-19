use std::fmt;

use tokio::sync::mpsc;

use crate::{h1, Encoder, Piece, Response};

use super::parse::{Frame, StreamId};

pub(crate) enum H2ConnEvent {
    ClientFrame(Frame, Piece),
    ServerEvent(H2Event),
    AcknowledgeSettings,
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

pub struct H2Encoder {
    pub(crate) stream_id: StreamId,
    pub(crate) tx: mpsc::Sender<H2ConnEvent>,
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
        self.send(H2EventPayload::Headers(res)).await?;
        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_chunk(
        &mut self,
        chunk: crate::Piece,
        _mode: h1::body::BodyWriteMode,
    ) -> eyre::Result<()> {
        self.send(H2EventPayload::BodyChunk(chunk)).await?;
        Ok(())
    }

    // TODO: BodyWriteMode is not relevant for h2
    async fn write_body_end(&mut self, _mode: h1::body::BodyWriteMode) -> eyre::Result<()> {
        self.send(H2EventPayload::BodyEnd).await?;
        Ok(())
    }

    // TODO: handle trailers
    async fn write_trailers(&mut self, _trailers: Box<crate::Headers>) -> eyre::Result<()> {
        todo!("write trailers")
    }
}
