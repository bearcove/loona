use std::{any::Any, net::SocketAddr, process::Stdio};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tracing::debug;

pub async fn start() -> eyre::Result<(SocketAddr, impl Any)> {
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<SocketAddr>();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut cmd = Command::new(format!(
        "{manifest_dir}/test-crates/hyper-testbed/target/release/hyper-testbed"
    ));
    cmd.stdout(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let mut addr_tx = Some(addr_tx);

    tokio_uring::spawn(async move {
        let stdout = BufReader::new(stdout);
        let mut lines = stdout.lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(rest) = line.strip_prefix("I listen on ") {
                let addr = rest.parse::<SocketAddr>().unwrap();
                if let Some(addr_tx) = addr_tx.take() {
                    addr_tx.send(addr).unwrap();
                }
            } else {
                debug!("[upstream] {}", line);
            }
        }
    });

    let upstream_addr = addr_rx.await?;
    Ok((upstream_addr, child))
}
