use std::rc::Rc;

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{Frame, FrameType, StreamId};
use tracing::debug;

use crate::{test_struct, Config, Conn, Test, TestGroup};

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

test_struct!("4.1", test4_1, Test4_1);
async fn test4_1<IO: IntoHalves + 'static>(
    _config: Rc<Config>,
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    // TODO: handshake facility on `conn`

    tracing::debug!("Writing http/2 preface");
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;

    let f = Frame::new(
        FrameType::Settings(Default::default()),
        StreamId::CONNECTION,
    )
    .with_len(16384 + 1); // make it too big
    debug!("Writing a settings frame of size 16384 + 1");
    conn.write_frame(f).await.unwrap();
    debug!("Sending out all zeroes for payload...");
    conn.send(vec![0u8; 16384 + 1]).await.unwrap();
    debug!("Now reading.");

    // sleep for 1 second
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    Ok(())
}

pub fn group<IO: IntoHalves + 'static>() -> TestGroup<IO> {
    fn t<IO: IntoHalves + 'static, T: Test<IO> + Default + 'static>() -> Box<dyn Test<IO>> {
        Box::new(T::default())
    }

    TestGroup {
        name: "RFC 9113".to_owned(),
        tests: vec![t::<IO, Test3_4>()],
    }
}
