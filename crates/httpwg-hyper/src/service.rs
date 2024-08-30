//! //! This service provides the following routes:
//!
//! - `/echo-body`: Echoes back the request body.
//! - `/status/{code}`: Returns a response with the specified status code.
//! - `/repeat-4k-blocks/{repeat}`: Streams the specified number of 4KB blocks.
//! - `/stream-file/{name}`: Streams the contents of a file from
//!   `/tmp/stream-file/`.
//! - `/`: Returns a default message.
//! - Any other path: Returns a 404 Not Found response.

use http_body_util::{BodyExt, StreamBody};
use httpwg_harness::SAMPLE_4K_BLOCK;
use tokio::io::AsyncReadExt;

use std::{convert::Infallible, fmt::Debug, pin::Pin};
use tokio::sync::mpsc;

use bytes::Bytes;
use futures::Future;
use hyper::{
    body::{Body, Frame},
    service::Service,
    Request, Response,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;

type BoxBody<E> = Pin<Box<dyn Body<Data = Bytes, Error = E> + Send + Sync + 'static>>;

pub(super) struct TestService;

impl<B, E> Service<Request<B>> for TestService
where
    B: Body<Data = Bytes, Error = E> + Send + Sync + Unpin + 'static,
    E: Debug + Send + Sync + 'static,
{
    type Response = Response<BoxBody<E>>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn call(&self, req: Request<B>) -> Self::Future {
        Box::pin(async move {
            let (parts, mut req_body) = req.into_parts();

            let path = parts.uri.path();
            let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();

            if let ["echo-body"] = parts.as_slice() {
                let body: BoxBody<E> = Box::pin(req_body);
                let res = Response::builder().body(body).unwrap();
                Ok(res)
            } else {
                let body: BoxBody<E> =
                    Box::pin(http_body_util::Empty::new().map_err(|_| unreachable!()));

                if let ["status", code] = parts.as_slice() {
                    // drain body
                    while let Some(_frame) = req_body.frame().await {}

                    let code = code.parse::<u16>().unwrap();
                    let res = Response::builder().status(code).body(body).unwrap();
                    debug!("Replying with {:?} {:?}", res.status(), res.headers());
                    Ok(res)
                } else if let ["repeat-4k-blocks", repeat] = parts.as_slice() {
                    // drain body
                    while let Some(_frame) = req_body.frame().await {}

                    let repeat = repeat.parse::<usize>().unwrap();

                    // TODO: custom impl of the Body trait to avoid channel overhead
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, E>>(1);

                    tokio::spawn(async move {
                        let block = Bytes::copy_from_slice(SAMPLE_4K_BLOCK);
                        for _ in 0..repeat {
                            let frame = Frame::data(block.clone());
                            let _ = tx.send(Ok(frame)).await;
                        }
                    });

                    let rx = ReceiverStream::new(rx);
                    let body: BoxBody<E> = Box::pin(StreamBody::new(rx));
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                } else if let ["stream-file", name] = parts.as_slice() {
                    // drain body
                    while let Some(_frame) = req_body.frame().await {}

                    let name = name.to_string();

                    // TODO: custom impl of the Body trait to avoid channel overhead
                    // stream 64KB blocks of the file
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, E>>(1);
                    tokio::spawn(async move {
                        let mut file = tokio::fs::File::open(format!("/tmp/stream-file/{name}"))
                            .await
                            .unwrap();
                        let mut buf = vec![0u8; 64 * 1024];
                        while let Ok(n) = file.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                            let frame = Frame::data(Bytes::copy_from_slice(&buf[..n]));
                            let _ = tx.send(Ok(frame)).await;
                        }
                    });

                    let rx = ReceiverStream::new(rx);
                    let body: BoxBody<E> = Box::pin(StreamBody::new(rx));
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                } else if parts.as_slice().is_empty() {
                    // drain body
                    while let Some(_frame) = req_body.frame().await {}

                    let body = "it's less dire to lose, than to lose oneself".to_string();
                    let body: BoxBody<E> = Box::pin(body.map_err(|_| unreachable!()));
                    let res = Response::builder().status(200).body(body).unwrap();
                    Ok(res)
                } else {
                    // drain body
                    while let Some(_frame) = req_body.frame().await {}

                    // return a 404
                    let body = "404 Not Found".to_string();
                    let body: BoxBody<E> = Box::pin(body.map_err(|_| unreachable!()));
                    let res = Response::builder().status(404).body(body).unwrap();
                    Ok(res)
                }
            }
        })
    }
}
