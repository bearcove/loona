use std::{fmt, rc::Rc};

use tracing::debug;

use crate::{
    util::{read_and_parse, write_all_list},
    Body, BodyChunk, BodyErrorReason, IoChunk, IoChunkList, ReadOwned, RollMut, WriteOwned,
};

/// An HTTP/1.1 body, either chunked or content-length.
pub(crate) struct H1Body<T> {
    transport: Rc<T>,
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
    pub(crate) fn new(transport: Rc<T>, buf: RollMut, kind: H1BodyKind) -> Self {
        let state = match kind {
            H1BodyKind::Chunked => Decoder::Chunked(ChunkedDecoder::ReadingChunkHeader),
            H1BodyKind::ContentLength(len) => {
                Decoder::ContentLength(ContentLengthDecoder { len, read: 0 })
            }
        };
        H1Body {
            transport,
            buf: Some(buf),
            state,
        }
    }

    /// Returns the inner buffer, but only if the body has been
    /// fully read.
    pub(crate) fn into_buf(self) -> Option<RollMut> {
        if !self.eof() {
            return None;
        }
        self.buf
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
            Decoder::Chunked(state) => {
                state
                    .next_chunk(&mut self.buf, self.transport.as_ref())
                    .await
            }
            Decoder::ContentLength(state) => {
                state
                    .next_chunk(&mut self.buf, self.transport.as_ref())
                    .await
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
pub(crate) enum BodyWriteMode {
    Chunked,
    ContentLength,
}

pub(crate) async fn write_h1_body(
    transport: Rc<impl WriteOwned>,
    body: &mut impl Body,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    loop {
        match body.next_chunk().await? {
            BodyChunk::Chunk(chunk) => write_h1_body_chunk(transport.as_ref(), chunk, mode).await?,
            BodyChunk::Done { .. } => {
                // TODO: check that we've sent what we announced in terms of
                // content length
                write_h1_body_end(transport.as_ref(), mode).await?;
                break;
            }
        }
    }

    Ok(())
}

pub(crate) async fn write_h1_body_chunk(
    transport: &impl WriteOwned,
    chunk: IoChunk,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            let mut list = IoChunkList::default();
            list.push(format!("{:x}\r\n", chunk.len()).into_bytes());
            list.push_chunk(chunk);
            list.push("\r\n");

            let list = write_all_list(transport, list).await?;
            drop(list);
        }
        BodyWriteMode::ContentLength => {
            let (res, _) = transport.write_all(chunk).await;
            res?;
        }
    }
    Ok(())
}

pub(crate) async fn write_h1_body_end(
    transport: &impl WriteOwned,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            let mut list = IoChunkList::default();
            list.push("0\r\n\r\n");
            _ = write_all_list(transport, list).await?;
        }
        BodyWriteMode::ContentLength => {
            // nothing to do
        }
    }
    Ok(())
}
