use std::{
    mem::ManuallyDrop,
    net::ToSocketAddrs,
    os::unix::prelude::{AsRawFd, FromRawFd},
    rc::Rc,
    sync::Arc,
};

use b_x::{BxForResults, BX};
use http::Version;
use ktls::CorkStream;
use loona::{
    buffet::{net::TcpStream, IntoHalves, RollMut},
    h1, h2, Body, Encoder, ExpectResponseHeaders, Method, Request, Responder, ResponseDone,
    ServerDriver,
};
use rustls::{pki_types::PrivatePkcs8KeyDer, ServerConfig};
use tokio::net::TcpListener;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

pub(crate) fn main() {
    loona::buffet::start(async_main())
}

async fn async_main() {
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if std::env::args().any(|a| a == "--get") {
        sample_http_request().await.unwrap();
        return;
    }

    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let crt = certified_key.cert.der();
    let key = certified_key.key_pair.serialize_der();

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![crt.clone()],
            PrivatePkcs8KeyDer::from(key.clone()).into(),
        )
        .unwrap();

    server_config.key_log = Arc::new(rustls::KeyLogFile::new());
    server_config.enable_secret_extraction = true;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let acceptor = Rc::new(acceptor);

    let pt_h1_ln = TcpListener::bind("[::]:7080").await.unwrap();
    info!(
        "Serving plaintext HTTP/1.1 on {}",
        pt_h1_ln.local_addr().unwrap()
    );

    let pt_h2_ln = TcpListener::bind("[::]:7082").await.unwrap();
    info!(
        "Serving plaintext HTTP/2 on {}",
        pt_h2_ln.local_addr().unwrap()
    );

    let tls_ln = TcpListener::bind("[::]:7443").await.unwrap();
    info!("Serving HTTPS on {}", tls_ln.local_addr().unwrap());

    let h1_conf = Rc::new(h1::ServerConf::default());
    let h2_conf = Rc::new(h2::ServerConf::default());

    let pt_h1_loop = {
        let h1_conf = h1_conf.clone();

        async move {
            while let Ok((stream, remote_addr)) = pt_h1_ln.accept().await {
                loona::buffet::spawn({
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
        }
    };

    let pt_h2_loop = {
        let h2_conf = h2_conf.clone();

        async move {
            while let Ok((stream, remote_addr)) = pt_h2_ln.accept().await {
                loona::buffet::spawn({
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
        }
    };

    let tls_loop = async move {
        while let Ok((stream, remote_addr)) = tls_ln.accept().await {
            loona::buffet::spawn({
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
    };

    tokio::join!(pt_h1_loop, pt_h2_loop, tls_loop);
}

enum Proto {
    H1(Rc<h1::ServerConf>),
    H2(Rc<h2::ServerConf>),
}

async fn handle_plaintext_conn(
    stream: tokio::net::TcpStream,
    remote_addr: std::net::SocketAddr,
    proto: Proto,
) -> Result<(), BX> {
    info!("Accepted connection from {remote_addr}");
    let buf = RollMut::alloc()?;

    let stream = stream.to_uring_tcp_stream()?;
    let driver = SDriver {};

    match proto {
        Proto::H1(h1_conf) => {
            info!("Using HTTP/1.1");
            loona::h1::serve(stream.into_halves(), h1_conf, buf, driver).await?;
        }
        Proto::H2(h2_conf) => {
            info!("Using HTTP/2");
            loona::h2::serve(stream.into_halves(), h2_conf, buf, Rc::new(driver)).await?;
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
) -> b_x::Result<()> {
    info!("Accepted connection from {remote_addr}");
    let stream = CorkStream::new(stream);
    let stream = acceptor.accept(stream).await?;

    let sc = stream.get_ref().1;
    let alpn_proto = sc
        .alpn_protocol()
        .and_then(|p| std::str::from_utf8(p).ok().map(|s| s.to_string()));
    debug!(?alpn_proto, "Performed TLS handshake");

    let stream = ktls::config_ktls_server(stream).await.bx()?;

    debug!("Set up kTLS");
    let (drained, stream) = stream.into_raw();
    let drained = drained.unwrap_or_default();
    debug!("{} bytes already decoded by rustls", drained.len());

    let stream = stream.to_uring_tcp_stream()?;

    let mut buf = RollMut::alloc()?;
    buf.put(&drained[..])?;

    let driver = SDriver {};

    match alpn_proto.as_deref() {
        Some("h2") => {
            info!("Using HTTP/2");
            loona::h2::serve(stream.into_halves(), h2_conf, buf, Rc::new(driver)).await?;
        }
        Some("http/1.1") | None => {
            info!("Using HTTP/1.1");
            loona::h1::serve(stream.into_halves(), h1_conf, buf, driver).await?;
        }
        Some(other) => {
            b_x::bail!("Unsupported ALPN protocol: {}", other)
        }
    }

    Ok(())
}

struct SDriver {}

impl<OurEncoder> ServerDriver<OurEncoder> for SDriver
where
    OurEncoder: Encoder,
{
    type Error = BX;

    async fn handle(
        &self,
        mut req: loona::Request,
        req_body: &mut impl Body,
        respond: Responder<OurEncoder, ExpectResponseHeaders>,
    ) -> b_x::Result<Responder<OurEncoder, ResponseDone>> {
        info!("Handling {:?} {}", req.method, req.uri);

        let addr = "httpbingo.org:80"
            .to_socket_addrs()?
            .next()
            .expect("http bingo should be up");
        let transport = TcpStream::connect(addr).await?;
        debug!("Connected to httpbingo");

        let driver = CDriver { respond };

        req.version = Version::HTTP_11;
        req.headers.insert("host", "httpbingo.org".into());
        let (transport, respond) =
            h1::request(transport.into_halves(), req, req_body, driver).await?;

        // don't re-use transport for now
        drop(transport);

        Ok(respond)
    }
}

struct CDriver<OurEncoder>
where
    OurEncoder: Encoder,
{
    respond: Responder<OurEncoder, ExpectResponseHeaders>,
}

impl<OurEncoder> h1::ClientDriver for CDriver<OurEncoder>
where
    OurEncoder: Encoder,
{
    type Error = BX;
    type Return = Responder<OurEncoder, ResponseDone>;

    async fn on_informational_response(&mut self, _res: loona::Response) -> b_x::Result<()> {
        // ignore informational responses

        Ok(())
    }

    async fn on_final_response(
        self,
        res: loona::Response,
        body: &mut impl Body,
    ) -> b_x::Result<Self::Return> {
        info!("Client got final response: {}", res.status);
        let respond = self.respond;

        let mut respond = respond.write_final_response(res).await?;

        let trailers = loop {
            debug!("Reading from body {body:?}");
            match body.next_chunk().await.bx()? {
                loona::BodyChunk::Chunk(chunk) => {
                    debug!("Client got chunk of len {}", chunk.len());

                    respond.write_chunk(chunk).await?;
                }
                loona::BodyChunk::Done { trailers } => {
                    break trailers;
                }
            }
        };

        respond.finish_body(trailers).await.bx()
    }
}

struct SampleCDriver {}

impl h1::ClientDriver for SampleCDriver {
    type Error = BX;
    type Return = ();

    async fn on_informational_response(&mut self, _res: loona::Response) -> b_x::Result<()> {
        // ignore informational responses

        Ok(())
    }

    async fn on_final_response(
        self,
        res: loona::Response,
        body: &mut impl Body,
    ) -> b_x::Result<Self::Return> {
        info!("Client got final response: {}", res.status);

        loop {
            debug!("Reading from body {body:?}");
            match body.next_chunk().await.bx()? {
                loona::BodyChunk::Chunk(chunk) => {
                    debug!("Client got chunk of len {}", chunk.len());
                }
                loona::BodyChunk::Done { .. } => {
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn sample_http_request() -> b_x::Result<()> {
    info!("Doing sample HTTP request to httpbingo");

    let addr = "httpbingo.org:80"
        .to_socket_addrs()?
        .next()
        .expect("http bingo should be up");
    let transport = TcpStream::connect(addr).await?;
    debug!("Connected to httpbingo");

    let driver = SampleCDriver {};

    let req = Request {
        method: Method::Get,
        uri: "http://httpbingo.org/image/jpeg".parse().unwrap(),
        version: Version::HTTP_11,
        headers: Default::default(),
    };

    let (transport, _) = h1::request(transport.into_halves(), req, &mut (), driver).await?;
    // don't re-use transport for now
    drop(transport);

    Ok(())
}

pub trait ToUringTcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream>;
}

impl ToUringTcpStream for tokio::net::TcpStream {
    fn to_uring_tcp_stream(self) -> std::io::Result<TcpStream> {
        {
            let sock = ManuallyDrop::new(unsafe { socket2::Socket::from_raw_fd(self.as_raw_fd()) });
            // tokio needs the socket to be non-blocking but tokio-uring
            // needs it to be "blocking" (but it won't be, because io_uring)
            sock.set_nonblocking(false)?;
        }
        let stream = unsafe { TcpStream::from_raw_fd(self.as_raw_fd()) };
        std::mem::forget(self);
        Ok(stream)
    }
}
