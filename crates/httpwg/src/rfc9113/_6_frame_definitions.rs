//! Section 6: Frame Definitions

use enumflags2::BitFlags;
use fluke_buffet::{IntoHalves, Piece};
use fluke_h2_parse::{
    ContinuationFlags, Frame, FrameType, GoAway, HeadersFlags, IntoPiece, KnownErrorCode,
    PrioritySpec, Setting, SettingPairs, SettingsFlags, StreamId,
};

use crate::{dummy_bytes, Conn, ErrorC, FrameT};

//---- Section 6.1: DATA

/// DATA frames MUST be associated with a stream. If a DATA frame is
/// received whose stream identifier field is 0x0, the recipient
/// MUST respond with a connection error (Section 5.4.1) of type
/// PROTOCOL_ERROR.
pub async fn sends_data_frame_with_zero_stream_id<IO: IntoHalves>(
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
pub async fn sends_data_frame_on_invalid_stream_state<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
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
pub async fn sends_data_frame_with_invalid_pad_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.append("content-length", "4");
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

//---- Section 6.2: HEADERS

/// HEADERS frames MUST be associated with a stream. If a HEADERS
/// frame is received whose stream identifier field is 0x0, the
/// recipient MUST respond with a connection error (Section 5.4.1)
/// of type PROTOCOL_ERROR.
pub async fn sends_headers_frame_with_zero_stream_id<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;

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
pub async fn sends_headers_frame_with_invalid_pad_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;

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

//---- Section 6.3: PRIORITY

/// The PRIORITY frame always identifies a stream. If a PRIORITY
/// frame is received with a stream identifier of 0x0, the recipient
/// MUST respond with a connection error (Section 5.4.1) of type
/// PROTOCOL_ERROR.
pub async fn sends_priority_frame_with_zero_stream_id<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_priority(
        StreamId::CONNECTION,
        PrioritySpec {
            stream_dependency: StreamId::CONNECTION,
            exclusive: false,
            weight: 255,
        },
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A PRIORITY frame with a length other than 5 octets MUST be
/// treated as a stream error (Section 5.4.2) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_priority_frame_with_invalid_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;

    conn.write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    conn.write_frame(
        Frame::new(FrameType::Priority, stream_id),
        b"\x80\x00\x00\x01",
    )
    .await?;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//---- Section 6.4: RST_STREAM

/// RST_STREAM frames MUST be associated with a stream. If a
/// RST_STREAM frame is received with a stream identifier of 0x0,
/// the recipient MUST treat this as a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_rst_stream_frame_with_zero_stream_id<IO: IntoHalves>(
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
pub async fn sends_rst_stream_frame_on_idle_stream<IO: IntoHalves>(
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
pub async fn sends_rst_stream_frame_with_invalid_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
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

//---- Section 6.5: SETTINGS
//---- Section 6.5.1: SETTINGS Format

/// ACK (0x1):
/// When set, bit 0 indicates that this frame acknowledges receipt
/// and application of the peer's SETTINGS frame. When this bit is
/// set, the payload of the SETTINGS frame MUST be empty. Receipt of
/// a SETTINGS frame with the ACK flag set and a length field value
/// other than 0 MUST be treated as a connection error (Section 5.4.1)
/// of type FRAME_SIZE_ERROR.
pub async fn sends_settings_frame_with_ack_and_payload<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(SettingsFlags::Ack.into()),
            StreamId::CONNECTION,
        ),
        b"\x00",
    )
    .await?;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

/// SETTINGS frames always apply to a connection, never a single
/// stream. The stream identifier for a SETTINGS frame MUST be
/// zero (0x0). If an endpoint receives a SETTINGS frame whose
/// stream identifier field is anything other than 0x0, the
/// endpoint MUST respond with a connection error (Section 5.4.1)
/// of type PROTOCOL_ERROR.
pub async fn sends_settings_frame_with_non_zero_stream_id<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::Settings(Default::default()), StreamId(1)).with_len(6),
        SettingPairs(&[(Setting::MaxConcurrentStreams, 0x64)]),
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
pub async fn sends_settings_frame_with_invalid_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        ),
        b"\x00\x03\x00",
    )
    .await?;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//---- Section 6.5.2: Defined SETTINGS Parameters

/// SETTINGS_ENABLE_PUSH (0x2):
/// The initial value is 1, which indicates that server push is
/// permitted. Any value other than 0 or 1 MUST be treated as a
/// connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_settings_enable_push_with_invalid_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_settings(&[(Setting::EnablePush, 2)]).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// SETTINGS_INITIAL_WINDOW_SIZE (0x4):
/// Values above the maximum flow-control window size of 2^31-1
/// MUST be treated as a connection error (Section 5.4.1) of
/// type FLOW_CONTROL_ERROR.
pub async fn sends_settings_initial_window_size_with_invalid_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_settings(&[(Setting::InitialWindowSize, 1 << 31)])
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
pub async fn sends_settings_max_frame_size_with_invalid_value_below_initial<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_settings(&[(Setting::MaxFrameSize, (1 << 14) - 1)])
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
pub async fn sends_settings_max_frame_size_with_invalid_value_above_max<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_settings(&[(Setting::MaxFrameSize, 1 << 24)])
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// An endpoint that receives a SETTINGS frame with any unknown
/// or unsupported identifier MUST ignore that setting.
pub async fn sends_settings_frame_with_unknown_identifier<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(6),
        // identifier 0xff, value 0x00
        b"\x00\xff\x00\x00\x00\x00",
    )
    .await?;

    conn.verify_connection_still_alive().await?;

    Ok(())
}

//---- Section 6.5.3: Settings Synchronization

/// The values in the SETTINGS frame MUST be processed in the order
/// they appear, with no other frame processing between values.
pub async fn sends_multiple_values_of_settings_initial_window_size<IO: IntoHalves>(
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
            (Setting::InitialWindowSize, 100),
            (Setting::InitialWindowSize, 1),
        ]),
    )
    .await?;

    conn.verify_settings_frame_with_ack().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
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
pub async fn sends_settings_frame_without_ack_flag<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        ),
        SettingPairs(&[(Setting::EnablePush, 0)]),
    )
    .await?;

    conn.verify_settings_frame_with_ack().await?;

    Ok(())
}

// (Note: Section 6.6 is skipped: push promise is discouraged nowadays)

//---- Section 6.7: PING

/// Receivers of a PING frame that does not include an ACK flag MUST
/// send a PING frame with the ACK flag set in response, with an
/// identical payload.
pub async fn sends_ping_frame<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
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
pub async fn sends_ping_frame_with_ack<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
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
pub async fn sends_ping_frame_with_non_zero_stream_id<IO: IntoHalves>(
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
pub async fn sends_ping_frame_with_invalid_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::Ping(Default::default()), StreamId::CONNECTION),
        dummy_bytes(6),
    )
    .await?;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//---- Section 6.8: GOAWAY

/// An endpoint MUST treat a GOAWAY frame with a stream identifier
/// other than 0x0 as a connection error (Section 5.4.1) of type
/// PROTOCOL_ERROR.
pub async fn sends_goaway_frame_with_non_zero_stream_id<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::GoAway, StreamId(1)),
        GoAway {
            additional_debug_data: Piece::empty(),
            error_code: KnownErrorCode::NoError.into(),
            last_stream_id: StreamId(0),
        },
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

//---- Section 6.9: WINDOW_UPDATE

/// A receiver MUST treat the receipt of a WINDOW_UPDATE frame with
/// a flow-control window increment of 0 as a stream error
/// (Section 5.4.2) of type PROTOCOL_ERROR; errors on the connection
/// flow-control window MUST be treated as a connection error
/// (Section 5.4.1).
pub async fn sends_window_update_frame_with_zero_increment<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_window_update(StreamId::CONNECTION, 0).await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A receiver MUST treat the receipt of a WINDOW_UPDATE frame with
/// a flow-control window increment of 0 as a stream error
/// (Section 5.4.2) of type PROTOCOL_ERROR; errors on the connection
/// flow-control window MUST be treated as a connection error
/// (Section 5.4.1).
pub async fn sends_window_update_frame_with_zero_increment_on_stream<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.append(":method", "POST");

    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    conn.write_window_update(stream_id, 0).await?;

    conn.verify_stream_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A WINDOW_UPDATE frame with a length other than 4 octets MUST
/// be treated as a connection error (Section 5.4.1) of type
/// FRAME_SIZE_ERROR.
pub async fn sends_window_update_frame_with_invalid_length<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        Frame::new(FrameType::WindowUpdate, StreamId::CONNECTION),
        b"\x00\x00\x01",
    )
    .await?;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//---- Section 6.9.1: The Flow-Control Window

/// The sender MUST NOT send a flow-controlled frame with a length
/// that exceeds the space available in either of the flow-control
/// windows advertised by the receiver.
pub async fn sends_settings_frame_to_set_initial_window_size_to_1_and_sends_headers_frame<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    conn.write_and_ack_settings(&[(Setting::InitialWindowSize, 1)])
        .await?;

    conn.encode_and_write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        &conn.common_headers("POST"),
    )
    .await?;

    let (frame, _payload) = conn.wait_for_frame(FrameT::Data).await.unwrap();
    assert_eq!(frame.len, 1);

    Ok(())
}

/// A sender MUST NOT allow a flow-control window to exceed 2^31-1
/// octets. If a sender receives a WINDOW_UPDATE that causes a
/// flow-control window to exceed this maximum, it MUST terminate
/// either the stream or the connection, as appropriate.
/// For streams, the sender sends a RST_STREAM with an error code
/// of FLOW_CONTROL_ERROR; for the connection, a GOAWAY frame with
/// an error code of FLOW_CONTROL_ERROR is sent.
pub async fn sends_multiple_window_update_frames_increasing_flow_control_window_above_max<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_window_update(StreamId::CONNECTION, (1 << 31) - 1)
        .await?;
    conn.write_window_update(StreamId::CONNECTION, (1 << 31) - 1)
        .await?;

    conn.verify_connection_error(ErrorC::FlowControlError)
        .await?;

    Ok(())
}

/// A sender MUST NOT allow a flow-control window to exceed 2^31-1
/// octets. If a sender receives a WINDOW_UPDATE that causes a
/// flow-control window to exceed this maximum, it MUST terminate
/// either the stream or the connection, as appropriate.
/// For streams, the sender sends a RST_STREAM with an error code
/// of FLOW_CONTROL_ERROR; for the connection, a GOAWAY frame with
/// an error code of FLOW_CONTROL_ERROR is sent.
pub async fn sends_multiple_window_update_frames_increasing_flow_control_window_above_max_on_stream<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    conn.encode_and_write_headers(
        stream_id,
        HeadersFlags::EndHeaders,
        &conn.common_headers("POST"),
    )
    .await?;

    conn.write_window_update(stream_id, (1 << 31) - 1).await?;
    // that write might fail: the window may already have exceeded the max
    _ = conn.write_window_update(stream_id, (1 << 31) - 1).await;

    conn.verify_stream_error(ErrorC::FlowControlError).await?;

    Ok(())
}

//---- Section 6.9.2: Initial Flow-Control Window Size

/// When the value of SETTINGS_INITIAL_WINDOW_SIZE changes,
/// a receiver MUST adjust the size of all stream flow-control
/// windows that it maintains by the difference between the new
/// value and the old value.
pub async fn changes_settings_initial_window_size_after_sending_headers_frame<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    // prevent the peer from sending us data frames
    conn.write_and_ack_settings(&[(Setting::InitialWindowSize, 0)])
        .await?;

    // make a request
    conn.encode_and_write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        &conn.common_headers("POST"),
    )
    .await?;

    // allow the peer to send us one byte
    conn.write_settings(&[(Setting::InitialWindowSize, 1)])
        .await?;

    let (frame, _payload) = conn.wait_for_frame(FrameT::Data).await.unwrap();
    assert_eq!(frame.len, 1);

    Ok(())
}

/// A sender MUST track the negative flow-control window and
/// MUST NOT send new flow-controlled frames until it receives
/// WINDOW_UPDATE frames that cause the flow-control window to
/// become positive.
pub async fn sends_settings_frame_for_window_size_to_be_negative<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    // note: this test assumes the response body is 5 bytes or above

    // make window size 3 & send request
    conn.write_and_ack_settings(&[(Setting::InitialWindowSize, 3)])
        .await?;
    conn.send_empty_post_to_root(stream_id).await?;

    // wait for peer to send us all 3 bytes it can
    let (_, payload) = conn.wait_for_frame(FrameT::Data).await.unwrap();
    assert_eq!(payload.len(), 3);

    // window size is 0, if we set SETTINGS_INITIAL_WINDOW_SIZE to 2
    // (from 3 before), it should go to -1
    conn.write_and_ack_settings(&[(Setting::InitialWindowSize, 2)])
        .await?;

    // bring it back up to 1
    conn.write_window_update(stream_id, 2).await?;

    // we should get exactly 1 byte
    let (frame, _payload) = conn.wait_for_frame(FrameT::Data).await.unwrap();
    assert_eq!(frame.len, 1);

    Ok(())
}

/// An endpoint MUST treat a change to SETTINGS_INITIAL_WINDOW_SIZE
/// that causes any flow-control window to exceed the maximum size
/// as a connection error (Section 5.4.1) of type FLOW_CONTROL_ERROR.
pub async fn sends_settings_initial_window_size_with_exceeded_max_window_size_value<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_settings(&[(Setting::InitialWindowSize, 1 << 31)])
        .await?;

    conn.verify_connection_error(ErrorC::FlowControlError)
        .await?;

    Ok(())
}

// Note: 6.9.3 (reducing the stream window size) is really tricky to test.

//---- Section 6.10: CONTINUATION

/// The CONTINUATION frame (type=0x9) is used to continue a sequence
/// of header block fragments (Section 4.3). Any number of
/// CONTINUATION frames can be sent, as long as the preceding frame
/// is on the same stream and is a HEADERS, PUSH_PROMISE,
/// or CONTINUATION frame without the END_HEADERS flag set.
pub async fn sends_multiple_continuation_frames_preceded_by_headers_frame<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers("POST");
    let block_fragment = conn.encode_headers(&headers)?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    let dummy_headers = conn.dummy_headers(1);
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, BitFlags::empty(), block_fragment)
        .await?;
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_headers_frame(stream_id).await?;

    Ok(())
}

/// END_HEADERS (0x4):
/// If the END_HEADERS bit is not set, this frame MUST be followed
/// by another CONTINUATION frame. A receiver MUST treat the receipt
/// of any other type of frame or a frame on a different stream as
/// a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_continuation_frame_followed_by_non_continuation_frame<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    let dummy_headers = conn.dummy_headers(1);
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, BitFlags::empty(), block_fragment)
        .await?;
    conn.write_data(stream_id, true, b"test").await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// CONTINUATION frames MUST be associated with a stream. If a
/// CONTINUATION frame is received whose stream identifier field is
/// 0x0, the recipient MUST respond with a connection error
/// (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_continuation_frame_with_zero_stream_id<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    let block_fragment = conn.encode_headers(&conn.dummy_headers(1))?;
    conn.write_continuation(
        StreamId::CONNECTION,
        ContinuationFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A CONTINUATION frame MUST be preceded by a HEADERS, PUSH_PROMISE
/// or CONTINUATION frame without the END_HEADERS flag set.
/// A recipient that observes violation of this rule MUST respond
/// with a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_continuation_frame_preceded_by_headers_frame_with_end_headers_flag<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(
        stream_id,
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    let dummy_headers = conn.dummy_headers(1);
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A CONTINUATION frame MUST be preceded by a HEADERS, PUSH_PROMISE
/// or CONTINUATION frame without the END_HEADERS flag set.
/// A recipient that observes violation of this rule MUST respond
/// with a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_continuation_frame_preceded_by_continuation_frame_with_end_headers_flag<
    IO: IntoHalves,
>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;

    let dummy_headers = conn.dummy_headers(1);
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;
    let block_fragment = conn.encode_headers(&dummy_headers)?;
    conn.write_continuation(stream_id, ContinuationFlags::EndHeaders, block_fragment)
        .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A CONTINUATION frame MUST be preceded by a HEADERS, PUSH_PROMISE
/// or CONTINUATION frame without the END_HEADERS flag set.
/// A recipient that observes violation of this rule MUST respond
/// with a connection error (Section 5.4.1) of type PROTOCOL_ERROR.
pub async fn sends_continuation_frame_preceded_by_data_frame<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndStream, block_fragment)
        .await?;
    conn.write_data(stream_id, true, b"test").await?;

    let block_fragment = conn.encode_headers(&conn.dummy_headers(1))?;
    // this may fail with broken pipe, the server may close the connection
    // in the middle of the write
    _ = conn
        .write_continuation(stream_id, BitFlags::empty(), block_fragment)
        .await;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
