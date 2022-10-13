#![feature(type_alias_impl_trait)]

mod helpers;

use bytes::BytesMut;
use httparse::{Status, EMPTY_HEADER};
use pretty_assertions::assert_eq;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
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
fn echo_non_chunked_body() {
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
