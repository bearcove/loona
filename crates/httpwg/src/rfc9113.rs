use std::rc::Rc;

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{Frame, FrameType, StreamId};
use tracing::debug;

use crate::{test_struct, Config, Conn, Test};

test_struct!("3.4", test3_4, Test3_4);
async fn test3_4<IO: IntoHalves + 'static>(
    _config: Rc<Config>,
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    tracing::debug!("Writing http/2 preface");
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;
    tracing::debug!("Sleeping a bit!");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    tracing::debug!("Okay that's it");
    Ok(())
}

test_struct!("4.2", test4_2, Test4_2);
async fn test4_2<IO: IntoHalves + 'static>(
    _config: Rc<Config>,
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    // TODO: handshake facility on `conn`

    tracing::debug!("Writing http/2 preface");
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;

    let f = Frame::new(
        FrameType::Settings(Default::default()),
        StreamId::CONNECTION,
    );
    conn.write_frame(f, vec![0u8; 16384 + 1]).await.unwrap();
    debug!("Sending out all zeroes for payload...");
    // it's okay if the peer doesn't let us send that.
    _ = conn.send(vec![0u8; 16384 + 1]).await;
    debug!("Now reading.");

    // sleep for 1 second
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    Ok(())
}
