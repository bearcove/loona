use std::fmt;

use crate::{IoChunk, Roll};

/// An HTTP method, see https://httpwg.org/specs/rfc9110.html#methods
#[derive(Clone, Debug)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Connect,
    Options,
    Trace,
    Other(Roll),
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
            // TODO: use RollStr
            Method::Other(roll) => return write!(f, "{}", roll.to_string_lossy()),
        };
        f.pad(s)
    }
}

impl Method {
    pub fn into_chunk(self) -> IoChunk {
        let s = match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Other(chunk) => return chunk.into(),
        };
        s.into()
    }
}

impl From<Roll> for Method {
    fn from(roll: Roll) -> Self {
        match &roll[..] {
            b"GET" => Method::Get,
            b"HEAD" => Method::Head,
            b"POST" => Method::Post,
            b"PUT" => Method::Put,
            b"DELETE" => Method::Delete,
            b"CONNECT" => Method::Connect,
            b"OPTIONS" => Method::Options,
            b"TRACE" => Method::Trace,
            _ => Method::Other(roll),
        }
    }
}
