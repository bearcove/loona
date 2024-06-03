//! Section 8: Expressing HTTP Semantics in HTTP/2

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, StreamId};

use crate::{Conn, ErrorC, Headers};

//---- Section 8.1: HTTP Message Framing

// An endpoint that receives a HEADERS frame without the
// END_STREAM flag set after receiving a final (non-informational)
// status code MUST treat the corresponding request or response
// as malformed (Section 8.1.2.6).
pub async fn sends_second_headers_frame_without_end_stream<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    let stream_id = StreamId(1);
    conn.handshake().await?;

    let headers_fragment = conn.encode_headers(&conn.common_headers("POST"))?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, headers_fragment)
        .await?;
    conn.write_data(stream_id, false, b"test").await?;

    let mut trailers = Headers::new();
    trailers.insert("x-test".into(), "ok".into());
    let trailers_fragment = conn.encode_headers(&trailers)?;
    conn.write_headers(stream_id, HeadersFlags::EndHeaders, trailers_fragment)
        .await?;

    conn.verify_stream_error(ErrorC::ProtocolError).await?;

    Ok(())
}

//--- Section 8.2.1: Field Validity

/// A field name MUST NOT contain characters in the ranges 0x00-0x20, 0x41-0x5a,
/// or 0x7f-0xff (all ranges inclusive). This specifically excludes all
/// non-visible ASCII characters, ASCII SP (0x20), and uppercase characters ('A'
/// to 'Z', ASCII 0x41 to 0x5a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_uppercase_field_name<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("UPPERCASE".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field name MUST NOT contain characters in the ranges 0x00-0x20, 0x41-0x5a,
/// or 0x7f-0xff (all ranges inclusive). This specifically excludes all
/// non-visible ASCII characters, ASCII SP (0x20), and uppercase characters ('A'
/// to 'Z', ASCII 0x41 to 0x5a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_space_in_field_name<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("space force".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field name MUST NOT contain characters in the ranges 0x00-0x20, 0x41-0x5a,
/// or 0x7f-0xff (all ranges inclusive). This specifically excludes all
/// non-visible ASCII characters, ASCII SP (0x20), and uppercase characters ('A'
/// to 'Z', ASCII 0x41 to 0x5a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_non_visible_ascii<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("\x01invalid".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field name MUST NOT contain characters in the ranges 0x00-0x20, 0x41-0x5a,
/// or 0x7f-0xff (all ranges inclusive). This specifically excludes all
/// non-visible ASCII characters, ASCII SP (0x20), and uppercase characters ('A'
/// to 'Z', ASCII 0x41 to 0x5a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_del_character<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("\x7Finvalid".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field name MUST NOT contain characters in the ranges 0x00-0x20, 0x41-0x5a,
/// or 0x7f-0xff (all ranges inclusive). This specifically excludes all
/// non-visible ASCII characters, ASCII SP (0x20), and uppercase characters ('A'
/// to 'Z', ASCII 0x41 to 0x5a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_non_ascii_character<IO: IntoHalves + 'static>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("inválid".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}
