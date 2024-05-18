use fluke_buffet::IntoHalves;
use fluke_h2_parse::{FrameType, Settings, StreamId};
use tracing::debug;

use crate::{
    rfc9113::{DEFAULT_FRAME_SIZE, DEFAULT_WINDOW_SIZE},
    Conn,
};

/// The server connection preface consists of a potentially empty
/// SETTINGS frame (Section 6.5) that MUST be the first frame
/// the server sends in the HTTP/2 connection.
pub async fn http2_connection_preface<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.send(&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..]).await?;
    conn.write_frame(
        FrameType::Settings(Default::default()).into_frame(StreamId::CONNECTION),
        Settings {
            initial_window_size: DEFAULT_WINDOW_SIZE,
            max_frame_size: DEFAULT_FRAME_SIZE,
            ..Default::default()
        },
    )
    .await?;

    debug!("Okay that's it");
    Ok(())
}
