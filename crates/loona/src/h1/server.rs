use std::rc::Rc;

use tracing::debug;

use crate::{
    error::ServeError,
    h1::body::{H1Body, H1BodyKind},
    util::{read_and_parse, ReadAndParseError},
    HeadersExt, Responder, ServeOutcome, ServerDriver,
};
use buffet::{ReadOwned, RollMut, WriteOwned};

use super::encode::H1Encoder;

pub struct ServerConf {
    /// Max length of the request line + HTTP headers
    pub max_http_header_len: usize,

    /// Max length of a single header record, e.g. `user-agent: foobar`
    pub max_header_record_len: usize,

    /// Max number of header records
    pub max_header_records: usize,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self {
            max_http_header_len: 64 * 1024,
            max_header_record_len: 4 * 1024,
            max_header_records: 128,
        }
    }
}

pub async fn serve<Driver>(
    (mut transport_r, mut transport_w): (impl ReadOwned, impl WriteOwned),
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: Driver,
) -> Result<ServeOutcome, ServeError<Driver::Error>>
where
    Driver: ServerDriver,
{
    loop {
        let req;
        (client_buf, req) = match read_and_parse(
            "Http1Request",
            super::parse::request,
            &mut transport_r,
            client_buf,
            conf.max_http_header_len,
        )
        .await
        {
            Ok(t) => match t {
                Some(t) => t,
                None => {
                    debug!("client went away before sending request headers");
                    return Ok(ServeOutcome::ClientClosedConnectionBetweenRequests);
                }
            },
            Err(e) => match e {
                ReadAndParseError::BufferLimitReachedWhileParsing { limit } => {
                    debug!("request headers larger than {limit} bytes, replying with 431 and hanging up");
                    let reply = b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n";
                    transport_w
                        .write_all_owned(reply)
                        .await
                        .map_err(ServeError::DownstreamWrite)?;

                    return Ok(ServeOutcome::RequestHeadersTooLargeOnHttp1Conn);
                }
                _ => {
                    debug!(?e, "error reading request header from downstream");
                    return Ok(ServeOutcome::ClientDidntSpeakHttp11);
                }
            },
        };
        debug!("got request {req:?}");

        let chunked = req.headers.is_chunked_transfer_encoding();
        let connection_close = req.headers.is_connection_close();
        let content_len = req.headers.content_length().unwrap_or_default();

        let mut req_body = H1Body::new(
            transport_r,
            client_buf,
            if chunked {
                H1BodyKind::Chunked
            } else {
                H1BodyKind::ContentLength(content_len)
            },
        );

        let responder = Responder::new(H1Encoder::new(transport_w));

        let resp = driver
            .handle(req, &mut req_body, responder)
            .await
            .map_err(ServeError::Driver)?;

        // TODO: if we sent `connection: close` we should close now
        transport_w = resp.into_inner().transport_w;

        (client_buf, transport_r) = req_body
            .into_inner()
            .ok_or(ServeError::ResponseHandlerBodyNotDrained)?;

        if connection_close {
            debug!("client requested connection close");
            return Ok(ServeOutcome::ClientRequestedConnectionClose);
        }
    }
}
