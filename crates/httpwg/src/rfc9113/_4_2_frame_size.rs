//! Section 4.2: Frame Size

use fluke_buffet::{IntoHalves, Piece};
use fluke_h2_parse::{Frame, FrameType, StreamId};

use crate::{Conn, ErrorC, Headers};

/// An endpoint MUST send an error code of FRAME_SIZE_ERROR if a frame
/// exceeds the size defined in SETTINGS_MAX_FRAME_SIZE, exceeds any
/// limit defined for the frame type, or is too small to contain mandatory frame data
pub async fn frame_exceeding_max_size<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = common_headers();
    headers;

    // FIXME: here, h2spec sends a POST request, and then the DATA frame is too large.
    // This ends up resetting the stream only (for some implementations)

    let f = Frame::new(FrameType::Headers(Default::default()), StreamId(1));
    _ = conn.write_frame(f, vec![0u8; 16384 + 1]).await;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

fn common_headers() -> Headers {
    let mut headers = Headers::default();
    headers.insert(":method", "POST".into());
    headers
}
