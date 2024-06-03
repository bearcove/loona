//! Section 5: Streams and Multiplexing

use enumflags2::BitFlags;
use fluke_buffet::IntoHalves;
use fluke_h2_parse::{
    ContinuationFlags, EncodedFrameType, FrameType, HeadersFlags, Setting, StreamId,
};

use crate::{dummy_bytes, Conn, ErrorC, Headers};

//---- Section 5.1: Stream States

/// idle:
/// Receiving any frame other than HEADERS or PRIORITY on a stream
/// in this state MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn idle_sends_data_frame<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;
    conn.write_data(StreamId(1), true, b"test").await?;

    // 6.1 says this should be treated as a stream error: If a DATA frame is
    // received whose stream is not in "open" or "half-closed (local)" state,
    // the recipient MUST respond with a stream error (Section 5.4.2) of type
    // STREAM_CLOSED.
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
    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
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
    conn.send_empty_post_to_root(stream_id).await?;
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
    conn.send_empty_post_to_root(stream_id).await?;
    conn.send_empty_post_to_root(stream_id).await?;
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
    conn.send_empty_post_to_root(stream_id).await?;

    let headers = conn.common_headers("POST");
    let block_fragment = conn.encode_headers(&headers)?;
    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    // In 6.10, the spec allows PROTOCOL_ERROR as well as STREAM_CLOSED:
    // A CONTINUATION frame MUST be preceded by a HEADERS, PUSH_PROMISE or
    // CONTINUATION frame without the END_HEADERS flag set. A recipient that
    // observes violation of this rule MUST respond with a connection error
    // (Section 5.4.1) of type PROTOCOL_ERROR.
    conn.verify_stream_error(ErrorC::StreamClosed | ErrorC::ProtocolError)
        .await?;

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

    let headers = conn.common_headers("POST");
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

    let headers = conn.common_headers("POST");
    let block_fragment = conn.encode_headers(&headers)?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment.clone())
        .await?;
    conn.write_rst_stream(stream_id, ErrorC::Cancel).await?;
    conn.send_empty_post_to_root(stream_id).await?;
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

    let headers = conn.common_headers("POST");
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

    // In 6.10, the spec allows PROTOCOL_ERROR as well as STREAM_CLOSED:
    // A CONTINUATION frame MUST be preceded by a HEADERS, PUSH_PROMISE or
    // CONTINUATION frame without the END_HEADERS flag set. A recipient that
    // observes violation of this rule MUST respond with a connection error
    // (Section 5.4.1) of type PROTOCOL_ERROR.
    conn.verify_stream_error(ErrorC::StreamClosed | ErrorC::ProtocolError)
        .await?;

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

    conn.send_empty_post_to_root(stream_id).await?;
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

    conn.send_empty_post_to_root(stream_id).await?;
    conn.verify_stream_close(stream_id).await?;
    conn.send_empty_post_to_root(stream_id).await?;
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

    conn.send_empty_post_to_root(stream_id).await?;
    conn.verify_stream_close(stream_id).await?;

    let continuation_fragment = conn.encode_headers(&conn.dummy_headers(1))?;
    conn.write_continuation(
        stream_id,
        ContinuationFlags::EndHeaders,
        continuation_fragment,
    )
    .await?;

    // In 6.10, the spec allows PROTOCOL_ERROR as well as STREAM_CLOSED
    conn.verify_connection_error(ErrorC::StreamClosed | ErrorC::ProtocolError)
        .await?;

    Ok(())
}

//--- Section 5.1.1: Stream Identifiers

/// An endpoint that receives an unexpected stream identifier
/// MUST respond with a connection error (Section 5.4.1) of
/// type PROTOCOL_ERROR.
pub async fn sends_even_numbered_stream_identifier<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.send_empty_post_to_root(StreamId(2)).await?;
    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// An endpoint that receives an unexpected stream identifier
/// MUST respond with a connection error (Section 5.4.1) of
/// type PROTOCOL_ERROR.
pub async fn sends_smaller_stream_identifier<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.send_empty_post_to_root(StreamId(5)).await?;
    conn.send_empty_post_to_root(StreamId(3)).await?;
    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

//---- Section 5.1.2: Stream Concurrency

pub async fn exceeds_concurrent_stream_limit<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    // Skip this test case when SETTINGS_MAX_CONCURRENT_STREAMS is unlimited.
    let max_streams = match conn.settings.max_concurrent_streams {
        Some(value) => value,
        None => return Ok(()), // spec.ErrSkipped equivalent
    };

    // Set INITIAL_WINDOW_SIZE to zero to prevent the peer from closing the stream.
    conn.write_settings(&[(Setting::InitialWindowSize, 0)])
        .await?;

    for i in 0..=max_streams {
        conn.send_empty_post_to_root(StreamId(1 + i * 2)).await?;
    }

    conn.verify_stream_error(ErrorC::ProtocolError | ErrorC::RefusedStream)
        .await?;

    Ok(())
}

// Note: In RFC9113, Section 5.3 mostly describes how prioritization in HTTP/2
// was a failure, and is now deprecated. RFC9218 describes another scheme, cf.
// https://www.rfc-editor.org/rfc/rfc9218.html

//---- Section 5.4.1: Connection Error Handling

/// After sending the GOAWAY frame for an error condition,
/// the endpoint MUST close the TCP connection.
pub async fn invalid_ping_frame_for_connection_close<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    // PING frame with invalid stream ID
    conn.write_frame(
        FrameType::Ping(BitFlags::default()).into_frame(StreamId(3)),
        dummy_bytes(8),
    )
    .await?;

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

    conn.write_frame(
        FrameType::Ping(BitFlags::default()).into_frame(StreamId(3)),
        dummy_bytes(8),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

//---- Section 5.5: Extending HTTP/2

/// Extension frames that appear in the middle of a header block
/// (Section 4.3) are not permitted; these MUST be treated as
/// a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn unknown_extension_frame_in_header_block<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    conn.write_frame(
        FrameType::Unknown(EncodedFrameType {
            ty: 0xff,
            flags: 0x0,
        })
        .into_frame(stream_id),
        dummy_bytes(8),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
