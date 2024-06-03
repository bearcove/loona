//! Section 5.3: Stream Dependencies

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, PrioritySpec, StreamId};

use crate::{Conn, ErrorC};

/// A stream cannot depend on itself. An endpoint MUST treat this
/// as a stream error (Section 5.4.2) of type PROTOCOL_ERROR.
pub async fn headers_frame_depends_on_itself<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    let headers = conn.common_headers("POST");
    let block_fragment = conn.encode_headers(&headers)?;

    conn.write_headers_with_priority(
        stream_id,
        HeadersFlags::EndHeaders | HeadersFlags::EndStream,
        PrioritySpec {
            stream_dependency: stream_id,
            exclusive: false,
            weight: 255,
        },
        block_fragment,
    )
    .await?;

    conn.verify_stream_error(ErrorC::ProtocolError).await?;

    Ok(())
}

/// A stream cannot depend on itself. An endpoint MUST treat this
/// as a stream error (Section 5.4.2) of type PROTOCOL_ERROR.
pub async fn priority_frame_depends_on_itself<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);

    conn.handshake().await?;

    conn.write_priority(
        stream_id,
        PrioritySpec {
            stream_dependency: stream_id,
            exclusive: false,
            weight: 255,
        },
    )
    .await?;

    conn.verify_stream_error(ErrorC::ProtocolError).await?;

    Ok(())
}
