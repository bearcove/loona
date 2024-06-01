//! Section 5.1.1: Stream Identifiers

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{ContinuationFlags, HeadersFlags, StreamId};

use crate::{Conn, ErrorC};

/// An endpoint that receives an unexpected stream identifier
/// MUST respond with a connection error (Section 5.4.1) of
/// type PROTOCOL_ERROR.
pub async fn sends_even_numbered_stream_identifier<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        StreamId(2),
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

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

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers(
        StreamId(5),
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment.clone(),
    )
    .await?;

    conn.write_headers(
        StreamId(3),
        HeadersFlags::EndStream | HeadersFlags::EndHeaders,
        block_fragment,
    )
    .await?;

    conn.verify_connection_error(ErrorC::ProtocolError).await?;

    Ok(())
}
