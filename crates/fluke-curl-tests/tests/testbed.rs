use std::{any::Any, net::SocketAddr, path::PathBuf, process::Stdio};

#[cfg(target_os = "linux")]
use libc::{prctl, PR_SET_PDEATHSIG, SIGKILL};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tracing::debug;

#[cfg(target_os = "windows")]
const EXE_FILE_EXT: &str = ".exe";

#[cfg(not(target_os = "windows"))]
const EXE_FILE_EXT: &str = "";

pub async fn start() -> eyre::Result<(SocketAddr, impl Any)> {
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<SocketAddr>();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_dir = PathBuf::from(manifest_dir);
    let project_dir = manifest_dir.parent().unwrap().parent().unwrap();

    let exe_name = format!("fluke-hyper-testbed{EXE_FILE_EXT}");
    let binary_path = project_dir.join("target").join("release").join(exe_name);
    debug!("Using testbed binary: {}", binary_path.display());
    let mut cmd = Command::new(binary_path);
    cmd.stdout(Stdio::piped());

    // Only Linux gets the nice "I'm taking you with me" feature for now.
    #[cfg(target_os = "linux")]
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

    fluke::maybe_uring::spawn(async move {
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
