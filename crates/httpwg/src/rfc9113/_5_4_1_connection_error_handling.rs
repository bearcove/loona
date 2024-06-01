//! Section 5.4.1: Connection Error Handling

use fluke_buffet::IntoHalves;

use crate::{Conn, ErrorC};

/// After sending the GOAWAY frame for an error condition,
/// the endpoint MUST close the TCP connection.
pub async fn invalid_ping_frame_for_connection_close<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    // PING frame with invalid stream ID
    conn.send(b"\x00\x00\x08\x06\x00\x00\x00\x00\x03").await?;
    conn.send(b"\x00\x00\x00\x00\x00\x00\x00\x00").await?;

    conn.verify_connection_close().await?;

    Ok(())
}

// An endpoint that encounters a connection error SHOULD first send
// a GOAWAY frame (Section 6.8) with the stream identifier of the last
// stream that it successfully received from its peer.
pub async fn test_invalid_ping_frame_for_goaway<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    // PING frame with invalid stream ID
    conn.send(b"\x00\x00\x08\x06\x00\x00\x00\x00\x03").await?;
    conn.send(b"\x00\x00\x00\x00\x00\x00\x00\x00").await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
