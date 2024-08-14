//! Section 4: HTTP Frames

use crate::{dummy_bytes, Conn, ErrorC};
use buffet::IntoHalves;
use enumflags2::BitFlags;
use loona_h2::{
    ContinuationFlags, EncodedFrameType, Frame, FrameType, HeadersFlags, PrioritySpec, StreamId,
};

//---- Section 4.1: Frame Format

/// Implementations MUST ignore and discard frames of unknown types.
pub async fn sends_frame_with_unknown_type<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
    conn.handshake().await?;

    conn.write_frame(
        FrameType::Unknown(EncodedFrameType {
            ty: 0xff,
            flags: 0x0,
        })
        .into_frame(StreamId(0)),
        dummy_bytes(8),
    )
    .await?;

    conn.verify_connection_still_alive().await?;

    Ok(())
}

/// Unused flags MUST be ignored on receipt and MUST be left
/// unset (0x00) when sending.
pub async fn sends_frame_with_unused_flags<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
    conn.handshake().await?;

    let data = b"sfunflgs";
    conn.write_frame(
        FrameType::Unknown(EncodedFrameType {
            ty: 6,         // type 6 = PING
            flags: 0b1110, // (don't set ACK bit)
        })
        .into_frame(StreamId::CONNECTION),
        data,
    )
    .await?;
    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}

/// Reserved: A reserved 1-bit field. The semantics of this bit are
/// undefined, and the bit MUST remain unset (0x00) when sending and
/// MUST be ignored when receiving.
pub async fn sends_frame_with_reserved_bit_set<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let data = b"sfresbit";
    conn.write_frame(
        Frame {
            frame_type: FrameType::Ping(Default::default()),
            reserved: 1,
            ..Default::default()
        },
        data,
    )
    .await?;
    conn.verify_ping_frame_with_ack(data).await?;

    Ok(())
}

//--- Section 4.2: Frame Size

// All implementations MUST be capable of receiving and minimally
// processing frames up to 2^14 octets in length, plus the 9-octet
// frame header (Section 4.1).
pub async fn data_frame_with_max_length<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    let data = dummy_bytes(conn.settings.max_frame_size as usize);
    conn.write_data(stream_id, true, data).await?;

    conn.verify_headers_frame(stream_id).await?;

    Ok(())
}

/// An endpoint MUST send an error code of FRAME_SIZE_ERROR if a frame
/// exceeds the size defined in SETTINGS_MAX_FRAME_SIZE, exceeds any
/// limit defined for the frame type, or is too small to contain mandatory frame
/// data
pub async fn frame_exceeding_max_size<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    // this might fail partway through with a broken pipe error, that's okay
    _ = conn
        .write_data(
            stream_id,
            true,
            dummy_bytes(conn.settings.max_frame_size as usize + 1),
        )
        .await;

    conn.verify_stream_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

/// A frame size error in a frame that could alter the state of
/// the entire connection MUST be treated as a connection error
/// (Section 5.4.1); this includes any frame carrying a field block
/// (Section 4.3) (that is, HEADERS, PUSH_PROMISE, and CONTINUATION),
/// a SETTINGS frame, and any frame with a stream identifier of 0.
pub async fn large_headers_frame_exceeding_max_size<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.extend(conn.dummy_headers(5));
    let block_fragment = conn.encode_headers(&headers)?;
    assert!(
        block_fragment.len() > conn.settings.max_frame_size as usize,
        "if this assertion fails the test is broken"
    );

    // this might fail partway through, since we're sending a frame that's too large
    // large.
    _ = conn
        .write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await;

    conn.verify_connection_error(ErrorC::FrameSizeError).await?;

    Ok(())
}

//---- Section 4.3: Header Compression and Decompression

/// A decoding error in a header block MUST be treated as a connection error
/// (Section 5.4.1) of type COMPRESSION_ERROR.
pub async fn invalid_header_block_fragment<IO: IntoHalves>(mut conn: Conn<IO>) -> eyre::Result<()> {
    conn.handshake().await?;

    // Literal Header Field with Incremental Indexing without
    // Length and String segment.
    conn.write_frame(
        FrameType::Headers(HeadersFlags::EndStream | HeadersFlags::EndHeaders)
            .into_frame(StreamId(1)),
        b"\x40",
    )
    .await?;

    conn.verify_connection_error(ErrorC::CompressionError)
        .await?;

    Ok(())
}

/// Each header block is processed as a discrete unit. Header blocks
/// MUST be transmitted as a contiguous sequence of frames, with no
/// interleaved frames of any other type or from any other stream.
pub async fn priority_frame_while_sending_headers<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let block_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, block_fragment)
        .await?;

    // this priority frame doesn't belong here, the peer should send
    // use a protocol error.
    conn.write_priority(
        stream_id,
        PrioritySpec {
            stream_dependency: StreamId(0),
            exclusive: false,
            weight: 255,
        },
    )
    .await?;

    let continuation_fragment = conn.encode_headers(&conn.dummy_headers(1))?;
    // this may fail (we broke the protocol)
    _ = conn
        .write_continuation(
            stream_id,
            ContinuationFlags::EndHeaders,
            continuation_fragment,
        )
        .await;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// Each header block is processed as a discrete unit. Header blocks
/// MUST be transmitted as a contiguous sequence of frames, with no
/// interleaved frames of any other type or from any other stream.
pub async fn headers_frame_to_another_stream<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let headers = conn.common_headers("POST");
    let block_fragment_1 = conn.encode_headers(&headers)?;
    conn.write_headers(stream_id, BitFlags::default(), block_fragment_1)
        .await?;

    // interleave a HEADERS frame for another stream
    let block_fragment_2 = conn.encode_headers(&headers)?;
    conn.write_headers(
        StreamId(stream_id.0 + 2),
        HeadersFlags::EndHeaders,
        block_fragment_2,
    )
    .await?;

    let dummy_headers = conn.dummy_headers(1);
    let continuation_fragment = conn.encode_headers(&dummy_headers)?;

    // this may fail (we broke the protocol)
    _ = conn
        .write_continuation(
            stream_id,
            ContinuationFlags::EndHeaders,
            continuation_fragment,
        )
        .await;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
