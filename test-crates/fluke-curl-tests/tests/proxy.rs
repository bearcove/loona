use fluke::{
    buffet::RollMut,
    h1,
    maybe_uring::{
        io::IntoHalves,
        net::{TcpReadHalf, TcpWriteHalf},
    },
    Body, BodyChunk, Encoder, ExpectResponseHeaders, HeadersExt, Responder, Response, ResponseDone,
    ServerDriver,
};
use http::StatusCode;
use std::{cell::RefCell, future::Future, net::SocketAddr, rc::Rc};
use tracing::debug;

pub type TransportPool = Rc<RefCell<Vec<(TcpReadHalf, TcpWriteHalf)>>>;

pub struct ProxyDriver {
    pub upstream_addr: SocketAddr,
    pub pool: TransportPool,
}

impl ServerDriver for ProxyDriver {
    async fn handle<E: Encoder>(
        &self,
        req: fluke::Request,
        req_body: &mut impl Body,
        mut respond: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>> {
        if req.headers.expects_100_continue() {
            debug!("Sending 100-continue");
            let res = Response {
                status: StatusCode::CONTINUE,
                ..Default::default()
            };
            respond.write_interim_response(res).await?;
        }

        let transport = {
            let mut pool = self.pool.borrow_mut();
            pool.pop()
        };

        let transport = if let Some(transport) = transport {
            debug!("re-using existing transport!");
            transport
        } else {
            debug!("making new connection to upstream!");
            fluke::maybe_uring::net::TcpStream::connect(self.upstream_addr)
                .await?
                .into_halves()
        };

        let driver = ProxyClientDriver { respond };

        let (transport, res) = h1::request(transport, req, req_body, driver).await?;

        if let Some(transport) = transport {
            let mut pool = self.pool.borrow_mut();
            // FIXME: leaky abstraction, `h1::request` returns both halves of the
            // transport, which are both actually `Rc<TcpStream>`
            pool.push(transport);
        }

        Ok(res)
    }
}

struct ProxyClientDriver<E>
where
    E: Encoder,
{
    respond: Responder<E, ExpectResponseHeaders>,
}

impl<E> h1::ClientDriver for ProxyClientDriver<E>
where
    E: Encoder,
{
    type Return = Responder<E, ResponseDone>;

    async fn on_informational_response(&mut self, res: Response) -> eyre::Result<()> {
        debug!("Got informational response {}", res.status);
        Ok(())
    }

    async fn on_final_response(
        self,
        res: Response,
        body: &mut impl Body,
    ) -> eyre::Result<Self::Return> {
        let respond = self.respond;
        let mut respond = respond.write_final_response(res).await?;

        let trailers = loop {
            match body.next_chunk().await? {
                BodyChunk::Chunk(chunk) => {
                    respond.write_chunk(chunk).await?;
                }
                BodyChunk::Done { trailers } => {
                    // should we do something here in case of
                    // content-length mismatches or something?
                    break trailers;
                }
            }
        };

        let respond = respond.finish_body(trailers).await?;

        Ok(respond)
    }
}

pub async fn start(
    upstream_addr: SocketAddr,
) -> eyre::Result<(
    SocketAddr,
    impl Drop,
    impl Future<Output = eyre::Result<()>>,
)> {
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

    let ln = fluke::maybe_uring::net::TcpListener::bind("127.0.0.1:0".parse()?).await?;
    let ln_addr = ln.local_addr()?;

    let proxy_fut = async move {
        let conf = Rc::new(h1::ServerConf::default());
        let pool: TransportPool = Default::default();

        enum Event {
            Accepted((fluke::maybe_uring::net::TcpStream, SocketAddr)),
            ShuttingDown,
        }

        loop {
            let ev = tokio::select! {
                accept_res = ln.accept() => {
                    Event::Accepted(accept_res?)
                },
                _ = &mut rx => {
                    Event::ShuttingDown
                }
            };

            match ev {
                Event::Accepted((transport, remote_addr)) => {
                    debug!("Accepted connection from {remote_addr}");

                    let pool = pool.clone();
                    let conf = conf.clone();

                    fluke::maybe_uring::spawn(async move {
                        let driver = ProxyDriver {
                            upstream_addr,
                            pool,
                        };
                        h1::serve(
                            transport.into_halves(),
                            conf,
                            RollMut::alloc().unwrap(),
                            driver,
                        )
                        .await
                        .unwrap();
                        debug!("Done serving h1 connection");
                    });
                }
                Event::ShuttingDown => {
                    debug!("Shutting down proxy");
                    break;
                }
            }
        }

        debug!("Proxy server shutting down.");
        drop(pool);

        Ok(())
    };

    Ok((ln_addr, tx, proxy_fut))
}
