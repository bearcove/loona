use http_body_util::{BodyExt, StreamBody};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;

use hyper_util::server::conn::auto;
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

pub(crate) struct TestService;

pub fn big_body() -> String {
    "this is a big chunk".repeat(256).repeat(128)
}

type BoxBody<E> = Pin<Box<dyn Body<Data = Bytes, Error = E> + Send + Sync + 'static>>;

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
            let (parts, body) = req.into_parts();

            let path = parts.uri.path();
            match path {
                "/echo-body" => {
                    let body: BoxBody<E> = Box::pin(body);
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                }
                "/stream-big-body" => {
                    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, E>>(1);

                    tokio::spawn(async move {
                        let chunk = "this is a big chunk".repeat(256);
                        let chunk = Bytes::from(chunk);
                        for _ in 0..128 {
                            let frame = Frame::data(chunk.clone());
                            let _ = tx.send(Ok(frame)).await;
                        }
                    });

                    let rx = ReceiverStream::new(rx);
                    let body: BoxBody<E> = Box::pin(StreamBody::new(rx));
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                }
                _ => {
                    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
                    let body: BoxBody<E> =
                        Box::pin(http_body_util::Empty::new().map_err(|_| unreachable!()));

                    if let ["status", code] = parts.as_slice() {
                        let code = code.parse::<u16>().unwrap();
                        let res = Response::builder().status(code).body(body).unwrap();
                        debug!("Replying with {:?} {:?}", res.status(), res.headers());
                        Ok(res)
                    } else {
                        let body = "it's less dire to lose, than to lose oneself".to_string();
                        let body: BoxBody<E> = Box::pin(body.map_err(|_| unreachable!()));
                        let res = Response::builder().status(200).body(body).unwrap();
                        Ok(res)
                    }
                }
            }
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port = std::env::var("PORT").unwrap_or("0".to_string());
    let ln = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let upstream_addr = ln.local_addr().unwrap();
    println!("I listen on {upstream_addr}");

    #[derive(Debug, Clone, Copy)]
    enum Proto {
        H1,
        H2,
    }

    let proto = match std::env::var("TEST_PROTO")
        .unwrap_or("h1".to_string())
        .as_str()
    {
        "h1" => Proto::H1,
        "h2" => Proto::H2,
        _ => panic!("TEST_PROTO must be either 'h1' or 'h2'"),
    };
    println!("Using {proto:?} protocol (export TEST_PROTO=h1 or TEST_PROTO=h2 to override)");

    while let Ok((stream, _)) = ln.accept().await {
        tokio::spawn(async move {
            let mut builder = auto::Builder::new(TokioExecutor::new());

            match proto {
                Proto::H1 => {
                    builder = builder.http1_only();
                }
                Proto::H2 => {
                    builder = builder.http2_only();
                }
            }
            builder
                .serve_connection(TokioIo::new(stream), TestService)
                .await
        });
    }
}
