use std::rc::Rc;

use eyre::Context;
use tracing::debug;

use crate::{
    h1::body::{H1Body, H1BodyKind},
    util::{read_and_parse, SemanticError},
    HeadersExt, Responder, ServerDriver,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    ClientRequestedConnectionClose,
    ServerRequestedConnectionClose,
    ClientClosedConnectionBetweenRequests,
    // TODO: return buffer there so we can see what they did write?
    ClientDidntSpeakHttp11,
}

pub async fn serve(
    (mut transport_r, mut transport_w): (impl ReadOwned, impl WriteOwned),
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: impl ServerDriver,
) -> eyre::Result<ServeOutcome> {
    loop {
        let req;
        (client_buf, req) = match read_and_parse(
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
            Err(e) => {
                if let Some(se) = e.downcast_ref::<SemanticError>() {
                    transport_w
                        .write_all_owned(se.as_http_response())
                        .await
                        .wrap_err("writing error response downstream")?;
                }

                debug!(?e, "error reading request header from downstream");
                return Ok(ServeOutcome::ClientDidntSpeakHttp11);
            }
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
            .wrap_err("handling request")?;

        // TODO: if we sent `connection: close` we should close now
        transport_w = resp.into_inner().transport_w;

        (client_buf, transport_r) = req_body
            .into_inner()
            .ok_or_else(|| eyre::eyre!("request body not drained, have to close connection"))?;

        if connection_close {
            debug!("client requested connection close");
            return Ok(ServeOutcome::ClientRequestedConnectionClose);
        }
    }
}
