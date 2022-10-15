use eyre::Context;
use nom::IResult;
use tokio_uring::net::TcpStream;
use tracing::debug;

use crate::{
    bufpool::{AggBuf, AggSlice, IoChunkList},
    proto::errors::SemanticError,
};

/// Returns `None` on EOF, error if partially parsed message.
pub(crate) async fn read_and_parse<Parser, Output>(
    parser: Parser,
    stream: &TcpStream,
    mut buf: AggBuf,
    max_len: u32,
) -> eyre::Result<Option<(AggBuf, Output)>>
where
    Parser: Fn(AggSlice) -> IResult<AggSlice, Output>,
{
    loop {
        if buf.write().capacity() >= max_len {
            // XXX: not great that the error here is 'headers too long' when
            // this is a generic parse function.
            return Err(SemanticError::HeadersTooLong.into());
        }
        buf.write().grow_if_needed()?;

        let (res, buf_s) = stream.read(buf.write_slice()).await;
        let n = res.wrap_err("reading request headers from downstream")?;
        buf = buf_s.into_inner();

        if n == 0 {
            if !buf.read().is_empty() {
                return Err(eyre::eyre!("unexpected EOF"));
            } else {
                return Ok(None);
            }
        }

        debug!("reading headers ({} bytes so far)", buf.read().len());
        let slice = buf.read().slice(0..buf.read().len());

        let (rest, req) = match parser(slice) {
            Ok(t) => t,
            Err(err) => {
                if err.is_incomplete() {
                    debug!("incomplete request, need more data");
                    continue;
                } else {
                    if let nom::Err::Error(e) = &err {
                        debug!(?err, "parsing error");
                        debug!(input = %e.input.to_string_lossy(), "input was");
                    }
                    return Err(eyre::eyre!("parsing error: {err}"));
                }
            }
        };

        return Ok(Some((buf.split_at(rest), req)));
    }
}

/// Write the filled part of a buffer to the given [TcpStream], returning a
/// buffer re-using the remaining space.
pub(crate) async fn write_all(stream: &TcpStream, buf: AggBuf) -> eyre::Result<AggBuf> {
    let slice = buf.read().read_slice();

    let mut list = IoChunkList::default();
    list.push(slice);
    let len = list.len();
    let list = list.into_vec();

    let (res, _list) = stream.writev(list).await;
    let n = res?;
    debug!("wrote {n}/{len}");
    if n < len {
        unimplemented!();
    }

    Ok(buf.split())
}

/// Write the filled part of a buffer to the given [TcpStream], returning a
/// buffer re-using the remaining space.
pub(crate) async fn write_all_list(
    stream: &TcpStream,
    list: IoChunkList,
) -> eyre::Result<IoChunkList> {
    let len = list.len();
    let num_chunks = list.num_chunks();
    let list = list.into_vec();
    debug!("writing {len} bytes in {num_chunks} chunks");

    let (res, mut list) = stream.writev(list).await;
    let n = res?;
    debug!("wrote {n}/{len}");
    if n < len {
        unimplemented!();
    }

    list.clear();
    Ok(list.into())
}
