use eyre::Context;
use nom::IResult;
use pretty_hex::PrettyHex;
use tracing::debug;

use crate::{
    buffet::PieceList,
    io::{ReadOwned, WriteOwned},
    Roll, RollMut,
};

/// Returns `None` on EOF, error if partially parsed message.
pub(crate) async fn read_and_parse<Parser, Output>(
    parser: Parser,
    stream: &impl ReadOwned,
    mut buf: RollMut,
    max_len: usize,
) -> eyre::Result<Option<(RollMut, Output)>>
where
    Parser: Fn(Roll) -> IResult<Roll, Output>,
{
    loop {
        debug!("reading+parsing ({} bytes so far)", buf.len());
        let filled = buf.filled();

        match parser(filled) {
            Ok((rest, output)) => {
                buf.keep(rest);
                return Ok(Some((buf, output)));
            }
            Err(err) => {
                if err.is_incomplete() {
                    {
                        debug!(
                            "incomplete request, need more data. start of buffer: {:?}",
                            &buf[..std::cmp::min(buf.len(), 128)].hex_dump()
                        );
                    }

                    let res;
                    let read_limit = max_len - buf.len();
                    if buf.len() >= max_len {
                        return Err(SemanticError::BufferLimitReachedWhileParsing.into());
                    }

                    (res, buf) = buf.read_into(read_limit, stream).await;

                    let n = res.wrap_err("reading request headers from downstream")?;
                    if n == 0 {
                        if !buf.is_empty() {
                            return Err(eyre::eyre!("unexpected EOF"));
                        } else {
                            return Ok(None);
                        }
                    }

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
    }
}

/// Write the filled part of a buffer to the given [TcpStream], returning a
/// buffer re-using the remaining space.
pub(crate) async fn write_all_list(
    stream: &impl WriteOwned,
    list: PieceList,
) -> eyre::Result<PieceList> {
    let len = list.len();
    let num_chunks = list.num_pieces();
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

#[derive(thiserror::Error, Debug)]
pub(crate) enum SemanticError {
    #[error("buffering limit reached while parsing")]
    BufferLimitReachedWhileParsing,
}

impl SemanticError {
    pub(crate) fn as_http_response(&self) -> &'static [u8] {
        match self {
            Self::BufferLimitReachedWhileParsing => {
                b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n"
            }
        }
    }
}
