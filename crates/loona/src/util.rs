use nom::IResult;
use pretty_hex::PrettyHex;
use tracing::{debug, trace};

use buffet::{ReadOwned, Roll, RollMut};

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReadAndParseError {
    /// Allocation error
    #[error("Allocation error: {0}")]
    Alloc(#[from] buffet::bufpool::Error),

    /// Read error
    #[error("Read error: {0}")]
    ReadError(#[from] std::io::Error),

    /// Buffer limit reached while parsing
    #[error("Buffer limit reached while parsing (limit: {limit})")]
    BufferLimitReachedWhileParsing { limit: usize },

    /// Parsing error
    // TODO: should we pass any amount of detail here?
    #[error("Parsing error in parser: {parser}")]
    ParsingError { parser: &'static str },
}

/// Returns `None` on EOF, error if partially parsed message.
pub(crate) async fn read_and_parse<Parser, Output>(
    parser_name: &'static str,
    parser: Parser,
    stream: &mut impl ReadOwned,
    mut buf: RollMut,
    max_len: usize,
    // TODO: proper error handling, no eyre::Result
) -> Result<Option<(RollMut, Output)>, ReadAndParseError>
where
    Parser: Fn(Roll) -> IResult<Roll, Output>,
{
    loop {
        trace!("Running parser (len={}, cap={})", buf.len(), buf.cap());
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
                            "need more data. so far, we have:\n{:?}",
                            &buf[..std::cmp::min(buf.len(), 128)].hex_dump()
                        );
                    }

                    let res;
                    let read_limit = max_len - buf.len();
                    if buf.len() >= max_len {
                        return Err(ReadAndParseError::BufferLimitReachedWhileParsing {
                            limit: max_len,
                        });
                    }

                    if buf.cap() == 0 {
                        trace!("buf had zero cap, reserving");
                        buf.reserve()?;
                    }
                    trace!(
                        "Calling read_into (len={}, cap={}, read_limit={read_limit})",
                        buf.len(),
                        buf.cap(),
                    );
                    (res, buf) = buf.read_into(read_limit, stream).await;

                    let n = res.map_err(ReadAndParseError::ReadError)?;
                    if n == 0 {
                        if !buf.is_empty() {
                            return Err(ReadAndParseError::ReadError(
                                std::io::ErrorKind::UnexpectedEof.into(),
                            ));
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
                    return Err(ReadAndParseError::ParsingError {
                        parser: parser_name,
                    });
                }
            }
        };
    }
}
