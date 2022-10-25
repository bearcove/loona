use std::rc::Rc;

use tracing::debug;

use crate::{
    bufpool::{AggBuf, IoChunkList, IoChunkable},
    util::{read_and_parse, write_all_list},
    Body, BodyChunk, ReadOwned, WriteOwned,
};

pub(crate) struct H1Body<T> {
    pub(crate) transport: Rc<T>,
    pub(crate) buf: Option<AggBuf>,
    pub(crate) kind: H1BodyKind,
    pub(crate) read: u64,
    pub(crate) eof: bool,
}

pub(crate) enum H1BodyKind {
    Chunked,
    ContentLength(u64),
    Empty,
}

impl<T> Body for H1Body<T>
where
    T: ReadOwned,
{
    fn content_len(&self) -> Option<u64> {
        match self.kind {
            H1BodyKind::Chunked => None,
            H1BodyKind::ContentLength(len) => Some(len),
            H1BodyKind::Empty => Some(0),
        }
    }

    async fn next_chunk(&mut self) -> eyre::Result<BodyChunk> {
        if self.eof {
            return Ok(BodyChunk::Eof);
        }

        match self.kind {
            H1BodyKind::Chunked => {
                const MAX_CHUNK_LENGTH: u32 = 1024 * 1024;

                debug!("reading chunk");
                let chunk;
                let mut buf = self.buf.take().unwrap();
                buf.write().grow_if_needed()?;

                // TODO: this reads the whole chunk, but if we don't need to maintain
                // chunk size, we don't need to buffer that far. we can just read
                // whatever, skip the CRLF, know when we need to stop to read another
                // chunk length, etc. this needs to be a state machine.
                (buf, chunk) = match read_and_parse(
                    super::parse::chunk,
                    self.transport.as_ref(),
                    buf,
                    MAX_CHUNK_LENGTH,
                )
                .await?
                {
                    Some(t) => t,
                    None => {
                        return Err(eyre::eyre!("peer went away before sending final chunk"));
                    }
                };
                debug!("read {} byte chunk", chunk.len);

                self.buf = Some(buf);

                if chunk.len == 0 {
                    debug!("received 0-length chunk, that's EOF!");
                    self.eof = true;
                    Ok(BodyChunk::Eof)
                } else {
                    self.read += chunk.len;
                    Ok(BodyChunk::AggSlice(chunk.data))
                }
            }
            H1BodyKind::ContentLength(len) => {
                let remain = len - self.read;
                if remain == 0 {
                    self.eof = true;
                    return Ok(BodyChunk::Eof);
                }

                let mut buf = self.buf.take().unwrap();

                buf.write().grow_if_needed()?;
                let mut slice = buf.write_slice().limit(remain);
                let res;
                (res, slice) = self.transport.as_ref().read(slice).await;
                buf = slice.into_inner();
                let n = res?;

                self.read += n as u64;
                let slice = buf.read().slice(0..n as u32);
                self.buf = Some(buf.split());

                Ok(BodyChunk::AggSlice(slice))
            }
            H1BodyKind::Empty => {
                self.eof = true;
                Ok(BodyChunk::Eof)
            }
        }
    }

    fn eof(&self) -> bool {
        match self.kind {
            H1BodyKind::Chunked => self.eof,
            H1BodyKind::ContentLength(len) => self.read == len,
            H1BodyKind::Empty => true,
        }
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
            BodyChunk::Buf(chunk) => write_h1_body_chunk(transport.as_ref(), chunk, mode).await?,
            BodyChunk::AggSlice(chunk) => {
                write_h1_body_chunk(transport.as_ref(), chunk, mode).await?
            }
            BodyChunk::Eof => {
                // TODO: check that we've sent what we announced in terms of
                // content length
                write_h1_body_end(transport.as_ref(), mode).await?;
                break;
            }
        }
    }

    Ok(())
}

async fn write_h1_body_chunk(
    transport: &impl WriteOwned,
    chunk: impl IoChunkable,
    mode: BodyWriteMode,
) -> eyre::Result<()> {
    match mode {
        BodyWriteMode::Chunked => {
            let mut list = IoChunkList::default();
            list.push(format!("{:x}\r\n", chunk.len()).into_bytes());
            list.push(chunk);
            list.push("\r\n");

            let list = write_all_list(transport, list).await?;
            drop(list);
        }
        BodyWriteMode::ContentLength => {
            let mut list = IoChunkList::default();
            list.push(chunk);
            let list = write_all_list(transport, list).await?;
            drop(list);
        }
    }
    Ok(())
}

async fn write_h1_body_end(transport: &impl WriteOwned, mode: BodyWriteMode) -> eyre::Result<()> {
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
