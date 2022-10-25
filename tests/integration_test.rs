#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]

mod helpers;

use bytes::BytesMut;
use futures::FutureExt;
use hring::{
    bufpool::{AggBuf, IoChunkList},
    io::{ChanRead, ChanWrite, ReadWritePair, WriteOwned},
    proto::h1::{
        self, Body, ClientDriver, ExpectResponseHeaders, H1Responder, ResponseDone, ServerConf,
    },
    types::{Headers, Request, Response},
};
use httparse::{Status, EMPTY_HEADER};
use pretty_assertions::assert_eq;
use pretty_hex::PrettyHex;
use std::{cell::RefCell, net::SocketAddr, process::Stdio, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
};
use tracing::debug;

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
        let request_fut = tokio_uring::spawn(h1::request(Rc::new(transport), req, (), driver));

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
    async fn client(
        ln_addr: SocketAddr,
        _tx: tokio::sync::oneshot::Sender<()>,
    ) -> eyre::Result<()> {
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
                debug!("Client got a complete response");
                assert_eq!(res.code, Some(status));

                _ = buf.split();
                break 'read_response;
            }
        }

        debug!("Http client shutting down");
        Ok(())
    }

    helpers::run(async move {
        let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<SocketAddr>();

        let mut cmd = Command::new("cargo");
        cmd.arg("run");
        cmd.arg("--release");
        cmd.arg("--manifest-path");
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        cmd.arg(format!("${manifest_dir}/hyper-testbed/Cargo.toml"));
        cmd.stdout(Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd.spawn()?;
        let mut addr_tx = Some(addr_tx);

        tokio_uring::spawn(async move {
            let stdout = child.stdout.unwrap();
            let stdout = BufReader::new(stdout);
            let mut lines = stdout.lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                if let Some(rest) = line.strip_prefix("I listen on ") {
                    let addr = rest.parse::<SocketAddr>().unwrap();
                    if let Some(addr_tx) = addr_tx.take() {
                        addr_tx.send(addr).unwrap();
                    }
                } else {
                    debug!("[upstream] {}", line);
                }
            }
        });

        let upstream_addr = addr_rx.await?;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut rx = rx.shared();

        let ln = tokio_uring::net::TcpListener::bind("[::]:0".parse()?)?;
        let ln_addr = ln.local_addr()?;

        let proxy_fut = async move {
            let conf = ServerConf::default();
            let conf = Rc::new(conf);

            type TransportPool = Rc<RefCell<Vec<Rc<tokio_uring::net::TcpStream>>>>;

            struct SDriver {
                upstream_addr: SocketAddr,
                pool: TransportPool,
            }

            impl h1::ServerDriver for SDriver {
                async fn handle<T, B>(
                    &self,
                    req: hring::types::Request,
                    req_body: B,
                    respond: h1::H1Responder<T, h1::ExpectResponseHeaders>,
                ) -> eyre::Result<(B, h1::H1Responder<T, h1::ResponseDone>)>
                where
                    T: WriteOwned,
                    B: Body,
                {
                    let transport = {
                        let mut pool = self.pool.borrow_mut();
                        pool.pop()
                    };

                    let transport = if let Some(transport) = transport {
                        debug!("re-using existing transport!");
                        transport
                    } else {
                        debug!("making new connection to upstream!");
                        Rc::new(tokio_uring::net::TcpStream::connect(self.upstream_addr).await?)
                    };

                    let driver = CDriver { respond };

                    let (transport, req_body, res) =
                        h1::request(transport, req, req_body, driver).await?;

                    if let Some(transport) = transport {
                        let mut pool = self.pool.borrow_mut();
                        pool.push(transport);
                    }

                    Ok((req_body, res))
                }
            }

            struct CDriver<T>
            where
                T: WriteOwned,
            {
                respond: H1Responder<T, ExpectResponseHeaders>,
            }

            impl<T> h1::ClientDriver for CDriver<T>
            where
                T: WriteOwned,
            {
                type Return = H1Responder<T, ResponseDone>;

                async fn on_informational_response(&self, _res: Response) -> eyre::Result<()> {
                    todo!()
                }

                async fn on_final_response<B>(
                    self,
                    res: Response,
                    mut body: B,
                ) -> eyre::Result<(B, Self::Return)>
                where
                    B: h1::Body,
                {
                    let respond = self.respond;
                    let mut respond = respond.write_final_response(res).await?;

                    loop {
                        let chunk;
                        (body, chunk) = body.next_chunk().await?;

                        match chunk {
                            h1::BodyChunk::Buf(buf) => {
                                respond = respond.write_body_chunk(buf).await?;
                            }
                            h1::BodyChunk::AggSlice(slice) => {
                                let mut list = IoChunkList::default();
                                list.push(slice);
                                let chunks = list.into_vec();
                                for chunk in chunks {
                                    match chunk {
                                        hring::bufpool::IoChunk::Static(_) => unreachable!(),
                                        hring::bufpool::IoChunk::Vec(_) => unreachable!(),
                                        hring::bufpool::IoChunk::Buf(buf) => {
                                            respond = respond.write_body_chunk(buf).await?;
                                        }
                                    }
                                }
                            }
                            h1::BodyChunk::Eof => {
                                // should we do something here in case of
                                // content-length mismatches or something?
                                break;
                            }
                        }
                    }

                    let respond = respond.finish_body(None).await?;

                    Ok((body, respond))
                }
            }

            let pool: TransportPool = Default::default();

            loop {
                tokio::select! {
                    accept_res = ln.accept() => {
                        let (transport, remote_addr) = accept_res?;
                        debug!("Accepted connection from {remote_addr}");

                        let pool = pool.clone();
                        let conf = conf.clone();

                        tokio_uring::spawn(async move {
                            let client_buf = AggBuf::default();
                            let driver = SDriver {
                                upstream_addr,
                                pool,
                            };
                            h1::serve(transport, conf, client_buf, driver)
                                .await
                                .unwrap();
                            debug!("Done serving h1 connection");
                        });
                    },
                    _ = &mut rx => {
                        debug!("Shutting down proxy");
                        break;
                    }
                }
            }

            debug!("Proxy server shutting down.");
            drop(pool);

            Ok(())
        };

        let client_fut = client(ln_addr, tx);

        tokio::try_join!(proxy_fut, client_fut)?;
        debug!("Everything has been joined!");

        Ok(())
    });
}
