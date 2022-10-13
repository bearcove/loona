use nom::{
    bytes::streaming::{tag, take, take_until, take_while1},
    IResult,
};

use super::aggregate::AggregateSlice;

const CRLF: &[u8] = b"\r\n";

pub struct Request {
    pub method: AggregateSlice,

    pub path: AggregateSlice,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,
}

pub struct Header {
    pub name: AggregateSlice,
    pub value: AggregateSlice,
}

// Looks like `GET /path HTTP/1.1\r\n`, then headers
pub fn request(i: AggregateSlice) -> IResult<AggregateSlice, Request> {
    let (i, method) = take_while1_and_consume(i, |c| c != b' ')?;
    let (i, path) = take_while1_and_consume(i, |c| c != b' ')?;
    let (i, _) = tag(&b"HTTP/1."[..])(i)?;
    let (i, version) = take(1usize)(i)?;
    let version = match version.iter().next().unwrap() {
        b'0' => 0,
        b'1' => 1,
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                // FIXME: this is not good error reporting
                nom::error::ErrorKind::Digit,
            )));
        }
    };
    let (i, _) = tag(CRLF)(i)?;

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
    let (i, out) = take_until(needle)(i)?;
    let (i, _separator) = tag(needle)(i)?;

    Ok((i, out))
}
