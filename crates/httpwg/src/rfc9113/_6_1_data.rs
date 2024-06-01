//! Section 6.1: DATA

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{
    Frame, FrameType, HeadersFlags, IntoPiece, PrioritySpec, SettingCode, SettingPairs,
    SettingsFlags, StreamId,
};

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

//------------- 6.2

/// HEADERS frames MUST be associated with a stream. If a HEADERS
/// frame is received whose stream identifier field is 0x0, the
/// recipient MUST respond with a connection error (Section 5.4.1)
/// of type PROTOCOL_ERROR.
pub async fn sends_headers_frame_with_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        StreamId::CONNECTION,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// The HEADERS frame can include padding. Padding fields and flags
/// are identical to those defined for DATA frames (Section 6.1).
/// Padding that exceeds the size remaining for the header block
/// fragment MUST be treated as a PROTOCOL_ERROR.
pub async fn sends_headers_frame_with_invalid_pad_length<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    let frame = Frame::new(
        FrameType::Headers(
            HeadersFlags::Padded | HeadersFlags::EndHeaders | HeadersFlags::EndStream,
        ),
        StreamId(1),
    )
    .with_len((block_fragment.len() + 1) as _);
    let frame_header = frame.into_piece(&mut conn.scratch)?;

    conn.send(frame_header).await?;
    conn.send(vec![(block_fragment.len() + 2) as u8]).await?;
    conn.send(block_fragment).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

//------------- 6.3

/// The PRIORITY frame always identifies a stream. If a PRIORITY
/// frame is received with a stream identifier of 0x0, the recipient
/// MUST respond with a connection error (Section 5.4.1) of type
/// PROTOCOL_ERROR.
pub async fn sends_priority_frame_with_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let priority_param = PrioritySpec {
        stream_dependency: StreamId::CONNECTION,
        exclusive: false,
        weight: 255,
    };
    conn.write_priority(StreamId::CONNECTION, priority_param)
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A PRIORITY frame with a length other than 5 octets MUST be
/// treated as a stream error (Section 5.4.2) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_priority_frame_with_invalid_length<IO: IntoHalves + 'static>(
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

    let frame = Frame::new(FrameType::Priority, stream_id).with_len(4);
    let frame_header = frame.into_piece(&mut conn.scratch)?;
    conn.send(frame_header).await?;
    conn.send(b"\x80\x00\x00\x01").await?;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//------------- 6.4

/// RST_STREAM frames MUST be associated with a stream. If a
/// RST_STREAM frame is received with a stream identifier of 0x0,
/// the recipient MUST treat this as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_rst_stream_frame_with_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_rst_stream(StreamId::CONNECTION, ErrorC::Cancel)
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// RST_STREAM frames MUST NOT be sent for a stream in the "idle"
/// state. If a RST_STREAM frame identifying an idle stream is
/// received, the recipient MUST treat this as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_rst_stream_frame_on_idle_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_rst_stream(StreamId(1), ErrorC::Cancel).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A RST_STREAM frame with a length other than 4 octets MUST be
/// treated as a connection error (Section 5.4.1) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_rst_stream_frame_with_invalid_length<IO: IntoHalves + 'static>(
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

    let frame = Frame::new(FrameType::RstStream, StreamId(1)).with_len(3);
    let frame_header = frame.into_piece(&mut conn.scratch)?;
    conn.send(frame_header).await?;
    conn.send(b"\x00\x00\x00").await?;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//------------- 6.5

/// ACK (0x1):
/// When set, bit 0 indicates that this frame acknowledges receipt
/// and application of the peer's SETTINGS frame. When this bit is
/// set, the payload of the SETTINGS frame MUST be empty. Receipt of
/// a SETTINGS frame with the ACK flag set and a length field value
/// other than 0 MUST be treated as a connection error (Section 5.4.1)
/// of type FRAME_SIZE_ERROR.
pub async fn sends_settings_frame_with_ack_and_payload<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let frame = Frame::new(
        FrameType::Settings(SettingsFlags::Ack.into()),
        StreamId::CONNECTION,
    )
    .with_len(1);
    let frame_header = frame.into_piece(&mut conn.scratch)?;
    conn.send(frame_header).await?;
    // this may fail, we already broke the protocol
    _ = conn.send(b"\x00").await;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

/// SETTINGS frames always apply to a connection, never a single
/// stream. The stream identifier for a SETTINGS frame MUST be
/// zero (0x0). If an endpoint receives a SETTINGS frame whose
/// stream identifier field is anything other than 0x0, the
/// endpoint MUST respond with a connection error (Section 5.4.1)
/// of type PROTOCOL_ERROR.
pub async fn sends_settings_frame_with_non_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::Settings(Default::default()), StreamId(1)).with_len(6),
        SettingPairs(&[(SettingCode::MaxConcurrentStreams, 0x64)]),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// The SETTINGS frame affects connection state. A badly formed or
/// incomplete SETTINGS frame MUST be treated as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
///
/// A SETTINGS frame with a length other than a multiple of 6 octets
/// MUST be treated as a connection error (Section 5.4.1) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_settings_frame_with_invalid_length<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let frame = Frame::new(
        FrameType::Settings(Default::default()),
        StreamId::CONNECTION,
    )
    .with_len(3);
    let frame = frame.into_piece(&mut conn.scratch)?;
    conn.send(frame).await?;
    // this may fail, we already broke the protocol
    _ = conn.send(b"\x00\x03\x00").await;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}
