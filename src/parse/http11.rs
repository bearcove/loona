use nom::{
    bytes::streaming::{tag, take_until, take_while1},
    IResult,
};
use tracing::debug;

use super::aggregate::AggregateSlice;

const CRLF: &[u8] = b"\r\n";

pub struct Request {
    pub method: AggregateSlice,
    pub path: AggregateSlice,
    pub version: AggregateSlice,
}

// Looks like `GET /path HTTP/1.1\r\n`, then headers
pub fn request(i: AggregateSlice) -> IResult<AggregateSlice, Request> {
    debug!("parsing method");
    let (i, method) = take_while1_and_consume(i, |c| c != b' ')?;
    debug!("parsing path");
    let (i, path) = take_while1_and_consume(i, |c| c != b' ')?;
    debug!("parsing version");
    let (i, version) = take_until_and_consume(i, CRLF)?;

    let request = Request {
        method,
        path,
        version,
    };

    Ok((i, request))
}

fn take_while1_and_consume(
    i: AggregateSlice,
    predicate: impl Fn(u8) -> bool,
) -> IResult<AggregateSlice, AggregateSlice> {
    // isn't actually redundant. passing it moves it
    #[allow(clippy::redundant_closure)]
    let (i, out) = take_while1(|c| predicate(c))(i)?;
    let (i, _separator) = take_while1(|c| !predicate(c))(i)?;
    Ok((i, out))
}

fn take_until_and_consume(
    i: AggregateSlice,
    needle: &[u8],
) -> IResult<AggregateSlice, AggregateSlice> {
    // isn't actually redundant. passing it moves it
    #[allow(clippy::redundant_closure)]
    let (i, out) = take_until(needle)(i)?;
    let (i, _separator) = tag(needle)(i)?;
    Ok((i, out))
}
