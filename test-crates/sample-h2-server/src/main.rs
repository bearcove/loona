use std::{convert::Infallible, future::Future};

use bytes::Bytes;
use hyper::{
    service::{make_service_fn, Service},
    Body, Request, Response,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::debug;

struct TestService;

impl Service<Request<Body>> for TestService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        async move {
            let (parts, body) = req.into_parts();
            println!("Handling {parts:?}");

            let path = parts.uri.path();
            match path {
                "/echo-body" => {
                    let res = Response::builder().body(body).unwrap();
                    Ok(res)
                }
                "/stream-big-body" => {
                    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(1);
                    let rx = ReceiverStream::new(rx).map(Ok::<_, Infallible>);

                    tokio::spawn(async move {
                        let chunk = "this is a big chunk".repeat(256);
                        let chunk = Bytes::from(chunk);
                        for _ in 0..128 {
                            let _ = tx.send(chunk.clone()).await;
                        }
                    });

                    let res = Response::builder().body(Body::wrap_stream(rx)).unwrap();
                    Ok(res)
                }
                _ => {
                    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
                    if let ["status", code] = parts.as_slice() {
                        let code = code.parse::<u16>().unwrap();
                        let res = Response::builder()
                            .status(code)
                            .body(Body::empty())
                            .unwrap();
                        debug!("Replying with {res:?}");
                        Ok(res)
                    } else {
                        let res = Response::builder().status(404).body(Body::empty()).unwrap();
                        Ok(res)
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let listen_port: u16 = std::env::var("LISTEN_PORT")
        .map(|s| s.parse().unwrap())
        .unwrap_or_default();

    let upstream = hyper::Server::bind(&format!("127.0.0.1:{listen_port}").parse()?)
        .http2_only(true)
        .serve(make_service_fn(|_addr| async move {
            Ok::<_, Infallible>(TestService)
        }));

    let upstream_addr = upstream.local_addr();
    println!("I listen on {upstream_addr}");

    upstream.await?;

    Ok(())
}
