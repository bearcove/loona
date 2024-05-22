//! Section 3: Starting HTTP/2

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{FrameType, Settings, SettingsFlags, StreamId};

use crate::{
    rfc9113::{DEFAULT_FRAME_SIZE, DEFAULT_WINDOW_SIZE},
    Conn, ErrorC, FrameT,
};

/// The server connection preface consists of a potentially empty
/// SETTINGS frame (Section 6.5) that MUST be the first frame
/// the server sends in the HTTP/2 connection.
pub async fn sends_client_connection_preface<IO: IntoHalves + 'static>(
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

    let (frame, _) = conn.wait_for_frame(FrameT::Settings).await.unwrap();
    match frame.frame_type {
        FrameType::Settings(flags) => {
            assert!(!flags.contains(SettingsFlags::Ack), "The server connection preface MUST be the first frame the server sends in the HTTP/2 connection.");
        }
        _ => unreachable!(),
    };

    Ok(())
}

/// Clients and servers MUST treat an invalid connection preface as
/// a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_invalid_connection_preface<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.send(&b"INVALID CONNECTION PREFACE\r\n\r\n"[..])
        .await?;
    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
