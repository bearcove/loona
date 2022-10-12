#![feature(type_alias_impl_trait)]

mod helpers;

use std::{convert::Infallible, net::SocketAddr, rc::Rc};

use alt_http::{ConnectionDriver, RequestDriver};
use hyper::{service::make_service_fn, Body, Request};

use crate::helpers::sample_hyper_server::TestService;

#[test]
fn test_simple_server() {
    helpers::run(test_simple_server_inner());

    async fn test_simple_server_inner() -> eyre::Result<()> {
        let upstream =
            hyper::Server::bind(&"[::]:0".parse()?).serve(make_service_fn(|_addr| async {
                Ok::<_, Infallible>(TestService)
            }));
        let upstream_addr = upstream.local_addr();
        tokio_uring::spawn(upstream);

        let ln = tokio_uring::net::TcpListener::bind("[::]:0".parse()?)?;
        let ln_addr = ln.local_addr()?;

        let client_jh = tokio_uring::spawn(do_client(ln_addr));

        struct CDriver {
            upstream_addr: SocketAddr,
        }

        struct RDriver {
            upstream_addr: SocketAddr,
        }

        impl ConnectionDriver for CDriver {
            type RequestDriver = RDriver;

            fn build_request_context(
                &self,
                _req: &httparse::Request,
            ) -> eyre::Result<Self::RequestDriver> {
                Ok(RDriver {
                    upstream_addr: self.upstream_addr,
                })
            }
        }

        impl RequestDriver for RDriver {
            fn upstream_addr(&self) -> eyre::Result<std::net::SocketAddr> {
                Ok(self.upstream_addr)
            }

            fn keep_header(&self, _name: &str) -> bool {
                true
            }
        }

        let conn_dv = Rc::new(CDriver { upstream_addr });

        let (stream, _remote_addr) = ln.accept().await?;
        alt_http::serve_h1(conn_dv, stream).await?;

        client_jh.await??;

        Ok(())
    }
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
