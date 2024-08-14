//! Section 7: Error Codes

use buffet::{IntoHalves, Piece};
use loona_h2::{ErrorCode, FrameType, GoAway, HeadersFlags, StreamId};

use crate::Conn;

/// Unknown or unsupported error codes MUST NOT trigger any special
/// behavior. These MAY be treated by an implementation as being
/// equivalent to INTERNAL_ERROR.
pub async fn sends_goaway_frame_with_unknown_error_code<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        FrameType::GoAway.into_frame(StreamId::CONNECTION),
        GoAway {
            additional_debug_data: Piece::empty(),
            error_code: ErrorCode(0xff),
            last_stream_id: StreamId(0),
        },
    )
    .await?;

    conn.verify_connection_still_alive().await?;

    Ok(())
}

/// Unknown or unsupported error codes MUST NOT trigger any special
/// behavior. These MAY be treated by an implementation as being
/// equivalent to INTERNAL_ERROR.
pub async fn sends_rst_stream_frame_with_unknown_error_code<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.write_rst_stream(stream_id, ErrorCode(0xff)).await?;

    conn.verify_connection_still_alive().await?;

    Ok(())
}
