//! Section 5.1.2: Stream Concurrency

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{Setting, StreamId};

use crate::{Conn, ErrorC};

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
