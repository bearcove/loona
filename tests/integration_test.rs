#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]

mod helpers;

use bytes::BytesMut;
use futures::TryStreamExt;
use hring::{
    bufpool::AggBuf,
    io::{ChanRead, ChanWrite, ReadWritePair},
    proto::h1::{self, ServerConf},
};
use httparse::{Status, EMPTY_HEADER};
use pretty_assertions::assert_eq;
use std::{net::SocketAddr, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tracing::debug;

use crate::helpers::tcp_serve_h1_once;

// Test ideas:
// headers too large (ups/dos)
// too many headers (ups/dos)
// invalid transfer-encoding header
// chunked transfer-encoding but bad chunks
// timeout while reading request headers from downstream
// timeout while writing request headers to upstream
// connection reset from upstream after request headers
// various timeouts (slowloris, idle body, etc.)
// proxy responds with 4xx, 5xx
// upstream connection resets
// idle timeout while streaming bodies
// proxies status from upstream (200, 404, 500, etc.)
// 204
// 204 with body?
// retry / replay
// re-use upstream connection
// connection close

#[test]
fn header_too_large() {
    async fn client(ln_addr: SocketAddr) -> eyre::Result<()> {
        let mut socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        debug!("Making request...");
        socket.write_all(b"POST /hi HTTP/1.1\r\ntoo-long: ").await?;

        let garbage = "o".repeat(32678);
        let mut conn_reset = false;

        for _ in 0..128 {
            debug!("writing...");
            if let Err(e) = socket.write_all(garbage.as_bytes()).await {
                if e.kind() == std::io::ErrorKind::ConnectionReset {
                    debug!("connection reset, as expected");
                    conn_reset = true;
                    break;
                } else {
                    return Err(e.into());
                }
            };
            // this isn't great, but we're waiting for a connection reset
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(conn_reset);

        debug!("reading response");
        let mut buf = BytesMut::new();
        socket.read_buf(&mut buf).await?;
        debug!("got response: {:?}", String::from_utf8_lossy(&buf[..]));

        let mut headers = [EMPTY_HEADER; 16];
        let mut res = httparse::Response::new(&mut headers[..]);
        let _body_offset = match res.parse(&buf[..])? {
            Status::Complete(off) => off,
            Status::Partial => panic!("partial response"),
        };

        assert_eq!(res.code, Some(431));
        assert_eq!(res.reason, Some("Request Header Fields Too Large"));

        debug!("Done with client altogether");

        Ok(())
    }

    helpers::run(async move {
        let (server_addr, server_fut) = tcp_serve_h1_once()?;
        let client_fut = client(server_addr);

        tokio::try_join!(server_fut, client_fut)?;
        Ok(())
    })
}

#[test]
fn echo_body_small() {
    async fn client(ln_addr: SocketAddr) -> eyre::Result<()> {
        let test_body = "A fairly simple request body";
        let content_length = test_body.len();
        let mut socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        debug!("Sending request headers...");
        socket
        .write_all(
            format!("POST /echo-body HTTP/1.1\r\ncontent-length: {content_length}\r\nconnection: close\r\n\r\n").as_bytes(),
        )
        .await?;

        debug!("Sending request body...");
        socket.write_all(test_body.as_bytes()).await?;

        socket.flush().await?;

        debug!("Reading response...");
        let mut buf = Vec::new();
        socket.read_to_end(&mut buf).await?;
        debug!("Done reading response");

        let mut headers = [EMPTY_HEADER; 16];
        let mut res = httparse::Response::new(&mut headers[..]);
        let body_offset = match res.parse(&buf[..])? {
            Status::Complete(off) => off,
            Status::Partial => panic!("partial response"),
        };

        let body_len = buf.len() - body_offset;
        assert_eq!(content_length, body_len);

        assert_eq!(res.code, Some(200));
        assert_eq!(res.headers.len(), 2);

        let received_body = &buf[body_offset..];
        assert_eq!(test_body.as_bytes(), received_body);

        debug!("Done with client altogether");

        Ok(())
    }

    helpers::run(async move {
        let (server_addr, server_fut) = tcp_serve_h1_once()?;
        let client_fut = client(server_addr);

        tokio::try_join!(server_fut, client_fut)?;
        Ok(())
    })
}

#[test]
fn proxy_http_status() {
    async fn client(ln_addr: SocketAddr) -> eyre::Result<()> {
        let mut socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        let mut buf = BytesMut::with_capacity(256);

        for status in 200..=599 {
            debug!("Asking for a {status}");
            socket
                .write_all(format!("GET /status/{status} HTTP/1.1\r\n\r\n").as_bytes())
                .await?;

            socket.flush().await?;

            debug!("Reading response...");
            'read_response: loop {
                buf.reserve(256);
                socket.read_buf(&mut buf).await?;
                debug!("After read, got {} bytes", buf.len());

                let mut headers = [EMPTY_HEADER; 16];
                let mut res = httparse::Response::new(&mut headers[..]);
                let _body_offset = match res.parse(&buf[..])? {
                    Status::Complete(off) => off,
                    Status::Partial => continue 'read_response,
                };
                debug!("Got a complete response");
                assert_eq!(res.code, Some(status));

                _ = buf.split();
                break 'read_response;
            }
        }

        debug!("Done with client altogether");
        Ok(())
    }

    helpers::run(async move {
        let (server_addr, server_fut) = tcp_serve_h1_once()?;
        let client_fut = client(server_addr);

        tokio::try_join!(server_fut, client_fut)?;
        Ok(())
    })
}

#[test]
fn read_streaming_body() {
    async fn client(ln_addr: SocketAddr) -> eyre::Result<()> {
        let socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        let (mut send_req, conn) = hyper::client::conn::handshake(socket).await?;
        tokio::spawn(conn);

        debug!("Sending request headers");
        let res = send_req
            .send_request(
                hyper::Request::builder()
                    .uri("/stream-big-body")
                    .body(hyper::Body::empty())
                    .unwrap(),
            )
            .await?;

        debug!("Got response: {res:#?}");

        debug!("Now must read body");
        let mut body = res.into_body();

        let bb_expect = helpers::sample_hyper_server::big_body();
        let mut bb_actual = String::new();

        while let Some(chunk) = body.try_next().await? {
            debug!("Got a chunk, {} bytes", chunk.len());
            bb_actual += std::str::from_utf8(&chunk[..])?;
        }

        assert_eq!(bb_expect.len(), bb_actual.len());
        assert_eq!(bb_expect, bb_actual);

        Ok(())
    }

    helpers::run(async move {
        let (server_addr, server_fut) = tcp_serve_h1_once()?;
        let client_fut = client(server_addr);

        tokio::try_join!(server_fut, client_fut)?;
        Ok(())
    })
}

#[test]
fn serve_api() {
    helpers::run(async move {
        let conf = ServerConf::default();
        let conf = Rc::new(conf);

        struct TestDriver {}

        impl h1::ServerDriver for TestDriver {
            async fn handle<T, B>(
                &self,
                req: hring::types::Request,
                req_body: B,
                res: h1::H1Responder<T, h1::ExpectResponseHeaders>,
            ) -> eyre::Result<(B, h1::H1Responder<T, h1::ResponseDone>)>
            where
                T: hring::io::WriteOwned,
            {
                todo!()
            }
        }

        let (rx, write) = ChanWrite::new();
        let (tx, read) = ChanRead::new();
        let transport = ReadWritePair(read, write);
        let client_buf = AggBuf::default();
        let driver = TestDriver {};

        let serve_fut = h1::serve(transport, conf, client_buf, driver);
        tokio::time::timeout(Duration::from_secs(1), serve_fut).await??;

        Ok(())
    })
}
