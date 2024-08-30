use driver::TestDriver;
use httpwg_harness::{Proto, Settings};
use ktls::CorkStream;
use std::{
    mem::ManuallyDrop,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd},
    rc::Rc,
    sync::Arc,
};
use tokio_rustls::TlsAcceptor;

use buffet::{
    net::{TcpListener, TcpStream},
    IntoHalves, RollMut,
};
use loona::{
    error::ServeError,
    h1,
    h2::{self, types::H2ConnectionError},
};
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

mod driver;

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
                    let mut server_config = Settings::gen_rustls_server_config().unwrap();
                    server_config.enable_secret_extraction = true;
                    let driver = TestDriver;
                    let h1_conf = Rc::new(h1::ServerConf::default());
                    let h2_conf = Rc::new(h2::ServerConf::default());

                    // until we come up with `loona-rustls`, we need to temporarily go through a
                    // tokio TcpStream
                    let acceptor = TlsAcceptor::from(Arc::new(server_config));
                    let stream = unsafe { std::net::TcpStream::from_raw_fd(stream.into_raw_fd()) };
                    stream.set_nonblocking(true).unwrap();
                    let stream = tokio::net::TcpStream::from_std(stream)?;
                    let stream = CorkStream::new(stream);
                    let stream = acceptor.accept(stream).await?;

                    let is_h2 = matches!(stream.get_ref().1.alpn_protocol(), Some(b"h2"));
                    tracing::debug!(%is_h2, "Performed TLS handshake");

                    let stream = ktls::config_ktls_server(stream).await?;

                    tracing::debug!("Set up kTLS");
                    let (drained, stream) = stream.into_raw();
                    let drained = drained.unwrap_or_default();
                    tracing::debug!("{} bytes already decoded by rustls", drained.len());

                    // and back to a buffet TcpStream
                    let stream = stream.to_uring_tcp_stream()?;

                    let mut client_buf = RollMut::alloc()?;
                    client_buf.put(&drained[..])?;

                    if is_h2 {
                        tracing::info!("Using HTTP/2");
                        h2::serve(stream.into_halves(), h2_conf, client_buf, Rc::new(driver))
                            .await
                            .map_err(|e| eyre::eyre!("h2 server error: {e:?}"))?;
                    } else {
                        tracing::info!("Using HTTP/1.1");
                        h1::serve(stream.into_halves(), h1_conf, client_buf, driver)
                            .await
                            .map_err(|e| eyre::eyre!("h1 server error: {e:?}"))?;
                    }
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

pub trait ToUringTcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream>;
}

impl ToUringTcpStream for tokio::net::TcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream> {
        {
            let sock = ManuallyDrop::new(unsafe { socket2::Socket::from_raw_fd(self.as_raw_fd()) });
            // tokio needs the socket to be "non-blocking" (as in: return EAGAIN)
            // buffet needs it to be "blocking" (as in: let io_uring do the op async)
            sock.set_nonblocking(false)?;
        }
        let stream = unsafe { TcpStream::from_raw_fd(self.as_raw_fd()) };
        std::mem::forget(self);
        Ok(stream)
    }
}
