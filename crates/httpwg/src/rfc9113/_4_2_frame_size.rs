//! Section 4.2: Frame Size

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, StreamId};

use crate::{dummy_bytes, Conn, ErrorC};

/// An endpoint MUST send an error code of FRAME_SIZE_ERROR if a frame
/// exceeds the size defined in SETTINGS_MAX_FRAME_SIZE, exceeds any
/// limit defined for the frame type, or is too small to contain mandatory frame data
pub async fn frame_exceeding_max_size<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method", "POST".into());
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(block_fragment, stream_id, HeadersFlags::EndHeaders)
        .await?;

    // this is okay if it fails
    _ = conn
        .write_data(stream_id, true, dummy_bytes(conn.max_frame_size + 1))
        .await;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

pub async fn dummy_test<IO: IntoHalves + 'static>(mut conn: Conn<IO>) -> eyre::Result<()> {
    Ok(())
}
