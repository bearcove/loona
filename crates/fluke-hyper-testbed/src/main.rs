use http_body_util::{BodyExt, StreamBody};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;

use hyper_util::server::conn::auto;
use std::{convert::Infallible, pin::Pin};
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

pub(crate) struct TestService;

pub fn big_body() -> String {
    "this is a big chunk".repeat(256).repeat(128)
}

type BoxBody = Pin<Box<dyn Body<Data = Bytes, Error = Infallible> + Send + Sync + 'static>>;

impl<B> Service<Request<B>> for TestService
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    <B as Body>::Error: std::fmt::Debug + Send + 'static,
{
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn call(&self, req: Request<B>) -> Self::Future {
        Box::pin(async move {
            let (parts, mut body) = req.into_parts();
            println!("Handling {parts:?}");

            let path = parts.uri.path();
            match path {
                "/echo-body" => {
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1);
                    tokio::spawn(async move {
                        while let Some(frame) = body.frame().await {
                            let _ = tx.send(Ok(frame.unwrap())).await;
                        }
                    });

                    let rx = ReceiverStream::new(rx);
                    let body = StreamBody::new(rx);
                    let body: BoxBody = Box::pin(body);
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                }
                "/stream-big-body" => {
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1);

                    tokio::spawn(async move {
                        let chunk = "this is a big chunk".repeat(256);
                        let chunk = Bytes::from(chunk);
                        for _ in 0..128 {
                            let frame = Frame::data(chunk.clone());
                            let _ = tx.send(Ok(frame)).await;
                        }
                    });

                    let rx = ReceiverStream::new(rx);
                    let body: BoxBody = Box::pin(StreamBody::new(rx));
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                }
                _ => {
                    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
                    let body: BoxBody = Box::pin(http_body_util::Empty::new());

                    if let ["status", code] = parts.as_slice() {
                        let code = code.parse::<u16>().unwrap();
                        let res = Response::builder().status(code).body(body).unwrap();
                        debug!("Replying with {:?} {:?}", res.status(), res.headers());
                        Ok(res)
                    } else {
                        let res = Response::builder().status(404).body(body).unwrap();
                        Ok(res)
                    }
                }
            }
        })
    }
}

#[tokio::main]
async fn main() {
    let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = ln.local_addr().unwrap();
    println!("I listen on {upstream_addr}");

    while let Ok((stream, _)) = ln.accept().await {
        tokio::spawn(async move {
            let mut builder = auto::Builder::new(TokioExecutor::new());
            builder = builder.http2_only();
            builder
                .serve_connection(TokioIo::new(stream), TestService)
                .await
        });
    }
}
