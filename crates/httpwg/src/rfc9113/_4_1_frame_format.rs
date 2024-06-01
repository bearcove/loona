//! Section 4.1: Frame Format

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{EncodedFrameType, Frame, FrameType, StreamId};

use crate::Conn;

// Type: The 8-bit type of the frame. The frame type determines
// the format and semantics of the frame. Implementations MUST
// ignore and discard any frame that has a type that is unknown.
pub async fn sends_frame_with_unknown_type<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame {
            frame_type: FrameType::Unknown(EncodedFrameType {
                ty: 0xff,
                flags: 0x0,
            }),
            len: 8,
            reserved: 0,
            stream_id: StreamId::CONNECTION,
        },
        b"0".repeat(8),
    )
    .await?;

    let data = b"pingback";
    tracing::info!("writing PING");
    conn.write_ping(false, &data[..]).await?;
    tracing::info!("verifying PING ACK");
    conn.verify_ping_frame_with_ack(data).await?;
    tracing::info!("verified!");

    Ok(())
}
