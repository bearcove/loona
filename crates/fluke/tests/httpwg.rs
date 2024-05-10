use std::{rc::Rc, sync::Arc};

use fluke::{Body, Encoder, ExpectResponseHeaders, Responder, Response, ResponseDone};
use fluke_buffet::{IntoHalves, RollMut};
use http::StatusCode;
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

pub(crate) fn setup_tracing() {
    let targets = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log.parse::<Targets>().unwrap()
    } else {
        Targets::new()
            .with_default(Level::INFO)
            .with_target("fluke", Level::DEBUG)
            .with_target("want", Level::INFO)
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_file(false)
        .with_line_number(false);

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

#[test]
fn httpwg() {
    setup_tracing();

    async fn inner() -> eyre::Result<()> {
        tracing::debug!("Starting test...");

        let ln = fluke::buffet::net::TcpListener::bind("127.0.0.1:0".parse()?).await?;
        let ln_addr = ln.local_addr()?;

        let accept_conn = async move {
            let (stream, _addr) = ln.accept().await?;
            tracing::debug!("Accepted connection");
            let (r, w) = stream.into_halves();
            let conf = fluke::h2::ServerConf {
                ..Default::default()
            };

            let client_buf = RollMut::alloc()?;
            let driver = Rc::new(TestDriver);
            fluke::h2::serve((r, w), Rc::new(conf), client_buf, driver).await?;
            tracing::debug!("Connection done");
            Ok::<_, eyre::Report>(())
        };

        fluke_buffet::spawn(async move {
            accept_conn.await.unwrap();
        });

        let groups = httpwg::all_groups();
        for group in groups {
            println!("Group: {}", group.name);
            for test in group.tests {
                let config = Arc::new(httpwg::Config {});

                tracing::debug!("Connecting to server...");
                let io = fluke::buffet::net::TcpStream::connect(ln_addr).await?;
                tracing::debug!("Connecting to server... done!");
                let conn = httpwg::Conn::new(io);
                let result = test.run(config, conn).await;
                println!("  Test: {:?}", result);
            }
        }

        Ok(())
    }

    fluke_buffet::start(async move { inner().await.unwrap() });
}
