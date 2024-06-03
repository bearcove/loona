//! Section 5.5: Extending HTTP/2

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{EncodedFrameType, Frame, FrameType, HeadersFlags, StreamId};

use crate::{Conn, ErrorC};

/// Extension frames that appear in the middle of a header block
/// (Section 4.3) are not permitted; these MUST be treated as
/// a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn unknown_extension_frame_in_header_block<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers("POST");
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    conn.write_frame(
        Frame {
            stream_id,
            frame_type: FrameType::Unknown(EncodedFrameType {
                ty: 0xff,
                flags: 0x0,
            }),
            ..Default::default()
        },
        b"0".repeat(8),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
