use std::fmt;

use crate::{IoChunk, Roll};

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
    fn into_chunk(self) -> IoChunk {
        let s = match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Other(chunk) => return chunk.clone().into(),
        };
        s.into()
    }
}
