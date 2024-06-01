//! Section 4.2: Frame Size

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, StreamId};

use crate::{dummy_bytes, Conn, ErrorC};

// All implementations MUST be capable of receiving and minimally
// processing frames up to 2^14 octets in length, plus the 9-octet
// frame header (Section 4.1).
pub async fn data_frame_with_max_length<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method".into(), "POST".into());
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    let data = dummy_bytes(conn.max_frame_size);
    conn.write_data(stream_id, true, data).await?;

    conn.verify_headers_frame(stream_id).await?;

    Ok(())
}

/// An endpoint MUST send an error code of FRAME_SIZE_ERROR if a frame
/// exceeds the size defined in SETTINGS_MAX_FRAME_SIZE, exceeds any
/// limit defined for the frame type, or is too small to contain mandatory frame data
pub async fn frame_exceeding_max_size<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method".into(), "POST".into());
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    // this is okay if it fails
    _ = conn
        .write_data(stream_id, true, dummy_bytes(conn.max_frame_size + 1))
        .await;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

/// A frame size error in a frame that could alter the state of
/// the entire connection MUST be treated as a connection error
/// (Section 5.4.1); this includes any frame carrying a field block
/// (Section 4.3) (that is, HEADERS, PUSH_PROMISE, and CONTINUATION),
/// a SETTINGS frame, and any frame with a stream identifier of 0.
pub async fn large_headers_frame_exceeding_max_size<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.extend(conn.dummy_headers(5));
    let block_fragment = conn.encode_headers(&headers)?;

    // this is okay if it fails
    _ = conn
        .write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}
