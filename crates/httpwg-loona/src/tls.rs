use b_x::BxForResults;
use buffet::net::TcpStream;
use buffet::IntoHalves;
use buffet::RollMut;
use httpwg_harness::Settings;
use ktls::CorkStream;
use loona::h1;
use loona::h2;
use std::mem::ManuallyDrop;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::rc::Rc;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

use crate::driver::TestDriver;

pub(super) async fn handle_tls_conn(stream: TcpStream) -> b_x::Result<()> {
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

    let stream = ktls::config_ktls_server(stream).await.bx()?;

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
        h2::serve(stream.into_halves(), h2_conf, client_buf, Rc::new(driver)).await?;
    } else {
        tracing::info!("Using HTTP/1.1");
        h1::serve(stream.into_halves(), h1_conf, client_buf, driver).await?;
    }
    Ok(())
}

pub trait ToUringTcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream>;
}

impl ToUringTcpStream for tokio::net::TcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream> {
        {
            let sock = ManuallyDrop::new(unsafe { socket2::Socket::from_raw_fd(self.as_raw_fd()) });
            // tokio needs the socket to be "non-blocking" (as in: return
            // EAGAIN) buffet needs it to be
            // "blocking" (as in: let io_uring do the op async)
            sock.set_nonblocking(false)?;
        }
        let stream = unsafe { TcpStream::from_raw_fd(self.as_raw_fd()) };
        std::mem::forget(self);
        Ok(stream)
    }
}
