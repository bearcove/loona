//! Section 4.1: Frame Format

use crate::Conn;
use fluke_buffet::IntoHalves;
use fluke_h2_parse::{EncodedFrameType, Frame, FrameType};

/// Implementations MUST ignore and discard frames of unknown types.
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
            ..Default::default()
        },
        b"0".repeat(8),
    )
    .await?;

    let data = b"sfunktyp";
    conn.write_ping(false, data).await?;
    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}

/// Unused flags MUST be ignored on receipt and MUST be left
/// unset (0x00) when sending.
pub async fn sends_frame_with_unused_flags<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let data = b"sfunflgs";
    conn.write_frame(
        Frame {
            frame_type: FrameType::Unknown(EncodedFrameType {
                // type 6 = PING
                ty: 6,
                flags: 0xff & !0x01, // remove ack flag,
            }),
            ..Default::default()
        },
        data,
    )
    .await?;
    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}

/// Reserved: A reserved 1-bit field. The semantics of this bit are
/// undefined, and the bit MUST remain unset (0x00) when sending and
/// MUST be ignored when receiving.
pub async fn sends_frame_with_reserved_bit_set<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let data = b"sfresbit";
    conn.write_frame(
        Frame {
            frame_type: FrameType::Ping(Default::default()),
            ..Default::default()
        },
        data,
    )
    .await?;
    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}
