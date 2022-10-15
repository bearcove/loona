use std::{convert::Infallible, future::Future, net::SocketAddr, rc::Rc};

use futures::FutureExt;
use hyper::service::make_service_fn;
use tracing::debug;

use self::{fixed_driver::FixedConnDriver, sample_hyper_server::TestService};

pub(crate) mod fixed_driver;
pub(crate) mod sample_hyper_server;
pub(crate) mod tracing_common;

pub(crate) fn run(test: impl Future<Output = eyre::Result<()>>) {
    tokio_uring::start(async {
        tracing_common::setup_tracing();
        color_eyre::install().unwrap();

        if let Err(e) = test.await {
            panic!("Error: {}", e);
        }
    });
}

pub(crate) fn tcp_serve_h1_once(
) -> eyre::Result<(SocketAddr, impl Future<Output = eyre::Result<()>>)> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let upstream =
        hyper::Server::bind(&"[::]:0".parse()?).serve(make_service_fn(|_addr| async move {
            Ok::<_, Infallible>(TestService)
        }));
    let upstream_addr = upstream.local_addr();
    let upstream = upstream.with_graceful_shutdown(rx.map(|_| ()));

    let ln = tokio_uring::net::TcpListener::bind("[::]:0".parse()?)?;
    let ln_addr = ln.local_addr()?;
    let conn_dv = Rc::new(FixedConnDriver { upstream_addr });

    let upstream_fut = async move {
        debug!("Started h1 server");
        upstream.await?;
        debug!("Done with h1 server");
        Ok::<_, eyre::Report>(())
    };

    let proxy_fut = async move {
        let (stream, remote_addr) = ln.accept().await?;
        debug!("Accepted connection from {remote_addr}");
        hring::serve_h1(conn_dv, stream).await?;
        debug!("Done serving h1 connection, initiating graceful shutdown");
        drop(tx);
        Ok(())
    };

    let fut = async move {
        tokio::try_join!(upstream_fut, proxy_fut)?;
        Ok(())
    };
    Ok((ln_addr, fut))
}
