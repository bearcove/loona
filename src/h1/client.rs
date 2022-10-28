use std::rc::Rc;

use eyre::Context;
use tracing::debug;

use crate::{
    buffet::IoChunkList,
    io::ReadWriteOwned,
    types::Request,
    util::{read_and_parse, write_all_list},
    Body, Response, RollMut,
};

use super::{
    body::{write_h1_body, BodyWriteMode, H1Body, H1BodyKind},
    encode::encode_request,
};

pub struct ClientConf {}

pub trait ClientDriver {
    type Return;

    async fn on_informational_response(&self, res: Response) -> eyre::Result<()>;
    async fn on_final_response(
        self,
        res: Response,
        body: &mut impl Body,
    ) -> eyre::Result<Self::Return>;
}

/// Perform an HTTP/1.1 request against an HTTP/1.1 server
///
/// The transport will be returned unless the server requested connection close.
pub async fn request<T, D>(
    transport: Rc<T>,
    req: Request,
    body: &mut impl Body,
    driver: D,
) -> eyre::Result<(Option<Rc<T>>, D::Return)>
where
    T: ReadWriteOwned,
    D: ClientDriver,
{
    // TODO: set content-length if body isn't empty / missing
    let mode = if body.content_len().is_none() {
        BodyWriteMode::Chunked
    } else {
        BodyWriteMode::ContentLength
    };

    let mut list = IoChunkList::default();
    encode_request(req, &mut list)?;
    _ = write_all_list(transport.as_ref(), list)
        .await
        .wrap_err("writing request headers")?;

    // TODO: handle `expect: 100-continue` (don't start sending body until we get a 100 response)

    let send_body_fut = {
        let transport = transport.clone();
        async move {
            match write_h1_body(transport, body, mode).await {
                Err(err) => {
                    // TODO: find way to report this error to the driver without
                    // spawning, without ref-counting the driver, etc.
                    panic!("error writing request body: {err:?}");
                }
                Ok(body) => {
                    debug!("done writing request body");
                    Ok::<_, eyre::Report>(body)
                }
            }
        }
    };

    let recv_res_fut = {
        let transport = transport.clone();
        async move {
            let buf = RollMut::alloc()?;
            let (buf, res) = read_and_parse(
                super::parse::response,
                transport.as_ref(),
                buf,
                // TODO: make this configurable
                64 * 1024,
            )
            .await
            .map_err(|e| eyre::eyre!("error reading response headers from server: {e:?}"))?
            .ok_or_else(|| eyre::eyre!("server went away before sending response headers"))?;
            res.debug_print();

            // TODO: handle informational responses

            let chunked = res.headers.is_chunked_transfer_encoding();
            // TODO: handle 204/304 separately
            let content_len = res.headers.content_length().unwrap_or_default();

            let mut res_body = H1Body::new(
                transport,
                buf,
                if chunked {
                    // TODO: even with chunked transfer-encoding, we can announce
                    // a content length - we should probably detect errors there?
                    H1BodyKind::Chunked
                } else {
                    H1BodyKind::ContentLength(content_len)
                },
            );

            let conn_close = res.headers.is_connection_close();

            let ret = driver.on_final_response(res, &mut res_body).await?;

            // TODO: check that res_body is fully drained

            Ok((ret, conn_close))
        }
    };

    // TODO: cancel sending the body if we get a response early?
    let (_, (ret, conn_close)) = tokio::try_join!(send_body_fut, recv_res_fut)?;

    let transport = if conn_close { None } else { Some(transport) };
    Ok((transport, ret))
}
