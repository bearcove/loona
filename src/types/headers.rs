//! Types for HTTP headers

use smallvec::SmallVec;

use crate::bufpool::aggregate::AggregateSlice;

#[derive(Default)]
pub struct Headers {
    // TODO: this could/should be a multimap. http's multimap is neat but doesn't
    // support `AggregateSlice`. The `HeaderName` type should probably have three
    // variants:
    //   WellKnown (TransferEncoding, Connection, etc.)
    //   &'static [u8] (custom)
    //   AggregateSlice (proxied)
    headers: SmallVec<[Header; 32]>,
}

impl Headers {
    pub fn push(&mut self, header: Header) {
        self.headers.push(header);
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
    pub name: AggregateSlice,
    pub value: AggregateSlice,
}
