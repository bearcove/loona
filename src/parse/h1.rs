//! HTTP/1.1 parser
//!
//! As of June 2022, the authoritative document for HTTP/1.1
//! is https://www.rfc-editor.org/rfc/rfc9110

use nom::{
    bytes::streaming::{tag, take, take_until, take_while1},
    combinator::opt,
    sequence::{preceded, terminated},
    IResult,
};
use smallvec::SmallVec;

use super::aggregate::AggregateSlice;

const CRLF: &[u8] = b"\r\n";

// TODO: this could/should be a multimap. http's multimap is neat but doesn't
// support `AggregateSlice`. The `HeaderName` type should probably have three
// variants:
//   WellKnown (TransferEncoding, Connection, etc.)
//   &'static [u8] (custom)
//   AggregateSlice (proxied)
pub type Headers = SmallVec<[Header; 32]>;

/// An HTTP request
pub struct Request {
    pub method: AggregateSlice,

    pub path: AggregateSlice,

    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    pub headers: Headers,
}

/// An HTTP response
pub struct Response {
    /// The 'b' in 'HTTP/1.b'
    pub version: u8,

    /// Status code (1xx-5xx)
    pub code: u16,

    pub reason: AggregateSlice,

    pub headers: Headers,
}

pub struct Header {
    pub name: AggregateSlice,
    pub value: AggregateSlice,
}

// Looks like `GET /path HTTP/1.1\r\n`, then headers
pub fn request(i: AggregateSlice) -> IResult<AggregateSlice, Request> {
    let (i, method) = take_until_and_consume(b" ")(i)?;
    let (i, path) = take_until_and_consume(b" ")(i)?;
    let (i, version) = terminated(http_version, tag(CRLF))(i)?;
    let (i, headers) = headers_and_crlf(i)?;

    let request = Request {
        method,
        path,
        version,
        headers,
    };
    Ok((i, request))
}

// Looks like `HTTP/1.1 200 OK\r\n` or `HTTP/1.1 404 Not Found\r\n`, then headers
pub fn response(i: AggregateSlice) -> IResult<AggregateSlice, Response> {
    let (i, version) = terminated(http_version, space1)(i)?;
    let (i, code) = terminated(u16_text, space1)(i)?;
    let (i, reason) = terminated(take_until(CRLF), tag(CRLF))(i)?;
    let (i, headers) = headers_and_crlf(i)?;

    let response = Response {
        version,
        code,
        reason,
        headers,
    };
    Ok((i, response))
}

/// Parses text as a u16
fn u16_text(i: AggregateSlice) -> IResult<AggregateSlice, u16> {
    let f = take_while1(nom::character::is_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = nom::combinator::map_res(f, |s: AggregateSlice| String::from_utf8(s.to_vec()));
    let mut f = nom::combinator::map_res(f, |s| s.parse());
    f(i)
}

pub fn http_version(i: AggregateSlice) -> IResult<AggregateSlice, u8> {
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

    Ok((i, version))
}

pub fn headers_and_crlf(mut i: AggregateSlice) -> IResult<AggregateSlice, Headers> {
    let mut headers = Headers::default();
    loop {
        if let (i, Some(_)) = opt(tag(CRLF))(i.clone())? {
            // end of headers
            return Ok((i, headers));
        }

        let (i2, (name, value)) = header(i)?;
        headers.push(Header { name, value });
        i = i2;
    }
}

/// Parse a single header line
fn header(i: AggregateSlice) -> IResult<AggregateSlice, (AggregateSlice, AggregateSlice)> {
    let (i, name) = take_until_and_consume(b":")(i)?;
    let (i, value) = preceded(space1, take_until_and_consume(CRLF))(i)?;

    Ok((i, (name, value)))
}

/// Parse at least one SP character
fn space1(i: AggregateSlice) -> IResult<AggregateSlice, ()> {
    let (i, _) = take_while1(|c| c == b' ')(i)?;
    Ok((i, ()))
}

/// Parse until the given tag, then skip the tag
fn take_until_and_consume(
    needle: &[u8],
) -> impl FnMut(AggregateSlice) -> IResult<AggregateSlice, AggregateSlice> + '_ {
    terminated(take_until(needle), tag(needle))
}
