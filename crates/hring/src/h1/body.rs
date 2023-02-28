use std::fmt;

use tracing::debug;

use crate::{util::read_and_parse, Body, BodyChunk, BodyErrorReason};
use hring_buffet::{Piece, PieceList, ReadOwned, RollMut, WriteOwned};

/// An HTTP/1.1 body, either chunked or content-length.
pub(crate) struct H1Body<T> {
    transport_r: T,
    buf: Option<RollMut>,
    state: Decoder,
}

#[derive(Debug)]
enum Decoder {
    Chunked(ChunkedDecoder),
    ContentLength(ContentLengthDecoder),
}

#[derive(Debug)]
enum ChunkedDecoder {
    ReadingChunkHeader,
    ReadingChunk { remain: u64 },

    // We've gotten one empty chunk
    Done,
}

#[derive(Debug)]
struct ContentLengthDecoder {
    len: u64,
    read: u64,
}

#[derive(Debug)]
pub(crate) enum H1BodyKind {
    Chunked,
    ContentLength(u64),
}

impl<T> fmt::Debug for H1Body<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H1Body")
            .field("state", &self.state)
            .finish()
    }
}

impl<T: ReadOwned> H1Body<T> {
    pub(crate) fn new(transport_r: T, buf: RollMut, kind: H1BodyKind) -> Self {
        let state = match kind {
            H1BodyKind::Chunked => Decoder::Chunked(ChunkedDecoder::ReadingChunkHeader),
            H1BodyKind::ContentLength(len) => {
                Decoder::ContentLength(ContentLengthDecoder { len, read: 0 })
            }
        };
        H1Body {
            transport_r,
            buf: Some(buf),
            state,
        }
    }

    /// Returns the inner buffer and transport, but only if the body has been
    /// fully read.
    pub(crate) fn into_inner(self) -> Option<(RollMut, T)> {
        if !self.eof() {
            return None;
        }
        let buf = self.buf?;
        Some((buf, self.transport_r))
    }
}

impl<T: ReadOwned> Body for H1Body<T> {
    fn content_len(&self) -> Option<u64> {
        match &self.state {
            Decoder::Chunked(_) => None,
            Decoder::ContentLength(state) => Some(state.len),
        }
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        if self.buf.is_none() {
            return Ok(BodyChunk::Done { trailers: None });
        }

        match &mut self.state {
            Decoder::Chunked(state) => state.next_chunk(&mut self.buf, &self.transport_r).await,
            Decoder::ContentLength(state) => {
                state.next_chunk(&mut self.buf, &self.transport_r).await
            }
        }
    }

    fn eof(&self) -> bool {
        match &self.state {
            Decoder::Chunked(state) => state.eof(),
            Decoder::ContentLength(state) => state.eof(),
        }
    }
}

impl ContentLengthDecoder {
    async fn next_chunk(
        &mut self,
        buf_slot: &mut Option<RollMut>,
        transport: &impl ReadOwned,
    ) -> eyre::Result<BodyChunk> {
        let remain = self.len - self.read;
        if remain == 0 {
            return Ok(BodyChunk::Done { trailers: None });
        }

        debug!(%remain, "reading content-length body");

        let mut buf = buf_slot
            .take()
            .ok_or_else(|| BodyErrorReason::CalledNextChunkAfterError.as_err())?;

        if buf.is_empty() {
            buf.reserve()?;

            let res;
            (res, buf) = buf.read_into(usize::MAX, transport).await;
            res.map_err(|e| BodyErrorReason::ErrorWhileReadingChunkData.with_cx(e))?;
        }

        let chunk = buf
            .take_at_most(remain as usize)
            .ok_or_else(|| BodyErrorReason::ClosedWhileReadingContentLength.as_err())?;
        self.read += chunk.len() as u64;
        buf_slot.replace(buf);
        Ok(BodyChunk::Chunk(chunk.into()))
    }

    fn eof(&self) -> bool {
        self.len == self.read
    }
}

impl ChunkedDecoder {
    async fn next_chunk(
        &mut self,
        buf_slot: &mut Option<RollMut>,
        transport: &impl ReadOwned,
    ) -> eyre::Result<BodyChunk> {
        loop {
            let mut buf = buf_slot
                .take()
                .ok_or_else(|| BodyErrorReason::CalledNextChunkAfterError.as_err())?;

            if let ChunkedDecoder::Done = self {
                buf_slot.replace(buf);
                // TODO: prevent misuse when calling `next_chunk` after trailers
                // were already read?
                return Ok(BodyChunk::Done { trailers: None });
            }

            if let ChunkedDecoder::ReadingChunkHeader = self {
                let (next_buf, chunk_size) =
                    read_and_parse(super::parse::chunk_size, transport, buf, 16)
                        .await
                        .map_err(|e| BodyErrorReason::InvalidChunkSize.with_cx(e))?
                        .ok_or_else(|| BodyErrorReason::ClosedWhileReadingChunkSize.as_err())?;
                buf = next_buf;

                if chunk_size == 0 {
                    // that's the final chunk, look for the final CRLF
                    let (next_buf, _) = read_and_parse(super::parse::crlf, transport, buf, 2)
                        .await
                        .map_err(|e| BodyErrorReason::InvalidChunkTerminator.with_cx(e))?
                        .ok_or_else(|| {
                            BodyErrorReason::ClosedWhileReadingChunkTerminator.as_err()
                        })?;
                    buf = next_buf;
                    *self = ChunkedDecoder::Done;
                    buf_slot.replace(buf);

                    // TODO: trailers
                    return Ok(BodyChunk::Done { trailers: None });
                }

                *self = ChunkedDecoder::ReadingChunk { remain: chunk_size }
            };

            if let ChunkedDecoder::ReadingChunk { remain } = self {
                if *remain == 0 {
                    // look for CRLF terminator
                    let (next_buf, _) = read_and_parse(super::parse::crlf, transport, buf, 2)
                        .await
                        .map_err(|e| BodyErrorReason::InvalidChunkTerminator.with_cx(e))?
                        .ok_or_else(|| {
                            BodyErrorReason::ClosedWhileReadingChunkTerminator.as_err()
                        })?;
                    buf = next_buf;
                    *self = ChunkedDecoder::ReadingChunkHeader;
                    buf_slot.replace(buf);
                    continue;
                }

                if buf.is_empty() {
                    buf.reserve()?;

                    let res;
                    (res, buf) = buf.read_into(*remain as usize, transport).await;
                    res.map_err(|e| BodyErrorReason::ErrorWhileReadingChunkData.with_cx(e))?;
                }

                let chunk = buf.take_at_most(*remain as usize);
                match chunk {
                    Some(chunk) => {
                        *remain -= chunk.len() as u64;
                        buf_slot.replace(buf);
                        return Ok(BodyChunk::Chunk(chunk.into()));
                    }
                    None => {
                        return Err(BodyErrorReason::ClosedWhileReadingChunkData.as_err().into());
                    }
                }
            } else {
                unreachable!()
            };
        }
    }

    fn eof(&self) -> bool {
        matches!(self, ChunkedDecoder::Done)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyWriteMode {
    // we're doing chunked transfer encoding
    Chunked,

    // we set a length and are writing exactly the number of bytes we promised
    ContentLength,

    // we didn't set a content-length and we're not doing chunked transfer
    // encoding, so we're not sending a body at all.
    Empty,
}

pub(crate) async fn write_h1_body(
    transport: &impl WriteOwned,
    body: &mut impl Body,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    loop {
        match body.next_chunk().await? {
            BodyChunk::Chunk(chunk) => write_h1_body_chunk(transport, chunk, mode).await?,
            BodyChunk::Done { .. } => {
                // TODO: check that we've sent what we announced in terms of
                // content length
                write_h1_body_end(transport, mode).await?;
                break;
            }
        }
    }

    Ok(())
}

pub(crate) async fn write_h1_body_chunk(
    transport: &impl WriteOwned,
    chunk: Piece,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            transport
                .writev_all(
                    PieceList::default()
                        .with(format!("{:x}\r\n", chunk.len()).into_bytes())
                        .with(chunk)
                        .with("\r\n"),
                )
                .await?;
        }
        BodyWriteMode::ContentLength => {
            transport.write_all(chunk).await?;
        }
        BodyWriteMode::Empty => {
            return Err(BodyErrorReason::CalledWriteBodyChunkWhenNoBodyWasExpected
                .as_err()
                .into());
        }
    }
    Ok(())
}

pub(crate) async fn write_h1_body_end(
    transport: &impl WriteOwned,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    debug!(?mode, "writing h1 body end");
    match mode {
        BodyWriteMode::Chunked => {
            transport.write_all("0\r\n\r\n").await?;
        }
        BodyWriteMode::ContentLength => {
            // nothing to do
        }
        BodyWriteMode::Empty => {
            // nothing to do
        }
    }
    Ok(())
}
