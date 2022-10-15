#[derive(thiserror::Error, Debug)]
pub(crate) enum SemanticError {
    #[error("the headers are too long")]
    HeadersTooLong,

    #[error("chunked transfer encoding is not supported")]
    NoChunked,
}

impl SemanticError {
    pub(crate) fn as_http_response(&self) -> &'static [u8] {
        match self {
            Self::HeadersTooLong => b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n",
            Self::NoChunked => b"HTTP/1.1 501 Chunked Transfer Encoding Not Implemented\r\n\r\n",
        }
    }
}
