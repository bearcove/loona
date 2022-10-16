#[derive(thiserror::Error, Debug)]
pub(crate) enum SemanticError {
    #[error("the headers are too long")]
    HeadersTooLong,
}

impl SemanticError {
    pub(crate) fn as_http_response(&self) -> &'static [u8] {
        match self {
            Self::HeadersTooLong => b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n",
        }
    }
}
