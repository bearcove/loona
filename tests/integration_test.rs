#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]

mod helpers;

use bytes::BytesMut;
use hring::{
    h1, Body, BodyChunk, ChanRead, ChanWrite, Headers, Method, ReadWritePair, Request, Response,
    RollMut, WriteOwned,
};
use httparse::{Status, EMPTY_HEADER};
use pretty_assertions::assert_eq;
use pretty_hex::PrettyHex;
use std::{net::SocketAddr, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tracing::debug;

mod proxy;
mod testbed;

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
fn serve_api() {
    helpers::run(async move {
        let conf = h1::ServerConf::default();
        let conf = Rc::new(conf);

        struct TestDriver {}

        impl h1::ServerDriver for TestDriver {
            async fn handle<T: WriteOwned>(
                &self,
                _req: hring::Request,
                _req_body: &mut impl Body,
                res: h1::Responder<T, h1::ExpectResponseHeaders>,
            ) -> eyre::Result<h1::Responder<T, h1::ResponseDone>> {
                let mut buf = RollMut::alloc()?;

                buf.put(b"Continue")?;
                let reason = buf.take_all();

                let res = res
                    .write_interim_response(Response {
                        code: 101,
                        headers: Headers::default(),
                        reason,
                        version: 1,
                    })
                    .await?;

                buf.put(b"OK")?;
                let reason = buf.take_all();

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

                Ok(res)
            }
        }

        let (tx, read) = ChanRead::new();
        let (mut rx, write) = ChanWrite::new();
        let transport = ReadWritePair(read, write);
        let client_buf = RollMut::alloc()?;
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

        let mut buf = RollMut::alloc()?;

        buf.put(b"/")?;
        let path = buf.take_all();

        _ = buf;

        let req = Request {
            method: Method::Get,
            path,
            version: 1,
            headers: Headers::default(),
        };

        struct TestDriver {}

        impl h1::ClientDriver for TestDriver {
            type Return = ();

            async fn on_informational_response(&self, _res: Response) -> eyre::Result<()> {
                todo!("got informational response!")
            }

            async fn on_final_response(
                self,
                res: Response,
                body: &mut impl Body,
            ) -> eyre::Result<Self::Return> {
                debug!(
                    "got final response! content length = {:?}, is chunked = {}",
                    res.headers.content_length(),
                    res.headers.is_chunked_transfer_encoding(),
                );

                while let BodyChunk::Chunk(chunk) = body.next_chunk().await? {
                    debug!("got a chunk: {:?}", chunk.hex_dump());
                }

                Ok(())
            }
        }

        let driver = TestDriver {};
        let request_fut = tokio_uring::spawn(async {
            #[allow(clippy::let_unit_value)]
            let mut body = ();
            h1::request(Rc::new(transport), req, &mut body, driver).await
        });

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
fn proxy_statuses() {
    #[allow(drop_bounds)]
    async fn client(ln_addr: SocketAddr, _guard: impl Drop) -> eyre::Result<()> {
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
                debug!("client got a complete response");
                assert_eq!(res.code, Some(status));

                _ = buf.split();
                break 'read_response;
            }
        }

        debug!("http client shutting down");
        Ok(())
    }

    helpers::run(async move {
        let (upstream_addr, _upstream_guard) = testbed::start().await?;
        let (ln_addr, guard, proxy_fut) = proxy::start(upstream_addr).await?;
        let client_fut = client(ln_addr, guard);

        tokio::try_join!(proxy_fut, client_fut)?;
        debug!("everything has been joined");

        Ok(())
    });
}

#[test]
fn proxy_echo_body_content_len() {
    #[allow(drop_bounds)]
    async fn client(ln_addr: SocketAddr, _guard: impl Drop) -> eyre::Result<()> {
        let socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        let mut buf = BytesMut::with_capacity(256);

        debug!("Sending a small body");
        let body = "Please return to sender.";
        let content_len = body.len();

        let (mut read, mut write) = socket.into_split();

        let send_fut = async move {
            write
                .write_all(
                    format!("POST /echo-body HTTP/1.1\r\ncontent-length: {content_len}\r\n\r\n")
                        .as_bytes(),
                )
                .await?;
            write.flush().await?;

            write.write_all(body.as_bytes()).await?;
            write.flush().await?;

            Ok::<(), eyre::Report>(())
        };
        tokio_uring::spawn(async move {
            if let Err(e) = send_fut.await {
                panic!("Error sending request: {e}");
            }
        });

        debug!("Reading response...");
        'read_response: loop {
            buf.reserve(256);
            read.read_buf(&mut buf).await?;
            debug!("After read, got {} bytes", buf.len());

            let mut headers = [EMPTY_HEADER; 16];
            let mut res = httparse::Response::new(&mut headers[..]);
            let body_offset = match res.parse(&buf[..])? {
                Status::Complete(off) => off,
                Status::Partial => continue 'read_response,
            };
            debug!("client got a response header");
            assert_eq!(res.code, Some(200));

            let mut content_len = None;
            for header in res.headers.iter() {
                debug!(
                    "Got header: {} = {:?}",
                    header.name,
                    std::str::from_utf8(header.value)
                );
                if header.name.eq_ignore_ascii_case("content-length") {
                    content_len = Some(
                        std::str::from_utf8(header.value)
                            .unwrap()
                            .parse::<usize>()
                            .unwrap(),
                    );
                }
            }
            let content_len = content_len.expect("no content-length header");

            let mut buf = buf.split_off(body_offset);

            while buf.len() < content_len {
                debug!("buf has {} bytes, need {}", buf.len(), content_len);
                if buf.capacity() == 0 {
                    buf.reserve(256);
                }
                let n = read.read_buf(&mut buf).await?;
                if n == 0 {
                    panic!("unexpected EOF");
                }
                debug!("just read {n}");
            }
            debug!("Body: {:?}", std::str::from_utf8(&buf[..]));

            break 'read_response;
        }

        debug!("http client shutting down");
        Ok(())
    }

    helpers::run(async move {
        let (upstream_addr, _upstream_guard) = testbed::start().await?;
        let (ln_addr, guard, proxy_fut) = proxy::start(upstream_addr).await?;
        let client_fut = client(ln_addr, guard);

        tokio::try_join!(proxy_fut, client_fut)?;
        debug!("everything has been joined");

        Ok(())
    });
}

#[test]
fn proxy_echo_body_chunked() {
    eyre::set_hook(Box::new(|e| eyre::DefaultHandler::default_with(e))).unwrap();

    #[allow(drop_bounds)]
    async fn client(ln_addr: SocketAddr, _guard: impl Drop) -> eyre::Result<()> {
        let socket = TcpStream::connect(ln_addr).await?;
        socket.set_nodelay(true)?;

        let mut buf = BytesMut::with_capacity(256);

        let (mut read, mut write) = socket.into_split();

        let send_fut = async move {
            write
                .write_all(b"POST /echo-body HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n")
                .await?;
            write.flush().await?;

            let chunks = ["a first chunk", "a second chunk", "a third and final chunk"];
            for chunk in chunks {
                let chunk_len = chunk.len();
                write
                    .write_all(format!("{chunk_len:x}\r\n{chunk}\r\n").as_bytes())
                    .await?;
                write.flush().await?;
            }

            write.write_all(b"0\r\n\r\n").await?;
            write.flush().await?;

            Ok::<(), eyre::Report>(())
        };
        tokio_uring::spawn(async move {
            if let Err(e) = send_fut.await {
                panic!("Error sending request: {e}");
            }
        });

        debug!("Reading response...");
        'read_response: loop {
            buf.reserve(256);
            read.read_buf(&mut buf).await?;
            debug!("After read, got {} bytes", buf.len());

            let mut headers = [EMPTY_HEADER; 16];
            let mut res = httparse::Response::new(&mut headers[..]);
            let body_offset = match res.parse(&buf[..])? {
                Status::Complete(off) => off,
                Status::Partial => continue 'read_response,
            };
            debug!("client got a response header");
            assert_eq!(res.code, Some(200));

            let mut chunked = false;
            for header in res.headers.iter() {
                debug!(
                    "Got header: {} = {:?}",
                    header.name,
                    std::str::from_utf8(header.value)
                );
                if header.name.eq_ignore_ascii_case("transfer-encoding") {
                    let value = std::str::from_utf8(header.value).unwrap();
                    if value.eq_ignore_ascii_case("chunked") {
                        chunked = true;
                    }
                }
            }
            assert!(chunked);

            let mut buf = buf.split_off(body_offset);
            let mut body: Vec<u8> = Vec::new();

            loop {
                let status = httparse::parse_chunk_size(&buf[..]).unwrap();
                match status {
                    Status::Complete((offset, chunk_len)) => {
                        debug!("Reading {chunk_len} chunk");
                        buf = buf.split_off(offset);

                        while buf.len() < chunk_len as usize {
                            debug!("buf has {} bytes, need {}", buf.len(), chunk_len);
                            if buf.capacity() == 0 {
                                buf.reserve(256);
                            }
                            let n = read.read_buf(&mut buf).await?;
                            if n == 0 {
                                panic!("unexpected EOF");
                            }
                            debug!("just read {n}");
                        }

                        body.extend_from_slice(&buf[..chunk_len as usize]);
                        buf = buf.split_off(chunk_len as usize);

                        while buf.len() < 2 {
                            if buf.capacity() == 0 {
                                buf.reserve(2);
                            }
                            let n = read.read_buf(&mut buf).await?;
                            if n == 0 {
                                panic!("unexpected EOF");
                            }
                        }

                        assert_eq!(&buf[..2], b"\r\n");
                        buf = buf.split_off(2);

                        if chunk_len == 0 {
                            debug!("Got the last chunk");
                            break;
                        }
                    }
                    Status::Partial => {
                        // partial chunk, reading more
                        buf.reserve(256);
                        let n = read.read_buf(&mut buf).await?;
                        if n == 0 {
                            panic!("unexpected EOF");
                        }
                    }
                }
            }

            debug!("Full response body: {:?}", body.hex_dump());
            let body = String::from_utf8(body)?;
            assert_eq!(body, "a first chunka second chunka third and final chunk");

            break 'read_response;
        }

        debug!("http client shutting down");
        Ok(())
    }

    helpers::run(async move {
        let (upstream_addr, _upstream_guard) = testbed::start().await?;
        let (ln_addr, guard, proxy_fut) = proxy::start(upstream_addr).await?;
        let client_fut = client(ln_addr, guard);

        tokio::try_join!(proxy_fut, client_fut)?;
        debug!("everything has been joined");

        Ok(())
    });
}
