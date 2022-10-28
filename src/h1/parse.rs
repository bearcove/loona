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
    types::{Header, Headers, Request, Response},
    Method, Roll,
};

const CRLF: &[u8] = b"\r\n";

/// Parses a chunked transfer coding chunk size (hex text followed by CRLF)
pub fn chunk_size(i: Roll) -> IResult<Roll, u64> {
    terminated(u64_text_hex, tag(CRLF))(i)
}

pub fn crlf(i: Roll) -> IResult<Roll, ()> {
    let (i, _) = tag(CRLF)(i)?;
    Ok((i, ()))
}

// Looks like `GET /path HTTP/1.1\r\n`, then headers
pub fn request(i: Roll) -> IResult<Roll, Request> {
    let (i, method) = method_and_spacing(i)?;
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

// TODO: use `token`, cf. https://httpwg.org/specs/rfc9110.html#methods
pub fn method_and_spacing(i: Roll) -> IResult<Roll, Method> {
    let (i, method) = take_until_and_consume(b" ")(i)?;
    Ok((i, method.into()))
}

// Looks like `HTTP/1.1 200 OK\r\n` or `HTTP/1.1 404 Not Found\r\n`, then headers
pub fn response(i: Roll) -> IResult<Roll, Response> {
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
fn u16_text(i: Roll) -> IResult<Roll, u16> {
    // TODO: limit how many digits we read
    let f = take_while1(nom::character::is_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = nom::combinator::map_res(f, |s: Roll| String::from_utf8(s.to_vec()));
    let mut f = nom::combinator::map_res(f, |s| s.parse());
    f(i)
}

/// Parses text as a hex u64
fn u64_text_hex(i: Roll) -> IResult<Roll, u64> {
    // TODO: limit how many digits we read
    let f = take_while1(nom::character::is_hex_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = nom::combinator::map_res(f, |s: Roll| String::from_utf8(s.to_vec()));
    let mut f = nom::combinator::map_res(f, |s| u64::from_str_radix(&s, 16));
    f(i)
}

pub fn http_version(i: Roll) -> IResult<Roll, u8> {
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

pub fn headers_and_crlf(mut i: Roll) -> IResult<Roll, Headers> {
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
fn header(i: Roll) -> IResult<Roll, (Roll, Roll)> {
    let (i, name) = take_until_and_consume(b":")(i)?;
    let (i, value) = preceded(space1, take_until_and_consume(CRLF))(i)?;

    Ok((i, (name, value)))
}

/// Parse at least one SP character
fn space1(i: Roll) -> IResult<Roll, ()> {
    let (i, _) = take_while1(|c| c == b' ')(i)?;
    Ok((i, ()))
}

/// Parse until the given tag, then skip the tag
fn take_until_and_consume(needle: &[u8]) -> impl FnMut(Roll) -> IResult<Roll, Roll> + '_ {
    terminated(take_until(needle), tag(needle))
}
