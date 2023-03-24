use std::{borrow::Cow, rc::Rc};

use hring_buffet::{Piece, PieceStr};
use http::{
    header::{self, HeaderName},
    uri::{Authority, PathAndQuery, Scheme},
    Version,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::{ExpectResponseHeaders, Headers, Method, Request, Responder, ServerDriver};

use super::{
    body::{H2Body, H2BodyItem, PieceOrTrailers},
    encode::{EncoderState, H2ConnEvent, H2Encoder},
    parse::{KnownErrorCode, StreamId},
    send_goaway, ConnState, HeadersData, StreamRxStage, StreamState,
};

pub(crate) async fn end_headers(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    stream_id: StreamId,
    state: &mut ConnState,
    driver: &Rc<impl ServerDriver + 'static>,
    hpack_dec: &mut hring_hpack::Decoder<'_>,
) {
    let stream_state = state
        .streams
        .get_mut(&stream_id)
        // FIXME: don't panic
        .expect("stream state must exist");

    if let Err(err) = end_headers_inner(ev_tx, stream_id, stream_state, driver, hpack_dec).await {
        send_goaway(
            ev_tx,
            state,
            err,
            // FIXME: this isn't _always_ CompressionError. also
            // not all errors are GOAWAY material, some are just RST
            KnownErrorCode::CompressionError,
        )
        .await;
    }
}

async fn end_headers_inner(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    stream_id: StreamId,
    stream_state: &mut StreamState,
    driver: &Rc<impl ServerDriver + 'static>,
    hpack_dec: &mut hring_hpack::Decoder<'_>,
) -> eyre::Result<()> {
    let mut method: Option<Method> = None;
    let mut scheme: Option<Scheme> = None;
    let mut path: Option<PieceStr> = None;
    let mut authority: Option<Authority> = None;

    let mut headers = Headers::default();

    let mut decode_data =
        |headers_or_trailers: HeadersOrTrailers, data: &HeadersData| -> eyre::Result<()> {
            // TODO: find a way to propagate errors from here - probably will have to change
            // the function signature in hring-hpack.
            let on_header_pair = |key: Cow<[u8]>, value: Cow<[u8]>| {
                debug!(
                    "{headers_or_trailers:?} | {}: {}",
                    std::str::from_utf8(&key).unwrap_or("<non-utf8-key>"), // TODO: does this hurt performance when debug logging is disabled?
                    std::str::from_utf8(&value).unwrap_or("<non-utf8-value>"),
                );

                if &key[..1] == b":" {
                    if matches!(headers_or_trailers, HeadersOrTrailers::Trailers) {
                        // TODO: proper error handling
                        panic!("trailers cannot contain pseudo-headers");
                    }

                    // it's a pseudo-header!
                    // TODO: reject headers that occur after pseudo-headers
                    match &key[1..] {
                        b"method" => {
                            // TODO: error handling
                            let value: PieceStr = Piece::from(value.to_vec()).to_str().unwrap();
                            if method.replace(Method::try_from(value).unwrap()).is_some() {
                                unreachable!(); // No duplicate allowed.
                            }
                        }
                        b"scheme" => {
                            // TODO: error handling
                            let value: PieceStr = Piece::from(value.to_vec()).to_str().unwrap();
                            if scheme.replace(value.parse().unwrap()).is_some() {
                                unreachable!(); // No duplicate allowed.
                            }
                        }
                        b"path" => {
                            // TODO: error handling
                            let value: PieceStr = Piece::from(value.to_vec()).to_str().unwrap();
                            if value.len() == 0 || path.replace(value).is_some() {
                                unreachable!(); // No empty path nor duplicate allowed.
                            }
                        }
                        b"authority" => {
                            // TODO: error handling
                            let value: PieceStr = Piece::from(value.to_vec()).to_str().unwrap();
                            if authority.replace(value.parse().unwrap()).is_some() {
                                unreachable!(); // No duplicate allowed. (h2spec doesn't seem to test for
                                                // this case but rejecting for duplicates seems
                                                // reasonable.)
                            }
                        }
                        _ => {
                            debug!("ignoring pseudo-header");
                        }
                    }
                } else {
                    // TODO: what do we do in case of malformed header names?
                    // ignore it? return a 400?
                    let name = HeaderName::from_bytes(&key[..]).expect("malformed header name");
                    let value: Piece = value.to_vec().into();
                    headers.append(name, value);
                }
            };

            match &data.fragments[..] {
                [] => unreachable!("must have at least one fragment"),
                [payload] => {
                    // TODO: propagate errors here instead of panicking
                    hpack_dec
                        .decode_with_cb(&payload[..], on_header_pair)
                        .map_err(|e| eyre::eyre!("hpack error: {e:?}"))?;
                }
                _ => {
                    let total_len = data.fragments.iter().map(|f| f.len()).sum();
                    // this is a slow path, let's do a little heap allocation. we could
                    // be using `RollMut` for this, but it would probably need to resize
                    // a bunch
                    let mut payload = Vec::with_capacity(total_len);
                    for frag in &data.fragments {
                        payload.extend_from_slice(&frag[..]);
                    }
                    // TODO: propagate errors here instead of panicking
                    hpack_dec
                        .decode_with_cb(&payload[..], on_header_pair)
                        .map_err(|e| eyre::eyre!("hpack error: {e:?}"))?;
                }
            };

            Ok(())
        };

    // FIXME: don't panic on unexpected states here
    let mut rx_stage = StreamRxStage::Done;
    std::mem::swap(&mut rx_stage, &mut stream_state.rx_stage);

    match rx_stage {
        StreamRxStage::Headers(data) => {
            decode_data(HeadersOrTrailers::Headers, &data)?;

            // TODO: cf. https://httpwg.org/specs/rfc9113.html#HttpRequest
            // A server SHOULD treat a request as malformed if it contains a Host header
            // field that identifies an entity that differs from the entity in the
            // ":authority" pseudo-header field.

            // TODO: proper error handling (return 400)
            let method = method.unwrap();
            let scheme = scheme.unwrap();

            let path = path.unwrap();
            let path_and_query: PathAndQuery = path.parse().unwrap();

            let authority = match authority {
                Some(authority) => Some(authority),
                None => headers
                    .get(header::HOST)
                    .map(|host| host.as_str().unwrap().parse().unwrap()),
            };

            let mut uri_parts: http::uri::Parts = Default::default();
            uri_parts.scheme = Some(scheme);
            uri_parts.authority = authority;
            uri_parts.path_and_query = Some(path_and_query);

            let uri = http::uri::Uri::from_parts(uri_parts).unwrap();

            let req = Request {
                method,
                uri,
                version: Version::HTTP_2,
                headers,
            };

            let responder = Responder {
                encoder: H2Encoder {
                    stream_id,
                    tx: ev_tx.clone(),
                    state: EncoderState::ExpectResponseHeaders,
                },
                state: ExpectResponseHeaders,
            };

            let (piece_tx, piece_rx) = mpsc::channel::<H2BodyItem>(1); // TODO: is 1 a sensible value here?

            let req_body = H2Body {
                // FIXME: that's not right. h2 requests can still specify
                // a content-length
                content_length: if data.end_stream { Some(0) } else { None },
                eof: data.end_stream,
                rx: piece_rx,
            };

            maybe_uring::spawn({
                let driver = driver.clone();
                async move {
                    let mut req_body = req_body;
                    let responder = responder;

                    match driver.handle(req, &mut req_body, responder).await {
                        Ok(_responder) => {
                            debug!("Handler completed successfully, gave us a responder");
                        }
                        Err(e) => {
                            // TODO: actually handle that error.
                            debug!("Handler returned an error: {e}")
                        }
                    }
                }
            });

            stream_state.rx_stage = if data.end_stream {
                StreamRxStage::Done
            } else {
                StreamRxStage::Body(piece_tx)
            };
        }
        StreamRxStage::Body(_) => unreachable!(),
        StreamRxStage::Trailers(body_tx, data) => {
            decode_data(HeadersOrTrailers::Trailers, &data)?;

            if body_tx
                .send(Ok(PieceOrTrailers::Trailers(Box::new(headers))))
                .await
                .is_err()
            {
                warn!("TODO: The body is being ignored, we should reset the stream");
            }
        }
        StreamRxStage::Done => unreachable!(),
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HeadersOrTrailers {
    Headers,
    Trailers,
}
