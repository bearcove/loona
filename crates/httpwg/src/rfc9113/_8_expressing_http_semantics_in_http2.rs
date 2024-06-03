//! Section 8: Expressing HTTP Semantics in HTTP/2

use fluke_buffet::IntoHalves;
use fluke_h2_parse::{HeadersFlags, StreamId};

use crate::{Conn, ErrorC, Headers};

//---- Section 8.1: HTTP Message Framing

// An endpoint that receives a HEADERS frame without the
// END_STREAM flag set after receiving a final (non-informational)
// status code MUST treat the corresponding request or response
// as malformed (Section 8.1.2.6).
pub async fn sends_second_headers_frame_without_end_stream<IO: IntoHalves>(
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
pub async fn sends_headers_frame_with_uppercase_field_name<IO: IntoHalves>(
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
pub async fn sends_headers_frame_with_space_in_field_name<IO: IntoHalves>(
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
pub async fn sends_headers_frame_with_non_visible_ascii<IO: IntoHalves>(
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
pub async fn sends_headers_frame_with_del_character<IO: IntoHalves>(
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
pub async fn sends_headers_frame_with_non_ascii_character<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("inválid".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// With the exception of pseudo-header fields (Section 8.3), which have a name
/// that starts with a single colon, field names MUST NOT include a colon (ASCII
/// COLON, 0x3a).
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_colon_in_field_name<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid:field".into(), "oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field value MUST NOT contain the zero value (ASCII NUL, 0x00), line feed
/// (ASCII LF, 0x0a), or carriage return (ASCII CR, 0x0d) at any position.
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_lf_in_field_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid-value".into(), "oh\nno".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field value MUST NOT contain the zero value (ASCII NUL, 0x00), line feed
/// (ASCII LF, 0x0a), or carriage return (ASCII CR, 0x0d) at any position.
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_cr_in_field_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid-value".into(), "oh\rno".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field value MUST NOT contain the zero value (ASCII NUL, 0x00), line feed
/// (ASCII LF, 0x0a), or carriage return (ASCII CR, 0x0d) at any position.
///
/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_nul_in_field_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid-value".into(), "oh\0no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field value MUST NOT start or end with an ASCII whitespace character
/// (ASCII SP or HTAB, 0x20 or 0x09).

/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_leading_space_in_field_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid-value".into(), " oh no".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// A field value MUST NOT start or end with an ASCII whitespace character
/// (ASCII SP or HTAB, 0x20 or 0x09).

/// When a request message violates one of these requirements, an implementation
/// SHOULD generate a 400 (Bad Request) status code (see Section 15.5.1 of
/// [HTTP]), unless a more suitable status code is defined or the status code
/// cannot be sent (e.g., because the error occurs in a trailer field).
pub async fn sends_headers_frame_with_trailing_tab_in_field_value<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("invalid-value".into(), "oh no\t".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

//---- Section 8.2.2: Connection-Specific Header Fields

/// HTTP/2 does not use the Connection header field (Section 7.6.1 of [HTTP]) to
/// indicate connection-specific header fields; in this protocol,
/// connection-specific metadata is conveyed by other means. An endpoint MUST
/// NOT generate an HTTP/2 message containing connection-specific header fields.
/// This includes the Connection header field and those listed as having
/// connection-specific semantics in Section 7.6.1 of [HTTP] (that is,
/// Proxy-Connection, Keep-Alive, Transfer-Encoding, and Upgrade). Any message
/// containing connection-specific header fields MUST be treated as malformed
/// (Section 8.1.1).
pub async fn sends_headers_frame_with_connection_header<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("connection".into(), "keep-alive".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// HTTP/2 does not use the Connection header field (Section 7.6.1 of [HTTP]) to
/// indicate connection-specific header fields; in this protocol,
/// connection-specific metadata is conveyed by other means. An endpoint MUST
/// NOT generate an HTTP/2 message containing connection-specific header fields.
///
/// This includes the Connection header field and those listed as having
/// connection-specific semantics in Section 7.6.1 of [HTTP] (that is,
/// Proxy-Connection, Keep-Alive, Transfer-Encoding, and Upgrade). Any message
/// containing connection-specific header fields MUST be treated as malformed
/// (Section 8.1.1).
pub async fn sends_headers_frame_with_proxy_connection_header<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("proxy-connection".into(), "keep-alive".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// HTTP/2 does not use the Connection header field (Section 7.6.1 of [HTTP]) to
/// indicate connection-specific header fields; in this protocol,
/// connection-specific metadata is conveyed by other means. An endpoint MUST
/// NOT generate an HTTP/2 message containing connection-specific header fields.
///
/// This includes the Connection header field and those listed as having
/// connection-specific semantics in Section 7.6.1 of [HTTP] (that is,
/// Proxy-Connection, Keep-Alive, Transfer-Encoding, and Upgrade). Any message
/// containing connection-specific header fields MUST be treated as malformed
/// (Section 8.1.1).
pub async fn sends_headers_frame_with_keep_alive_header<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("keep-alive".into(), "timeout=5".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// HTTP/2 does not use the Connection header field (Section 7.6.1 of [HTTP]) to
/// indicate connection-specific header fields; in this protocol,
/// connection-specific metadata is conveyed by other means. An endpoint MUST
/// NOT generate an HTTP/2 message containing connection-specific header fields.
///
/// This includes the Connection header field and those listed as having
/// connection-specific semantics in Section 7.6.1 of [HTTP] (that is,
/// Proxy-Connection, Keep-Alive, Transfer-Encoding, and Upgrade). Any message
/// containing connection-specific header fields MUST be treated as malformed
/// (Section 8.1.1).
pub async fn sends_headers_frame_with_transfer_encoding_header<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("transfer-encoding".into(), "chunked".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// HTTP/2 does not use the Connection header field (Section 7.6.1 of [HTTP]) to
/// indicate connection-specific header fields; in this protocol,
/// connection-specific metadata is conveyed by other means. An endpoint MUST
/// NOT generate an HTTP/2 message containing connection-specific header fields.
///
/// This includes the Connection header field and those listed as having
/// connection-specific semantics in Section 7.6.1 of [HTTP] (that is,
/// Proxy-Connection, Keep-Alive, Transfer-Encoding, and Upgrade). Any message
/// containing connection-specific header fields MUST be treated as malformed
/// (Section 8.1.1).
pub async fn sends_headers_frame_with_upgrade_header<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("upgrade".into(), "h2c".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}

/// The only exception to this is the TE header field, which MAY be present in
/// an HTTP/2 request; when it is, it MUST NOT contain any value other than
/// "trailers".
pub async fn sends_headers_frame_with_te_trailers<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("te".into(), "trailers".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 200)
        .await?;

    Ok(())
}

/// The only exception to this is the TE header field, which MAY be present in
/// an HTTP/2 request; when it is, it MUST NOT contain any value other than
/// "trailers".
pub async fn sends_headers_frame_with_te_not_trailers<IO: IntoHalves>(
    mut conn: Conn<IO>,
) -> eyre::Result<()> {
    conn.handshake().await?;

    let mut headers = conn.common_headers("POST");
    headers.insert("te".into(), "not-trailers".into());
    conn.send_req_and_expect_status(StreamId(1), &headers, 400)
        .await?;

    Ok(())
}
