use eyre::Context;
use nom::IResult;
use pretty_hex::PrettyHex;
use tracing::{debug, trace};

use hring_buffet::{ReadOwned, Roll, RollMut};

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
        trace!(
            "reading+parsing (buf.len={}, buf.cap={})",
            buf.len(),
            buf.cap()
        );
        let filled = buf.filled();

        match parser(filled) {
            Ok((rest, output)) => {
                buf.keep(rest);
                return Ok(Some((buf, output)));
            }
            Err(err) => {
                if err.is_incomplete() {
                    {
                        trace!(
                            "incomplete request, need more data. start of buffer: {:?}",
                            &buf[..std::cmp::min(buf.len(), 128)].hex_dump()
                        );
                    }

                    let res;
                    let read_limit = max_len - buf.len();
                    if buf.len() >= max_len {
                        return Err(SemanticError::BufferLimitReachedWhileParsing.into());
                    }

                    if buf.cap() == 0 {
                        trace!("buf had zero cap, reserving");
                        buf.reserve()?;
                    }
                    trace!(
                        "calling read_into, buf.cap={}, buf.len={} read_limit={read_limit}",
                        buf.cap(),
                        buf.len()
                    );
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
