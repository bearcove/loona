use std::{net::SocketAddr, rc::Rc};

use hring::buffet::{RollMut, SplitOwned};
use tokio::net::TcpListener;

use crate::SDriver;

pub(crate) async fn spawn_server(addr: SocketAddr) -> color_eyre::Result<SocketAddr> {
    let ln = TcpListener::bind(addr).await?;
    let addr = ln.local_addr()?;
    tracing::info!("Listening on {}", ln.local_addr()?);

    let _task = tokio::task::spawn_local(async move { run_server(ln).await.unwrap() });

    Ok(addr)
}

pub(crate) async fn run_server(ln: TcpListener) -> color_eyre::Result<()> {
    loop {
        let (stream, addr) = ln.accept().await?;
        tracing::info!(%addr, "Accepted connection from");
        let conf = Rc::new(hring::h2::ServerConf::default());
        let client_buf = RollMut::alloc()?;
        let driver = Rc::new(SDriver);

        tokio::task::spawn_local(async move {
            if let Err(e) = hring::h2::serve(stream.split_owned(), conf, client_buf, driver).await {
                tracing::error!("error serving client {}: {}", addr, e);
            }
        });
    }
}
