//! Section 5.1.2: Stream Concurrency

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, Setting, SettingPairs, StreamId};

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
    conn.write_setting_pairs(SettingPairs(&[(Setting::InitialWindowSize, 0)]))
        .await?;

    let headers = conn.common_headers();
    let block_fragment = conn.encode_headers(&headers)?;

    let mut stream_id = 1;
    for _ in 0..=max_streams {
        conn.write_headers(
            StreamId(stream_id),
            HeadersFlags::EndStream | HeadersFlags::EndHeaders,
            block_fragment.clone(),
        )
        .await?;
        stream_id += 2;
    }

    conn.verify_stream_error(ErrorC::ProtocolError | ErrorC::RefusedStream)
        .await?;

    Ok(())
}
