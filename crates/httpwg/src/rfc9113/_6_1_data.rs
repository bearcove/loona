//! Section 6.1: DATA

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, StreamId};

use crate::{Conn, ErrorC};

/// DATA frames MUST be associated with a stream. If a DATA frame is
/// received whose stream identifier field is 0x0, the recipient
/// MUST respond with a connection error (Section 5.4.1) of type
/// PROTOCOL_ERROR.
pub async fn sends_data_frame_with_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_data(StreamId::CONNECTION, true, b"test").await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// If a DATA frame is received whose stream is not in "open" or
/// "half-closed (local)" state, the recipient MUST respond with
/// a stream error (Section 5.4.2) of type STREAM_CLOSED.
///
/// Note: This test case is duplicated with 5.1.
pub async fn sends_data_frame_on_invalid_stream_state<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method".into(), "POST".into());

    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    conn.write_data(stream_id, true, b"test").await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// If the length of the padding is the length of the frame payload
/// or greater, the recipient MUST treat this as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_data_frame_with_invalid_pad_length<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method".into(), "POST".into());
    headers.insert("content-length".into(), "4".into());

    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    // DATA frame:
    // frame length: 5, pad length: 6
    conn.send(b"\x00\x00\x05\x00\x09\x00\x00\x00\x01").await?;
    conn.send(b"\x06\x54\x65\x73\x74").await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
