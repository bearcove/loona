#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

use std::{
    net::ToSocketAddrs,
    os::unix::prelude::{AsRawFd, FromRawFd},
    rc::Rc,
    sync::Arc,
};

use color_eyre::eyre;
use hring::{
    buffet::{RollMut, SplitOwned},
    h1, h2,
    tokio_uring::net::TcpStream,
    Body, Encoder, ExpectResponseHeaders, Method, Request, Responder, ResponseDone, ServerDriver,
};
use http::Version;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

fn main() -> eyre::Result<()> {
    hring::tokio_uring::start(async_main())
}

async fn async_main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if std::env::args().any(|a| a == "--get") {
        sample_http_request().await.unwrap();
        return Ok(());
    }

    let pair = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let crt = pair.serialize_der()?;
    let key = pair.serialize_private_key_der();

    let mut server_config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(vec![rustls::Certificate(crt)], rustls::PrivateKey(key))
        .unwrap();

    server_config.key_log = Arc::new(rustls::KeyLogFile::new());
    server_config.enable_secret_extraction = true;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let acceptor = Rc::new(acceptor);

    let pt_h1_ln = TcpListener::bind("[::]:7080").await?;
    info!("Serving plaintext HTTP/1.1 on {}", pt_h1_ln.local_addr()?);

    let pt_h2_ln = TcpListener::bind("[::]:7082").await?;
    info!("Serving plaintext HTTP/2 on {}", pt_h2_ln.local_addr()?);

    let tls_ln = TcpListener::bind("[::]:7443").await?;
    info!("Serving HTTPS on {}", tls_ln.local_addr()?);

    let h1_conf = Rc::new(h1::ServerConf::default());
    let h2_conf = Rc::new(h2::ServerConf::default());

    let pt_h1_loop = {
        let h1_conf = h1_conf.clone();

        async move {
            while let Ok((stream, remote_addr)) = pt_h1_ln.accept().await {
                hring::tokio_uring::spawn({
                    let h1_conf = h1_conf.clone();
                    async move {
                        if let Err(e) =
                            handle_plaintext_conn(stream, remote_addr, Proto::H1(h1_conf)).await
                        {
                            tracing::error!(%e, "Error handling connection");
                        }
                    }
                });
            }

            Ok::<_, color_eyre::Report>(())
        }
    };

    let pt_h2_loop = {
        let h2_conf = h2_conf.clone();

        async move {
            while let Ok((stream, remote_addr)) = pt_h2_ln.accept().await {
                hring::tokio_uring::spawn({
                    let h2_conf = h2_conf.clone();
                    async move {
                        if let Err(e) =
                            handle_plaintext_conn(stream, remote_addr, Proto::H2(h2_conf)).await
                        {
                            tracing::error!(%e, "Error handling connection");
                        }
                    }
                });
            }

            Ok::<_, color_eyre::Report>(())
        }
    };

    let tls_loop = async move {
        while let Ok((stream, remote_addr)) = tls_ln.accept().await {
            hring::tokio_uring::spawn({
                let acceptor = acceptor.clone();
                let h1_conf = h1_conf.clone();
                let h2_conf = h2_conf.clone();
                async move {
                    if let Err(e) =
                        handle_tls_conn(acceptor, stream, remote_addr, h1_conf, h2_conf).await
                    {
                        tracing::error!(%e, "Error handling connection");
                    }
                }
            });
        }

        Ok::<_, color_eyre::Report>(())
    };

    tokio::try_join!(pt_h1_loop, pt_h2_loop, tls_loop)?;
    Ok(())
}

enum Proto {
    H1(Rc<h1::ServerConf>),
    H2(Rc<h2::ServerConf>),
}

async fn handle_plaintext_conn(
    stream: tokio::net::TcpStream,
    remote_addr: std::net::SocketAddr,
    proto: Proto,
) -> Result<(), color_eyre::Report> {
    info!("Accepted connection from {remote_addr}");
    let buf = RollMut::alloc()?;

    let fd = stream.as_raw_fd();
    std::mem::forget(stream);
    let stream = unsafe { TcpStream::from_raw_fd(fd) };

    let driver = SDriver {};

    match proto {
        Proto::H1(h1_conf) => {
            info!("Using HTTP/1.1");
            hring::h1::serve(stream.split_owned(), h1_conf, buf, driver).await?;
        }
        Proto::H2(h2_conf) => {
            info!("Using HTTP/2");
            hring::h2::serve(stream.split_owned(), h2_conf, buf, Rc::new(driver)).await?;
        }
    }

    Ok(())
}

async fn handle_tls_conn(
    acceptor: Rc<tokio_rustls::TlsAcceptor>,
    stream: tokio::net::TcpStream,
    remote_addr: std::net::SocketAddr,
    h1_conf: Rc<h1::ServerConf>,
    h2_conf: Rc<h2::ServerConf>,
) -> Result<(), color_eyre::Report> {
    info!("Accepted connection from {remote_addr}");
    let stream = acceptor.accept(stream).await?;

    let sc = stream.get_ref().1;
    let alpn_proto = sc
        .alpn_protocol()
        .and_then(|p| std::str::from_utf8(p).ok().map(|s| s.to_string()));
    debug!(?alpn_proto, "Performed TLS handshake");

    let stream = ktls::config_ktls_server(stream)?;

    debug!("Set up kTLS");
    let (drained, stream) = stream.into_raw();
    let drained = drained.unwrap_or_default();
    debug!("{} bytes already decoded by rustls", drained.len());

    let fd = stream.as_raw_fd();
    std::mem::forget(stream);
    let stream = unsafe { TcpStream::from_raw_fd(fd) };

    let mut buf = RollMut::alloc()?;
    buf.put(&drained[..])?;

    let driver = SDriver {};

    match alpn_proto.as_deref() {
        Some("h2") => {
            info!("Using HTTP/2");
            hring::h2::serve(stream.split_owned(), h2_conf, buf, Rc::new(driver)).await?;
        }
        Some("http/1.1") | None => {
            info!("Using HTTP/1.1");
            hring::h1::serve(stream.split_owned(), h1_conf, buf, driver).await?;
        }
        Some(other) => return Err(eyre::eyre!("Unsupported ALPN protocol: {}", other)),
    }

    Ok(())
}

struct SDriver {}

impl ServerDriver for SDriver {
    async fn handle<E: Encoder>(
        &self,
        mut req: hring::Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>> {
        info!("Handling {:?} {}", req.method, req.uri);

        let addr = "httpbingo.org:80"
            .to_socket_addrs()?
            .next()
            .expect("http bingo should be up");
        let transport = Rc::new(TcpStream::connect(addr).await?);
        debug!("Connected to httpbingo");

        let driver = CDriver { respond };

        req.version = Version::HTTP_11;
        let (transport, respond) = h1::request(transport, req, req_body, driver).await?;
        // don't re-use transport for now
        drop(transport);

        Ok(respond)
    }
}

struct CDriver<E>
where
    E: Encoder,
{
    respond: Responder<E, ExpectResponseHeaders>,
}

impl<E> h1::ClientDriver for CDriver<E>
where
    E: Encoder,
{
    type Return = Responder<E, ResponseDone>;

    async fn on_informational_response(&mut self, _res: hring::Response) -> eyre::Result<()> {
        // ignore informational responses

        Ok(())
    }

    async fn on_final_response(
        self,
        res: hring::Response,
        body: &mut impl Body,
    ) -> eyre::Result<Self::Return> {
        info!("Client got final response: {}", res.status);
        let respond = self.respond;

        let mut respond = respond.write_final_response(res).await?;

        let trailers = loop {
            debug!("Reading from body {body:?}");
            match body.next_chunk().await? {
                hring::BodyChunk::Chunk(chunk) => {
                    debug!("Client got chunk of len {}", chunk.len());

                    respond.write_chunk(chunk).await?;
                }
                hring::BodyChunk::Done { trailers } => {
                    break trailers;
                }
            }
        };

        respond.finish_body(trailers).await
    }
}

struct SampleCDriver {}

impl h1::ClientDriver for SampleCDriver {
    type Return = ();

    async fn on_informational_response(&mut self, _res: hring::Response) -> eyre::Result<()> {
        // ignore informational responses

        Ok(())
    }

    async fn on_final_response(
        self,
        res: hring::Response,
        body: &mut impl Body,
    ) -> eyre::Result<Self::Return> {
        info!("Client got final response: {}", res.status);

        loop {
            debug!("Reading from body {body:?}");
            match body.next_chunk().await? {
                hring::BodyChunk::Chunk(chunk) => {
                    debug!("Client got chunk of len {}", chunk.len());
                }
                hring::BodyChunk::Done { .. } => {
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn sample_http_request() -> color_eyre::Result<()> {
    info!("Doing sample HTTP request to httpbingo");

    let addr = "httpbingo.org:80"
        .to_socket_addrs()?
        .next()
        .expect("http bingo should be up");
    let transport = Rc::new(TcpStream::connect(addr).await?);
    debug!("Connected to httpbingo");

    let driver = SampleCDriver {};

    let req = Request {
        method: Method::Get,
        uri: "http://httpbingo.org/image/jpeg".parse().unwrap(),
        version: Version::HTTP_11,
        headers: Default::default(),
    };

    let (transport, _) = h1::request(transport, req, &mut (), driver).await?;
    // don't re-use transport for now
    drop(transport);

    Ok(())
}
