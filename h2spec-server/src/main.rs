#![allow(incomplete_features)]
#![feature(async_fn_in_trait)]

use std::{collections::VecDeque, net::SocketAddr, rc::Rc};

use hring::{
    http::{StatusCode, Version},
    tokio_uring::{self, net::TcpListener},
    Body, BodyChunk, Encoder, ExpectResponseHeaders, Headers, Request, Responder, Response,
    ResponseDone, RollMut, ServerDriver,
};
use tokio::process::Command;
use tracing_subscriber::EnvFilter;

fn main() {
    color_eyre::install().unwrap();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|e| {
            eprintln!("Couldn't parse RUST_LOG: {e}");
            EnvFilter::try_new("info").unwrap()
        }))
        .init();

    hring::tokio_uring::start(async move { real_main().await.unwrap() })
}

struct SDriver;

impl ServerDriver for SDriver {
    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> color_eyre::Result<Responder<E, ResponseDone>> {
        tracing::info!(
            "Handling {:?} {:?}, content_len = {:?}",
            req.method,
            req.path,
            req_body.content_len()
        );

        let res = Response {
            version: Version::HTTP_2,
            status: StatusCode::OK,
            headers: Headers::default(),
        };
        let mut body = TestBody::default();
        respond.write_final_response_with_body(res, &mut body).await
    }
}

#[derive(Default, Debug)]
struct TestBody {
    eof: bool,
}

impl TestBody {
    const CONTENTS: &str = "I am a test body";
}

impl Body for TestBody {
    fn content_len(&self) -> Option<u64> {
        Some(Self::CONTENTS.len() as _)
    }

    fn eof(&self) -> bool {
        self.eof
    }

    async fn next_chunk(&mut self) -> color_eyre::eyre::Result<BodyChunk> {
        if self.eof {
            Ok(BodyChunk::Done { trailers: None })
        } else {
            self.eof = true;
            Ok(BodyChunk::Chunk(Self::CONTENTS.as_bytes().into()))
        }
    }
}

async fn real_main() -> color_eyre::Result<()> {
    let addr: SocketAddr = "[::]:8888".parse()?;
    let ln = TcpListener::bind(addr)?;
    tracing::info!("Listening on {}", ln.local_addr()?);

    let _task = tokio_uring::spawn(async move { run_server(ln).await.unwrap() });

    let mut args = std::env::args().skip(1).collect::<VecDeque<_>>();
    if matches!(args.get(0).map(|s| s.as_str()), Some("--")) {
        args.pop_front();
    }
    tracing::info!("Custom args: {args:?}");

    Command::new("h2spec")
        .arg("-p")
        .arg("8888")
        .arg("-o")
        .arg("1")
        .args(&args)
        .spawn()?
        .wait()
        .await?;

    Ok(())
}

async fn run_server(ln: TcpListener) -> color_eyre::Result<()> {
    loop {
        let (stream, addr) = ln.accept().await?;
        tracing::info!(%addr, "Accepted connection from");
        let conf = Rc::new(hring::h2::ServerConf::default());
        let client_buf = RollMut::alloc()?;
        let driver = Rc::new(SDriver);
        tokio_uring::spawn(async move {
            if let Err(e) = hring::h2::serve(stream, conf, client_buf, driver).await {
                tracing::error!("error serving client {}: {}", addr, e);
            }
        });
    }
}
