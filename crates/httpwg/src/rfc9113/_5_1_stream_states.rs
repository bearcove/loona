//! Section 5.1: Stream States

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{ContinuationFlags, HeadersFlags, StreamId};

use crate::{Conn, ErrorC};

/// idle:
/// Receiving any frame other than HEADERS or PRIORITY on a stream
/// in this state MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn idle_sends_data_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_data(StreamId(1), true, b"test").await?;

    // This is an unclear part of the specification. Section 6.1 says
    // to treat this as a stream error.
    // --------
    // If a DATA frame is received whose stream is not in "open" or
    // "half-closed (local)" state, the recipient MUST respond with
    // a stream error (Section 5.4.2) of type STREAM_CLOSED.
    conn.verify_stream_error(ErrorC::ProtocolError | ErrorC::StreamClosed)
        .await?;

    Ok(())
}

/// idle:
/// Receiving any frame other than HEADERS or PRIORITY on a stream
/// in this state MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn idle_sends_rst_stream_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_rst_stream(StreamId(1), ErrorC::Cancel).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// idle:
/// Receiving any frame other than HEADERS or PRIORITY on a stream
/// in this state MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn idle_sends_window_update_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_window_update(StreamId(1), 100).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// idle:
/// Receiving any frame other than HEADERS or PRIORITY on a stream
/// in this state MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn idle_sends_continuation_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_continuation(StreamId(1), ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// half-closed (remote):
/// If an endpoint receives additional frames, other than
/// WINDOW_UPDATE, PRIORITY, or RST_STREAM, for a stream that is in
/// this state, it MUST respond with a stream error (Section 5.4.2)
/// of type STREAM_CLOSED.
pub async fn half_closed_remote_sends_data_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        stream_id,
        HeadersFlags::EndHeaders | HeadersFlags::EndStream,
        block_fragment,
    )
    .await?;

    conn.write_data(stream_id, true, b"test").await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// half-closed (remote):
/// If an endpoint receives additional frames, other than
/// WINDOW_UPDATE, PRIORITY, or RST_STREAM, for a stream that is in
/// this state, it MUST respond with a stream error (Section 5.4.2)
/// of type STREAM_CLOSED.
pub async fn half_closed_remote_sends_headers_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment.clone())
        .await?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// half-closed (remote):
/// If an endpoint receives additional frames, other than
/// WINDOW_UPDATE, PRIORITY, or RST_STREAM, for a stream that is in
/// this state, it MUST respond with a stream error (Section 5.4.2)
/// of type STREAM_CLOSED.
pub async fn half_closed_remote_sends_continuation_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment.clone())
        .await?;

    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frame other than PRIORITY after
/// receiving a RST_STREAM MUST treat that as a stream error
/// (Section 5.4.2) of type STREAM_CLOSED.
pub async fn closed_sends_data_frame_after_rst_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.write_rst_stream(stream_id, ErrorC::Cancel).await?;

    conn.write_data(stream_id, true, b"test").await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frame other than PRIORITY after
/// receiving a RST_STREAM MUST treat that as a stream error
/// (Section 5.4.2) of type STREAM_CLOSED.
pub async fn closed_sends_headers_frame_after_rst_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment.clone())
        .await?;

    conn.write_rst_stream(stream_id, ErrorC::Cancel).await?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frame other than PRIORITY after
/// receiving a RST_STREAM MUST treat that as a stream error
/// (Section 5.4.2) of type STREAM_CLOSED.
pub async fn closed_sends_continuation_frame_after_rst_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.write_rst_stream(stream_id, ErrorC::Cancel).await?;

    let dummy_headers = conn.dummy_headers(1);
    let continuation_fragment = conn.encode_headers(&dummy_headers)?;

    conn.write_continuation(
        stream_id,
        ContinuationFlags::EndHeaders,
        continuation_fragment,
    )
    .await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frames after receiving a frame
/// with the END_STREAM flag set MUST treat that as a connection
/// error (Section 6.4.1) of type STREAM_CLOSED.
pub async fn closed_sends_data_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    conn.verify_stream_close(stream_id).await?;

    conn.write_data(stream_id, true, b"test").await?;

    conn.verify_stream_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frames after receiving a frame
/// with the END_STREAM flag set MUST treat that as a connection
/// error (Section 6.4.1) of type STREAM_CLOSED.
pub async fn closed_sends_headers_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment.clone())
        .await?;

    conn.verify_stream_close(stream_id).await?;

    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    conn.verify_connection_error(ErrorC::StreamClosed).await?;

    Ok(())
}

/// closed:
/// An endpoint that receives any frames after receiving a frame
/// with the END_STREAM flag set MUST treat that as a connection
/// error (Section 6.4.1) of type STREAM_CLOSED.
pub async fn closed_sends_continuation_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    conn.verify_stream_close(stream_id).await?;

    let dummy_headers = conn.dummy_headers(1);
    let continuation_fragment = conn.encode_headers(&dummy_headers)?;

    conn.write_continuation(
        stream_id,
        ContinuationFlags::EndHeaders,
        continuation_fragment,
    )
    .await?;

    conn.verify_connection_error(ErrorC::StreamClosed).await?;

    Ok(())
}
