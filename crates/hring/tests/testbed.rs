use std::{any::Any, net::SocketAddr, path::PathBuf, process::Stdio};

use libc::{prctl, PR_SET_PDEATHSIG, SIGKILL};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tracing::debug;

pub async fn start() -> eyre::Result<(SocketAddr, impl Any)> {
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<SocketAddr>();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = PathBuf::from(manifest_dir);
    let project_dir = manifest_dir.parent().unwrap().parent().unwrap();
    let test_crates_dir = project_dir.join("test-crates");
    let hyper_testbed_dir = test_crates_dir.join("hyper-testbed");
    let binary_path = hyper_testbed_dir
        .join("target")
        .join("release")
        .join("hyper-testbed");
    debug!("Using testbed binary: {}", binary_path.display());
    let mut cmd = Command::new(binary_path);
    cmd.stdout(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            prctl(PR_SET_PDEATHSIG, SIGKILL);
            Ok(())
        })
    };
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
