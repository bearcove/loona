//! Section 6.1: DATA

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{
    Frame, FrameType, HeadersFlags, IntoPiece, PrioritySpec, SettingCode, SettingPairs,
    SettingsFlags, StreamId,
};

use crate::{dummy_bytes, Conn, ErrorC, FrameT};

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

//------------- 6.5.2

/// SETTINGS_ENABLE_PUSH (0x2):
/// The initial value is 1, which indicates that server push is
/// permitted. Any value other than 0 or 1 MUST be treated as a
/// connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_settings_enable_push_with_invalid_value<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        SettingPairs(&[(SettingCode::EnablePush, 2)]),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// SETTINGS_INITIAL_WINDOW_SIZE (0x4):
/// Values above the maximum flow-control window size of 2^31-1
/// MUST be treated as a connection error (Section 5.4.1) of
/// type FLOW_CONTROL_ERROR.
pub async fn sends_settings_initial_window_size_with_invalid_value<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        SettingPairs(&[(SettingCode::InitialWindowSize, 1 << 31)]),
    )
    .await?;

    conn.verify_connection_error(ErrorC::FlowControlError)
        .await?;

    Ok(())
}

/// SETTINGS_MAX_FRAME_SIZE (0x5):
/// The initial value is 2^14 (16,384) octets. The value advertised
/// by an endpoint MUST be between this initial value and the
/// maximum allowed frame size (2^24-1 or 16,777,215 octets),
/// inclusive. Values outside this range MUST be treated as a
/// connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_settings_max_frame_size_with_invalid_value_below_initial<
    IO: IntoHalves + 'static,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        SettingPairs(&[(SettingCode::MaxFrameSize, (1 << 14) - 1)]),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// SETTINGS_MAX_FRAME_SIZE (0x5):
/// The initial value is 2^14 (16,384) octets. The value advertised
/// by an endpoint MUST be between this initial value and the
/// maximum allowed frame size (2^24-1 or 16,777,215 octets),
/// inclusive. Values outside this range MUST be treated as a
/// connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_settings_max_frame_size_with_invalid_value_above_max<
    IO: IntoHalves + 'static,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        SettingPairs(&[(SettingCode::MaxFrameSize, 1 << 24)]),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// An endpoint that receives a SETTINGS frame with any unknown
/// or unsupported identifier MUST ignore that setting.
pub async fn sends_settings_frame_with_unknown_identifier<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        // settings payloads in http/2 are groups of 6 bytes,
        // composed of a (big-endian) 16-bit identifier and a 32-bit value
        // here we want to send a setting with an unknown identifier
        // 0xff, and value 0. here are its bytes:
        b"\x00\xff\x00\x00\x00\x00",
    )
    .await?;

    let data = [0; 8];
    conn.write_ping(false, data.to_vec()).await?;

    conn.verify_ping_frame_with_ack(&data).await?;

    Ok(())
}

//------------- 6.5.3

/// The values in the SETTINGS frame MUST be processed in the order
/// they appear, with no other frame processing between values.
pub async fn sends_multiple_values_of_settings_initial_window_size<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        ),
        SettingPairs(&[
            (SettingCode::InitialWindowSize, 100),
            (SettingCode::InitialWindowSize, 1),
        ]),
    )
    .await?;

    conn.verify_settings_frame_with_ack().await?;

    let mut headers = conn.common_headers();
    headers.insert(":method".into(), "POST".into());

    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    let (frame, _payload) = conn.wait_for_frame(FrameT::Data).await.unwrap();
    assert_eq!(frame.len, 1);

    Ok(())
}

/// Once all values have been processed, the recipient MUST
/// immediately emit a SETTINGS frame with the ACK flag set.
pub async fn sends_settings_frame_without_ack_flag<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        ),
        SettingPairs(&[(SettingCode::EnablePush, 0)]),
    )
    .await?;

    conn.verify_settings_frame_with_ack().await?;

    Ok(())
}

//----------------- 6.7

/// Receivers of a PING frame that does not include an ACK flag MUST
/// send a PING frame with the ACK flag set in response, with an
/// identical payload.
pub async fn sends_ping_frame<IO: IntoHalves + 'static>(mut conn: Conn<IO>) -> eyre::Result<()> {
    conn.handshake().await?;

    let data = b"h2spec\0\0";
    conn.write_ping(false, data.to_vec()).await?;

    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}

/// ACK (0x1):
/// When set, bit 0 indicates that this PING frame is a PING
/// response. An endpoint MUST set this flag in PING responses.
/// An endpoint MUST NOT respond to PING frames containing this
/// flag.
pub async fn sends_ping_frame_with_ack<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let unexpected_data = b"invalid\0";
    let expected_data = b"h2spec\0\0";
    conn.write_ping(true, unexpected_data.to_vec()).await?;
    conn.write_ping(false, expected_data.to_vec()).await?;

    conn.verify_ping_frame_with_ack(expected_data).await?;

    Ok(())
}

/// If a PING frame is received with a stream identifier field value
/// other than 0x0, the recipient MUST respond with a connection
/// error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_ping_frame_with_non_zero_stream_id<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::Ping(Default::default()), StreamId(1)),
        dummy_bytes(8),
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// Receipt of a PING frame with a length field value other than 8
/// MUST be treated as a connection error (Section 5.4.1) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_ping_frame_with_invalid_length<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let frame = Frame::new(FrameType::Ping(Default::default()), StreamId::CONNECTION).with_len(6);
    let frame_header = frame.into_piece(&mut conn.scratch)?;
    conn.send(frame_header).await?;
    conn.send(dummy_bytes(6)).await?;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}
