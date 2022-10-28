//! Types for HTTP headers

use smallvec::SmallVec;

use crate::{Piece, PieceStr};

const HEADERS_SMALLVEC_CAPACITY: usize = 32;

#[derive(Default)]
pub struct Headers {
    // TODO: this could/should be a multimap. http's multimap is neat but doesn't
    // support `AggSlice`. The `HeaderName` type should probably have three
    // variants:
    //   WellKnown (TransferEncoding, Connection, etc.)
    //   &'static [u8] (custom)
    //   AggSlice (proxied)
    headers: SmallVec<[Header; HEADERS_SMALLVEC_CAPACITY]>,
}

impl Headers {
    /// Append a new header. Does not replace anything.
    pub fn push(&mut self, header: Header) {
        self.headers.push(header);
    }

    /// Returns true if we have this key/value combination
    pub fn has_kv(&self, k: impl AsRef<str>, v: impl AsRef<[u8]>) -> bool {
        let k = k.as_ref();
        let v = v.as_ref();

        for h in self {
            if h.name.eq_ignore_ascii_case(k) && h.value.eq_ignore_ascii_case(v) {
                return true;
            }
        }
        false
    }

    /// Returns true if we have a `connection: close` header
    pub fn is_connection_close(&self) -> bool {
        self.has_kv("connection", "close")
    }

    /// Returns true if we have a `transfer-encoding: chunked` header
    pub fn is_chunked_transfer_encoding(&self) -> bool {
        self.has_kv("transfer-encoding", "chunked")
    }

    /// Returns the content-length header
    pub fn content_length(&self) -> Option<u64> {
        for h in self {
            if h.name.eq_ignore_ascii_case("content-length") {
                if let Ok(s) = std::str::from_utf8(&h.value[..]) {
                    if let Ok(l) = s.parse() {
                        return Some(l);
                    }
                }
            }
        }
        None
    }
}

impl<'a> IntoIterator for &'a Headers {
    type Item = &'a Header;
    type IntoIter = impl Iterator<Item = Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.headers.iter()
    }
}

impl IntoIterator for Headers {
    type Item = Header;
    type IntoIter = impl Iterator<Item = Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.headers.into_iter()
    }
}

pub struct Header {
    pub name: PieceStr,
    pub value: Piece,
}
