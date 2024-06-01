//! Section 4.3: Header Compression and Decompression

use enumflags2::BitFlags;
use fluke_buffet::IntoHalves;
use fluke_h2_parse::{ContinuationFlags, HeadersFlags, PrioritySpec, StreamId};

use crate::{Conn, ErrorC};

/// A decoding error in a header block MUST be treated as a connection error
/// (Section 5.4.1) of type COMPRESSION_ERROR.
pub async fn invalid_header_block_fragment<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    // Literal Header Field with Incremental Indexing without
    // Length and String segment.
    conn.send(b"\x00\x00\x01\x01\x05\x00\x00\x00\x01\x40")
        .await?;

    conn.verify_connection_error(ErrorC::CompressionError)
        .await?;

    Ok(())
}

/// Each header block is processed as a discrete unit. Header blocks
/// MUST be transmitted as a contiguous sequence of frames, with no
/// interleaved frames of any other type or from any other stream.
pub async fn priority_frame_while_sending_headers<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

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

/// Each header block is processed as a discrete unit. Header blocks
/// MUST be transmitted as a contiguous sequence of frames, with no
/// interleaved frames of any other type or from any other stream.
pub async fn headers_frame_to_another_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(stream_id, BitFlags::default(), block_fragment)
        .await?;

    // interleave a HEADERS frame for another stream
    let headers_fragment_2 = conn.encode_headers(&headers)?;
    conn.write_headers(
        StreamId(stream_id.0 + 2),
        HeadersFlags::EndHeaders,
        headers_fragment_2,
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
