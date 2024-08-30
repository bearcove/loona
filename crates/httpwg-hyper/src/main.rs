use httpwg_harness::Proto;
use httpwg_harness::Settings;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;

use hyper_util::server::conn::auto;
use service::TestService;
use std::sync::Arc;
use tokio::net::TcpListener;

mod service;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let settings = Settings::from_env().unwrap();
    let ln = TcpListener::bind(settings.listen_addr).await.unwrap();
    let listen_addr = ln.local_addr().unwrap();
    settings.print_listen_line(listen_addr);

    match settings.proto {
        Proto::TLS => {
            let server_config = Settings::gen_rustls_server_config().unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

            while let Ok((stream, _)) = ln.accept().await {
                stream.set_nodelay(true).unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let stream = acceptor.accept(stream).await.unwrap();

                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    match stream.get_ref().1.alpn_protocol() {
                        Some(b"h2") => {
                            builder = builder.http2_only();
                        }
                        Some(b"http/1.1") => {
                            builder = builder.http1_only();
                        }
                        _ => {}
                    }
                    builder
                        .serve_connection(TokioIo::new(stream), TestService)
                        .await
                });
            }
        }
        _ => {
            while let Ok((stream, _)) = ln.accept().await {
                stream.set_nodelay(true).unwrap();

                tokio::spawn(async move {
                    let mut builder = auto::Builder::new(TokioExecutor::new());

                    match settings.proto {
                        Proto::H1 => {
                            builder = builder.http1_only();
                        }
                        Proto::H2C => {
                            builder = builder.http2_only();
                        }
                        _ => {
                            // nothing
                        }
                    }
                    builder
                        .serve_connection(TokioIo::new(stream), TestService)
                        .await
                });
            }
        }
    }
}
