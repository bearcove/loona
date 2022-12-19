//! HTTP/1.1 parser
//!
//! HTTP/1.1 https://httpwg.org/specs/rfc9112.html
//! HTTP semantics https://httpwg.org/specs/rfc9110.html

use http::{header::HeaderName, StatusCode, Version};
use nom::{
    bytes::streaming::{tag, take, take_until, take_while1},
    combinator::{map_res, opt},
    sequence::{preceded, terminated},
    IResult,
};

use crate::{
    types::{Headers, Request, Response},
    Method, PieceStr, Roll, RollStr,
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
    let (i, method) = terminated(method, space1)(i)?;
    let (i, path) = terminated(path, space1)(i)?;
    let (i, version) = terminated(http_version, tag(CRLF))(i)?;
    let (i, headers) = headers_and_crlf(i)?;

    let request = Request {
        method,
        path: path.into(),
        version,
        headers,
    };
    Ok((i, request))
}

pub fn method(i: Roll) -> IResult<Roll, Method> {
    let (i, method) = token(i)?;
    let method: PieceStr = method.into();
    Ok((i, method.into()))
}

/// A short textual identifier that does not include whitspace or delimiters,
/// cf. https://httpwg.org/specs/rfc9110.html#rule.token.separators
pub fn token(i: Roll) -> IResult<Roll, RollStr> {
    let (i, token) = take_while1(is_tchar)(i)?;
    let token = unsafe { token.to_string_unchecked() };
    Ok((i, token))
}

/// cf. https://httpwg.org/specs/rfc9110.html#rule.token.separators
fn is_tchar(c: u8) -> bool {
    c.is_ascii_graphic() && !is_delimiter(c)
}

/// cf. https://httpwg.org/specs/rfc9110.html#rule.token.separators
fn is_delimiter(c: u8) -> bool {
    memchr::memchr(c, br#"(),/:;<=>?@[\]{}""#).is_some()
}

fn path(i: Roll) -> IResult<Roll, RollStr> {
    let (i, path) = take_while1(is_uri_char)(i)?;
    let path = unsafe { path.to_string_unchecked() };
    Ok((i, path))
}

/// Returns true if `c` is a character that can be found in an URI
/// cf. https://stackoverflow.com/a/7109208
fn is_uri_char(c: u8) -> bool {
    memchr::memchr(
        c,
        br#"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~:/?#[]@!$&'()*+,;%="#,
    )
    .is_some()
}

// Looks like `HTTP/1.1 200 OK\r\n` or `HTTP/1.1 404 Not Found\r\n`, then headers
pub fn response(i: Roll) -> IResult<Roll, Response> {
    let (i, version) = terminated(http_version, space1)(i)?;
    let (i, code) = terminated(status_code, space1)(i)?;
    let (i, _reason) = terminated(take_until(CRLF), tag(CRLF))(i)?;
    let (i, headers) = headers_and_crlf(i)?;

    let response = Response {
        version,
        status: code,
        headers,
    };
    Ok((i, response))
}

/// Parses an HTTP/1.1 status code
fn status_code(i: Roll) -> IResult<Roll, StatusCode> {
    let (i, code) = map_res(take(3_usize), |r: Roll| StatusCode::from_bytes(&r[..]))(i)?;
    Ok((i, code))
}

/// Parses text as a hex u64
fn u64_text_hex(i: Roll) -> IResult<Roll, u64> {
    // TODO: limit how many digits we read
    let f = take_while1(nom::character::is_hex_digit);
    // FIXME: this is inefficient (calling `to_vec` just to parse)
    let f = map_res(f, |s: Roll| String::from_utf8(s.to_vec()));
    let mut f = map_res(f, |s| u64::from_str_radix(&s, 16));
    f(i)
}

pub fn http_version(i: Roll) -> IResult<Roll, Version> {
    let (i, _) = tag(&b"HTTP/1."[..])(i)?;
    let (i, version) = take(1usize)(i)?;
    let version = match version.iter().next().unwrap() {
        b'0' => Version::HTTP_10,
        b'1' => Version::HTTP_11,
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

        let (i_next, (name, value)) = header(i)?;
        headers.append(name, value.into());
        i = i_next;
    }
}

/// Parse a single header line
fn header(i: Roll) -> IResult<Roll, (HeaderName, Roll)> {
    let (i, name) = map_res(take_until_and_consume(b":"), |s: Roll| {
        HeaderName::from_bytes(&s[..])
    })(i)?;
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

#[cfg(test)]
mod tests {
    use crate::h1::parse::is_delimiter;

    #[test]
    fn test_h1_parse_various_lowlevel_functions() {
        assert!(is_delimiter(b'('));
        assert!(is_delimiter(b'"'));
        assert!(is_delimiter(b'\\'));
        assert!(!is_delimiter(b'B'));
    }
}
