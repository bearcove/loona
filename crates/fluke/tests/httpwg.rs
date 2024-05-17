use std::rc::Rc;

use fluke::{Body, Encoder, ExpectResponseHeaders, Responder, Response, ResponseDone};
use fluke_buffet::{IntoHalves, PipeRead, PipeWrite, ReadOwned, RollMut, WriteOwned};
use http::StatusCode;
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

pub(crate) fn setup_tracing() {
    color_eyre::install().unwrap();

    let targets = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log.parse::<Targets>().unwrap()
    } else {
        Targets::new()
            .with_default(Level::INFO)
            .with_target("fluke", Level::DEBUG)
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

struct TestDriver;

impl fluke::ServerDriver for TestDriver {
    async fn handle<E: Encoder>(
        &self,
        _req: fluke::Request,
        _req_body: &mut impl Body,
        mut res: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>> {
        let mut buf = RollMut::alloc()?;

        buf.put(b"Continue")?;

        res.write_interim_response(Response {
            status: StatusCode::CONTINUE,
            ..Default::default()
        })
        .await?;

        buf.put(b"OK")?;

        _ = buf;

        let res = res
            .write_final_response(Response {
                status: StatusCode::OK,
                ..Default::default()
            })
            .await?;

        let res = res.finish_body(None).await?;

        Ok(res)
    }
}

pub struct TwoHalves<W, R>(W, R);
impl<W: WriteOwned, R: ReadOwned> IntoHalves for TwoHalves<W, R> {
    type Read = R;
    type Write = W;

    fn into_halves(self) -> (Self::Read, Self::Write) {
        (self.1, self.0)
    }
}

pub fn start_server() -> httpwg::Conn<TwoHalves<PipeWrite, PipeRead>> {
    let (server_write, client_read) = fluke::buffet::pipe();
    let (client_write, server_read) = fluke::buffet::pipe();

    let serve_fut = async move {
        let conf = fluke::h2::ServerConf {
            ..Default::default()
        };
        let conf = Rc::new(conf);

        let client_buf = RollMut::alloc()?;
        let driver = Rc::new(TestDriver);
        let io = (server_read, server_write);
        fluke::h2::serve(io, conf, client_buf, driver).await?;
        tracing::debug!("http/2 server done");
        Ok::<_, eyre::Report>(())
    };

    fluke_buffet::spawn(async move {
        serve_fut.await.unwrap();
    });

    httpwg::Conn::new(TwoHalves(client_write, client_read))
}

httpwg::gen_tests! {{
   crate::setup_tracing();

   fluke_buffet::start(async move {
       let conn = crate::start_server();
       let config = std::rc::Rc::new(httpwg::Config {});
       let test = Test::default();
       let result = httpwg::Test::run(&test, config, conn).await;
       result.unwrap()
   });
}}
