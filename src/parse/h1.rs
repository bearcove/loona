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

use crate::{
    bufpool::AggSlice,
    types::{Header, Headers, Request, Response},
};

const CRLF: &[u8] = b"\r\n";

pub struct Chunk {
    pub len: u64,
    pub data: AggSlice,
}

/// Parses a single transfer-encoding chunk
pub fn chunk(i: AggSlice) -> IResult<AggSlice, Chunk> {
    let (i, len) = terminated(u64_text_hex, tag(CRLF))(i)?;
    // FIXME: read trailers if any. we should not expect a CRLF after a chunk of
    // length zero, but a series of headers.
    let (i, data) = terminated(take(len), tag(CRLF))(i)?;

    Ok((i, Chunk { len, data }))
}

// Looks like `GET /path HTTP/1.1\r\n`, then headers
pub fn request(i: AggSlice) -> IResult<AggSlice, Request> {
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
pub fn response(i: AggSlice) -> IResult<AggSlice, Response> {
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
fn u16_text(i: AggSlice) -> IResult<AggSlice, u16> {
    // TODO: limit how many digits we read
    let f = take_while1(nom::character::is_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = nom::combinator::map_res(f, |s: AggSlice| String::from_utf8(s.to_vec()));
    let mut f = nom::combinator::map_res(f, |s| s.parse());
    f(i)
}

/// Parses text as a hex u64
fn u64_text_hex(i: AggSlice) -> IResult<AggSlice, u64> {
    // TODO: limit how many digits we read
    let f = take_while1(nom::character::is_hex_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = nom::combinator::map_res(f, |s: AggSlice| String::from_utf8(s.to_vec()));
    let mut f = nom::combinator::map_res(f, |s| u64::from_str_radix(&s, 16));
    f(i)
}

pub fn http_version(i: AggSlice) -> IResult<AggSlice, u8> {
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

pub fn headers_and_crlf(mut i: AggSlice) -> IResult<AggSlice, Headers> {
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
fn header(i: AggSlice) -> IResult<AggSlice, (AggSlice, AggSlice)> {
    let (i, name) = take_until_and_consume(b":")(i)?;
    let (i, value) = preceded(space1, take_until_and_consume(CRLF))(i)?;

    Ok((i, (name, value)))
}

/// Parse at least one SP character
fn space1(i: AggSlice) -> IResult<AggSlice, ()> {
    let (i, _) = take_while1(|c| c == b' ')(i)?;
    Ok((i, ()))
}

/// Parse until the given tag, then skip the tag
fn take_until_and_consume(
    needle: &[u8],
) -> impl FnMut(AggSlice) -> IResult<AggSlice, AggSlice> + '_ {
    terminated(take_until(needle), tag(needle))
}
