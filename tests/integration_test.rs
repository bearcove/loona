#![feature(type_alias_impl_trait)]

mod helpers;

use hyper::{Body, Request};
use std::net::SocketAddr;

use crate::helpers::tcp_serve_h1_once;

#[test]
fn test_simple_server() {
    helpers::run(async move {
        let (server_addr, server_fut) = tcp_serve_h1_once()?;
        let client_fut = do_client(server_addr);

        tokio::try_join!(server_fut, client_fut)?;
        Ok(())
    })
}

async fn do_client(ln_addr: SocketAddr) -> eyre::Result<()> {
    let test_body = "A fairly simple request body";

    let client = hyper::Client::new();
    let req = Request::builder()
        .uri(format!("http://{ln_addr}/hi"))
        .body(Body::from(test_body))
        .unwrap();

    let res = client.request(req).await.unwrap();
    dbg!(res.headers());
    let body = hyper::body::to_bytes(res.into_body())
        .await
        .unwrap()
        .to_vec();
    assert_eq!(body, test_body.as_bytes());

    Ok(())
}
