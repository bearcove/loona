//! Section 3: Starting HTTP/2

use buffet::IntoHalves;
use loona_h2::PREFACE;

use crate::{rfc9113::default_settings, Conn, ErrorC, FrameT};

//---- Section 3.4: HTTP/2 connection preface

/// The server connection preface consists of a potentially empty
/// SETTINGS frame (Section 6.5) that MUST be the first frame
/// the server sends in the HTTP/2 connection.
pub async fn sends_client_connection_preface<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.send(PREFACE).await?;

    let settings = default_settings();
    conn.write_settings(settings).await?;

    let (frame, _) = conn.wait_for_frame(FrameT::Settings).await.unwrap();
    assert!(!frame.is_ack(), "The server connection preface MUST be the first frame the server sends in the HTTP/2 connection.");

    Ok(())
}

/// Clients and servers MUST treat an invalid connection preface as
/// a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_invalid_connection_preface<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.send("INVALID CONNECTION PREFACE\r\n\r\n").await?;
    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
