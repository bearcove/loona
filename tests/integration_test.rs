#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]

mod helpers;

use bytes::BytesMut;
use futures::{FutureExt, TryStreamExt};
use hring::{
    bufpool::AggBuf,
    io::{ChanRead, ChanWrite, ReadWritePair, WriteOwned},
    proto::h1::{
        self, Body, ClientDriver, ExpectResponseHeaders, H1Responder, ResponseDone, ServerConf,
    },
    types::{Headers, Request, Response},
};
use httparse::{Status, EMPTY_HEADER};
use hyper::service::make_service_fn;
use pretty_assertions::assert_eq;
use pretty_hex::PrettyHex;
use std::{convert::Infallible, net::SocketAddr, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tracing::debug;

use crate::helpers::{sample_hyper_server::TestService, tcp_serve_h1_once};

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
                _req: hring::types::Request,
                req_body: B,
                res: h1::H1Responder<T, h1::ExpectResponseHeaders>,
            ) -> eyre::Result<(B, h1::H1Responder<T, h1::ResponseDone>)>
            where
                T: WriteOwned,
            {
                let mut buf = AggBuf::default();

                buf.write().put(b"Continue")?;
                let reason = buf.read().read_slice();
                buf = buf.split();

                let res = res
                    .write_interim_response(Response {
                        code: 101,
                        headers: Headers::default(),
                        reason,
                        version: 1,
                    })
                    .await?;

                buf.write().put(b"OK")?;
                let reason = buf.read().read_slice();
                buf = buf.split();

                _ = buf;

                let res = res
                    .write_final_response(Response {
                        code: 200,
                        headers: Headers::default(),
                        reason,
                        version: 1,
                    })
                    .await?;

                let res = res.finish_body(None).await?;

                Ok((req_body, res))
            }
        }

        let (tx, read) = ChanRead::new();
        let (mut rx, write) = ChanWrite::new();
        let transport = ReadWritePair(read, write);
        let client_buf = AggBuf::default();
        let driver = TestDriver {};
        let serve_fut = tokio_uring::spawn(h1::serve(transport, conf, client_buf, driver));

        tx.send("GET / HTTP/1.1\r\n\r\n").await?;
        let mut res_buf = BytesMut::new();
        while let Some(chunk) = rx.recv().await {
            debug!("Got a chunk:\n{:?}", chunk.hex_dump());
            res_buf.extend_from_slice(&chunk[..]);

            let mut headers = [EMPTY_HEADER; 16];
            let mut res = httparse::Response::new(&mut headers[..]);
            let body_offset = match res.parse(&res_buf[..])? {
                Status::Complete(off) => off,
                Status::Partial => {
                    debug!("partial response, continuing");
                    continue;
                }
            };

            debug!("Got a complete response: {res:?}");

            match res.code {
                Some(101) => {
                    assert_eq!(res.reason, Some("Continue"));
                    res_buf = res_buf.split_off(body_offset);
                }
                Some(200) => {
                    assert_eq!(res.reason, Some("OK"));
                    break;
                }
                _ => panic!("unexpected response code {:?}", res.code),
            }
        }

        drop(tx);

        tokio::time::timeout(Duration::from_secs(5), serve_fut).await???;

        Ok(())
    })
}

#[test]
fn request_api() {
    helpers::run(async move {
        let (tx, read) = ChanRead::new();
        let (mut rx, write) = ChanWrite::new();
        let transport = ReadWritePair(read, write);

        let mut buf = AggBuf::default();

        buf.write().put(b"GET")?;
        let method = buf.read().read_slice();
        buf = buf.split();

        buf.write().put(b"/")?;
        let path = buf.read().read_slice();
        buf = buf.split();

        _ = buf;

        let req = Request {
            method,
            path,
            version: 1,
            headers: Headers::default(),
        };

        struct TestDriver {}

        impl ClientDriver for TestDriver {
            type Return = ();

            async fn on_informational_response(&self, _res: Response) -> eyre::Result<()> {
                todo!("got informational response!")
            }

            async fn on_final_response<B>(
                self,
                res: Response,
                mut body: B,
            ) -> eyre::Result<(B, Self::Return)>
            where
                B: h1::Body,
            {
                debug!(
                    "got final response! content length = {:?}, is chunked = {}",
                    res.headers.content_length(),
                    res.headers.is_chunked_transfer_encoding(),
                );

                loop {
                    let chunk;
                    (body, chunk) = body.next_chunk().await?;
                    match chunk {
                        h1::BodyChunk::Buf(b) => {
                            debug!("got a chunk: {:?}", b.to_vec().hex_dump());
                        }
                        h1::BodyChunk::AggSlice(s) => {
                            debug!("got a chunk: {:?}", s.to_vec().hex_dump());
                        }
                        h1::BodyChunk::Eof => {
                            break;
                        }
                    }
                }

                Ok((body, ()))
            }
        }

        let driver = TestDriver {};
        let request_fut = tokio_uring::spawn(h1::request(transport, req, (), driver));

        let mut req_buf = BytesMut::new();
        while let Some(chunk) = rx.recv().await {
            debug!("Got a chunk:\n{:?}", chunk.hex_dump());
            req_buf.extend_from_slice(&chunk[..]);

            let mut headers = [EMPTY_HEADER; 16];
            let mut req = httparse::Request::new(&mut headers[..]);
            let body_offset = match req.parse(&req_buf[..])? {
                Status::Complete(off) => off,
                Status::Partial => {
                    debug!("partial request, continuing");
                    continue;
                }
            };

            debug!("Got a complete request: {req:?}");
            debug!("body_offset: {body_offset}");

            break;
        }

        let body = "Hi there";
        let body_len = body.len();
        tx.send(format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {body_len}\r\n\r\n"
        ))
        .await?;
        tx.send(body).await?;
        drop(tx);

        tokio::time::timeout(Duration::from_secs(5), request_fut).await???;

        Ok(())
    })
}

#[test]
fn proxy_verbose() {
    async fn client(ln_addr: SocketAddr) -> eyre::Result<()> {
        let mut socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        let mut buf = BytesMut::with_capacity(256);

        for status in [200, 400, 500] {
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
        let upstream =
            hyper::Server::bind(&"[::]:0".parse()?).serve(make_service_fn(|_addr| async move {
                Ok::<_, Infallible>(TestService)
            }));
        let upstream_addr = upstream.local_addr();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let upstream = upstream.with_graceful_shutdown(rx.map(|_| ()));

        let upstream_fut = async move {
            debug!("Started h1 server");
            upstream.await?;
            debug!("Done with h1 server");
            Ok::<_, eyre::Report>(())
        };

        let ln = tokio_uring::net::TcpListener::bind("[::]:0".parse()?)?;
        let ln_addr = ln.local_addr()?;

        let proxy_fut = async move {
            let conf = ServerConf::default();
            let conf = Rc::new(conf);

            struct SDriver {
                upstream_addr: SocketAddr,
            }

            impl h1::ServerDriver for SDriver {
                async fn handle<T, B>(
                    &self,
                    req: hring::types::Request,
                    req_body: B,
                    res: h1::H1Responder<T, h1::ExpectResponseHeaders>,
                ) -> eyre::Result<(B, h1::H1Responder<T, h1::ResponseDone>)>
                where
                    T: WriteOwned + 'static,
                    B: Body + 'static,
                {
                    let transport =
                        tokio_uring::net::TcpStream::connect(self.upstream_addr).await?;
                    let driver = CDriver { res };

                    let (req_body, res) = h1::request(transport, req, req_body, driver).await?;
                    Ok((req_body, res))
                }
            }

            struct CDriver<T>
            where
                T: WriteOwned,
            {
                res: H1Responder<T, ExpectResponseHeaders>,
            }

            impl<T> h1::ClientDriver for CDriver<T>
            where
                T: WriteOwned,
            {
                type Return = H1Responder<T, ResponseDone>;

                async fn on_informational_response(&self, res: Response) -> eyre::Result<()> {
                    todo!()
                }

                async fn on_final_response<B>(
                    self,
                    res: Response,
                    body: B,
                ) -> eyre::Result<(B, Self::Return)>
                where
                    B: h1::Body,
                {
                    todo!()
                }
            }

            let (transport, remote_addr) = ln.accept().await?;
            debug!("Accepted connection from {remote_addr}");
            let client_buf = AggBuf::default();
            let driver = SDriver { upstream_addr };
            h1::serve(transport, conf, client_buf, driver).await?;
            debug!("Done serving h1 connection, initiating graceful shutdown");
            drop(tx);

            Ok(())
        };

        let client_fut = client(ln_addr);

        tokio::try_join!(upstream_fut, proxy_fut, client_fut)?;

        Ok(())
    });
}
