use nom::{
    bytes::streaming::{tag, take_until, take_while1},
    character::is_digit,
    combinator::{map_res, opt},
    sequence::{preceded, terminated},
    IResult,
};

#[derive(Debug)]
pub struct Response<'a> {
    pub status: u16,
    pub status_text: &'a str,

    // header names/values could be non-UTF-8, but let's not care for this sample.
    // we are however careful not to use a HashMap, since headers can repeat.
    pub headers: Vec<(&'a str, &'a str)>,
}

const CRLF: &str = "\r\n";

// Looks like `HTTP/1.1 200 OK\r\n` or `HTTP/1.1 404 Not Found\r\n`
pub fn response(i: &[u8]) -> IResult<&[u8], Response<'_>> {
    let (i, _) = tag("HTTP/1.1 ")(i)?;

    let (i, status) = terminated(u16_text, ws)(i)?;
    let (i, status_text) =
        map_res(terminated(take_until(CRLF), tag(CRLF)), std::str::from_utf8)(i)?;

    let mut res = Response {
        status,
        status_text,
        headers: Default::default(),
    };

    let mut i = i;
    loop {
        if let (i, Some(_)) = opt(tag(CRLF))(i)? {
            // end of headers
            return Ok((i, res));
        }

        let (i2, (name, value)) = header(i)?;
        res.headers.push((name, value));
        i = i2;
    }
}

/// Parses a single header line
fn header(i: &[u8]) -> IResult<&[u8], (&str, &str)> {
    let (i, name) = map_res(terminated(take_until(":"), tag(":")), std::str::from_utf8)(i)?;
    let (i, value) = map_res(
        preceded(ws, terminated(take_until(CRLF), tag(CRLF))),
        std::str::from_utf8,
    )(i)?;

    Ok((i, (name, value)))
}

/// Parses whitespace (not including newlines)
fn ws(i: &[u8]) -> IResult<&[u8], ()> {
    let (i, _) = take_while1(|c| c == b' ')(i)?;
    Ok((i, ()))
}

/// Parses text as a u16
fn u16_text(i: &[u8]) -> IResult<&[u8], u16> {
    let f = take_while1(is_digit);
    let f = map_res(f, std::str::from_utf8);
    let mut f = map_res(f, |s| s.parse());
    f(i)
}
