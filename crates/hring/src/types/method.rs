use std::fmt;

use hring_buffet::{Piece, PieceStr};

/// An HTTP method, see https://httpwg.org/specs/rfc9110.html#methods
#[derive(Clone)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Connect,
    Options,
    Trace,
    Other(PieceStr),
}

impl fmt::Debug for Method {
    // forward to display
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Other(s) => s,
        };
        f.pad(s)
    }
}

impl Method {
    pub fn into_chunk(self) -> Piece {
        let s = match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Other(roll) => return roll.into_inner(),
        };
        s.into()
    }
}

impl From<PieceStr> for Method {
    fn from(s: PieceStr) -> Self {
        match &s[..] {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "CONNECT" => Method::Connect,
            "OPTIONS" => Method::Options,
            "TRACE" => Method::Trace,
            _ => Method::Other(s),
        }
    }
}
