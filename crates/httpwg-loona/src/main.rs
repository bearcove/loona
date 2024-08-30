use driver::TestDriver;
use httpwg_harness::Proto;
use httpwg_harness::Settings;
use std::rc::Rc;

use buffet::net::TcpListener;
use buffet::IntoHalves;
use buffet::RollMut;
use loona::error::ServeError;
use loona::h1;
use loona::h2;
use loona::h2::types::H2ConnectionError;
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

mod driver;

#[cfg(target_os = "linux")]
mod tls;

fn main() {
    setup_tracing_and_error_reporting();
    buffet::start(real_main());
}

async fn real_main() {
    let settings = Settings::from_env().unwrap();
    let ln = TcpListener::bind(settings.listen_addr).await.unwrap();
    let listen_addr = ln.local_addr().unwrap();
    settings.print_listen_line(listen_addr);

    loop {
        tracing::debug!("Accepting...");
        let (stream, _addr) = ln.accept().await.unwrap();

        let conn_fut = async move {
            let client_buf = RollMut::alloc().unwrap();

            match settings.proto {
                Proto::H1 => {
                    let driver = TestDriver;
                    let server_conf = Rc::new(h1::ServerConf {
                        ..Default::default()
                    });
                    let io = stream.into_halves();

                    if let Err(e) = h1::serve(io, server_conf, client_buf, driver).await {
                        tracing::warn!("http/1 server error: {e:?}");
                    }
                    tracing::debug!("http/1 server done");
                }
                Proto::H2C => {
                    let driver = Rc::new(TestDriver);
                    let server_conf = Rc::new(h2::ServerConf {
                        ..Default::default()
                    });
                    let io = stream.into_halves();

                    if let Err(e) = h2::serve(io, server_conf, client_buf, driver).await {
                        let mut should_ignore = false;
                        match &e {
                            ServeError::H2ConnectionError(H2ConnectionError::WriteError(e)) => {
                                if e.kind() == std::io::ErrorKind::BrokenPipe {
                                    should_ignore = true;
                                }
                            }
                            _ => {
                                // okay
                            }
                        }

                        if !should_ignore {
                            tracing::warn!("http/2 server error: {e:?}");
                        }
                    }
                    tracing::debug!("http/2 server done");
                }
                #[cfg(not(target_os = "linux"))]
                Proto::TLS => {
                    panic!("TLS support is provided through kTLS, which we only support the Linux variant of right now");
                }

                #[cfg(target_os = "linux")]
                Proto::TLS => {
                    tls::handle_tls_conn(stream)
                        .await
                        .map_err(|e| eyre::eyre!("tls error: {e:?}"))?;
                }
            }
            Ok::<_, eyre::Report>(())
        };

        let before_spawn = std::time::Instant::now();
        buffet::spawn(conn_fut);
        tracing::debug!("spawned connection in {:?}", before_spawn.elapsed());
    }
}

fn setup_tracing_and_error_reporting() {
    color_eyre::install().unwrap();

    let targets = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log.parse::<Targets>().unwrap()
    } else {
        Targets::new()
            .with_default(Level::INFO)
            .with_target("loona", Level::DEBUG)
            .with_target("httpwg", Level::DEBUG)
            .with_target("want", Level::INFO)
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_file(false)
        .with_line_number(false)
        .without_time();

    tracing_subscriber::registry()
        .with(targets)
        .with(fmt_layer)
        .init();
}
