//! Types for HTTP headers

use http::{header, HeaderMap};

use crate::Piece;

pub type Headers = HeaderMap<Piece>;

pub trait HeadersExt {
    /// Returns the content-length header
    fn content_length(&self) -> Option<u64>;

    /// Returns true if we have a `connection: close` header
    fn is_connection_close(&self) -> bool;

    /// Returns true if we have a `transfer-encoding: chunked` header
    fn is_chunked_transfer_encoding(&self) -> bool;

    /// Returns true if the client expects a `100-continue` response
    fn expects_100_continue(&self) -> bool;
}

impl HeadersExt for HeaderMap<Piece> {
    /// Returns the content-length header
    fn content_length(&self) -> Option<u64> {
        self.get(header::CONTENT_LENGTH)
            .and_then(|s| from_digits(s))
    }

    fn is_connection_close(&self) -> bool {
        self.get(header::CONNECTION)
            .map_or(false, |value| value.eq_ignore_ascii_case(b"close"))
    }

    fn is_chunked_transfer_encoding(&self) -> bool {
        self.get(header::TRANSFER_ENCODING)
            .map_or(false, |value| value.eq_ignore_ascii_case(b"chunked"))
    }

    fn expects_100_continue(&self) -> bool {
        self.get(header::EXPECT)
            .map_or(false, |value| value.eq_ignore_ascii_case(b"100-continue"))
    }
}

fn from_digits(bytes: &[u8]) -> Option<u64> {
    // cannot use FromStr for u64, since it allows a signed prefix
    let mut result = 0u64;
    const RADIX: u64 = 10;

    if bytes.is_empty() {
        return None;
    }

    for &b in bytes {
        // can't use char::to_digit, since we haven't verified these bytes
        // are utf-8.
        match b {
            b'0'..=b'9' => {
                result = result.checked_mul(RADIX)?;
                result = result.checked_add((b - b'0') as u64)?;
            }
            _ => {
                // not a DIGIT, get outta here!
                return None;
            }
        }
    }

    Some(result)
}
