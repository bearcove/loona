use nom::{
    bytes::streaming::{tag, take, take_until, take_while1},
    combinator::opt,
    sequence::{preceded, terminated},
    IResult,
};
use smallvec::SmallVec;

use super::aggregate::AggregateSlice;

const CRLF: &[u8] = b"\r\n";

pub struct Request {
    pub method: AggregateSlice,

    pub path: AggregateSlice,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    pub headers: SmallVec<[Header; 32]>,
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

    let mut request = Request {
        method,
        path,
        version,
        headers: Default::default(),
    };

    let mut i = i;
    loop {
        if let (i, Some(_)) = opt(tag(CRLF))(i.clone())? {
            // end of headers
            return Ok((i, request));
        }

        let (i2, (name, value)) = header(i)?;
        request.headers.push(Header { name, value });
        i = i2;
    }
}

/// Parses a single header line
fn header(i: AggregateSlice) -> IResult<AggregateSlice, (AggregateSlice, AggregateSlice)> {
    let (i, name) = terminated(take_until(&b":"[..]), tag(&b":"[..]))(i)?;
    let (i, value) = preceded(ws, terminated(take_until(CRLF), tag(CRLF)))(i)?;

    Ok((i, (name, value)))
}

/// Parses whitespace (not including newlines)
fn ws(i: AggregateSlice) -> IResult<AggregateSlice, ()> {
    let (i, _) = take_while1(|c| c == b' ')(i)?;
    Ok((i, ()))
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

#[allow(dead_code)]
fn take_until_and_consume(
    i: AggregateSlice,
    needle: &[u8],
) -> IResult<AggregateSlice, AggregateSlice> {
    let (i, out) = take_until(needle)(i)?;
    let (i, _separator) = tag(needle)(i)?;

    Ok((i, out))
}
