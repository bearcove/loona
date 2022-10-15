//! Types for HTTP headers

use smallvec::SmallVec;

use crate::bufpool::AggSlice;

#[derive(Default)]
pub struct Headers {
    // TODO: this could/should be a multimap. http's multimap is neat but doesn't
    // support `AggSlice`. The `HeaderName` type should probably have three
    // variants:
    //   WellKnown (TransferEncoding, Connection, etc.)
    //   &'static [u8] (custom)
    //   AggSlice (proxied)
    headers: SmallVec<[Header; 32]>,
}

impl Headers {
    /// Append a new header. Does not replace anything.
    pub fn push(&mut self, header: Header) {
        self.headers.push(header);
    }

    /// Returns true if we have this key/value combinatoin
    pub fn has_kv(&self, k: impl AsRef<[u8]>, v: impl AsRef<[u8]>) -> bool {
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
    pub fn has_connection_close(&self) -> bool {
        self.has_kv("connection", "close")
    }

    /// Returns true if we have a `transfer-encoding: chunked` header
    pub fn is_chunked_transfer_encoding(&self) -> bool {
        self.has_kv("transfer-encoding", "chunked")
    }

    /// Returns the content-length header
    pub fn content_len(&self) -> Option<u64> {
        for h in self {
            if h.name.eq_ignore_ascii_case("content-length") {
                // FIXME: this is really wasteful. maybe there's something to be
                // done where: 1) it's probably contiguous aynway, so just use that,
                // or: 2) if it's not, just copy it to a stack-allocated slice.
                //
                // this could be a method of AggSlice that lends a `&[u8]`
                // to a closure and errors out if it's too big. it could take
                // const generics.
                let value = h.value.to_vec();
                if let Ok(s) = std::str::from_utf8(&value[..]) {
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
    type IntoIter = std::slice::Iter<'a, Header>;

    fn into_iter(self) -> Self::IntoIter {
        self.headers.iter()
    }
}

pub struct Header {
    pub name: AggSlice,
    pub value: AggSlice,
}
