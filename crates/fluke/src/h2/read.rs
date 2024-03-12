use std::{
    borrow::Cow,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
};

use enumflags2::BitFlags;
use eyre::Context;
use fluke_buffet::{Piece, PieceStr, Roll, RollMut};
use fluke_maybe_uring::io::ReadOwned;
use http::{
    header,
    uri::{Authority, PathAndQuery, Scheme},
    HeaderName, Version,
};
use nom::Finish;
use smallvec::{smallvec, SmallVec};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{
    h2::{
        parse::{Frame, FrameType, PrioritySpec},
        types::H2ConnectionError,
    },
    util::read_and_parse,
    ExpectResponseHeaders, Headers, Method, Request, Responder, ServerDriver,
};

use super::{
    body::{H2Body, H2BodyItem, PieceOrTrailers},
    encode::{EncoderState, H2Encoder},
    parse::{
        ContinuationFlags, DataFlags, HeadersFlags, PingFlags, Settings, SettingsFlags, StreamId,
    },
    types::{ConnState, H2ConnEvent, H2StreamError, HeadersOrTrailers, StreamState},
};

/// Reads and processes h2 frames from the client.
pub(crate) struct H2ReadContext<D: ServerDriver + 'static> {
    driver: Rc<D>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    state: ConnState,
    hpack_dec: fluke_hpack::Decoder<'static>,

    /// Whether we've received a GOAWAY frame.
    pub goaway_recv: bool,
}

impl<D: ServerDriver + 'static> H2ReadContext<D> {
    pub(crate) fn new(driver: Rc<D>, ev_tx: mpsc::Sender<H2ConnEvent>, state: ConnState) -> Self {
        let mut hpack_dec = fluke_hpack::Decoder::new();
        hpack_dec
            .set_max_allowed_table_size(Settings::default().header_table_size.try_into().unwrap());

        Self {
            driver,
            ev_tx,
            state,
            hpack_dec,
            goaway_recv: false,
        }
    }

    pub(crate) async fn read_loop(
        mut self,
        client_buf: RollMut,
        transport_r: impl ReadOwned,
    ) -> eyre::Result<()> {
        let mut goaway_err: Option<H2ConnectionError> = None;

        {
            // read frames and send them into an mpsc buffer of size 1
            let (tx, rx) = mpsc::channel::<(Frame, Roll)>(1);

            // store max frame size setting as an atomic so we can share it across tasks
            // FIXME: the process_task should update this
            let max_frame_size = Rc::new(AtomicU32::new(self.state.self_settings.max_frame_size));

            let mut io_task =
                std::pin::pin!(Self::io_loop(client_buf, transport_r, tx, max_frame_size));
            let mut process_task = std::pin::pin!(self.process_loop(rx));

            tokio::select! {
                res = &mut io_task => {
                    debug!(?res, "h2 io task finished");

                    if let Err(H2ConnectionError::ReadError(e)) = res {
                        let mut should_ignore_err = false;

                        // if this is a connection reset and we've sent a goaway, ignore it
                        if let Some(io_error) = e.root_cause().downcast_ref::<std::io::Error>() {
                            if io_error.kind() == std::io::ErrorKind::ConnectionReset {
                                should_ignore_err = true;
                            }
                        }

                        if !should_ignore_err {
                            return Err(e.wrap_err("h2 io"));
                        }
                    }

                    if let Err(e) = (&mut process_task).await {
                        debug!("h2 process task finished with error: {e}");
                        return Err(e).wrap_err("h2 process");
                    }
                }
                res = &mut process_task => {
                    debug!(?res, "h2 process task finished");

                    if let Err(err) = res {
                        goaway_err = Some(err);
                    }

                    // probably don't need to wait for the io task there
                }
            }
        }

        if let Some(err) = goaway_err {
            if self
                .ev_tx
                .send(H2ConnEvent::GoAway {
                    err,
                    last_stream_id: self.state.last_stream_id,
                })
                .await
                .is_err()
            {
                debug!("error sending goaway");
            }
        }

        Ok(())
    }

    async fn io_loop(
        mut client_buf: RollMut,
        mut transport_r: impl ReadOwned,
        tx: mpsc::Sender<(Frame, Roll)>,
        max_frame_size: Rc<AtomicU32>,
    ) -> Result<(), H2ConnectionError> {
        'read_frames: loop {
            const MAX_FRAME_HEADER_SIZE: usize = 128;
            let frame;
            let frame_res = read_and_parse(
                Frame::parse,
                &mut transport_r,
                client_buf,
                MAX_FRAME_HEADER_SIZE,
            )
            .await;

            let maybe_frame = match frame_res {
                Ok(inner) => inner,
                Err(e) => return Err(H2ConnectionError::ReadError(e)),
            };
            (client_buf, frame) = match maybe_frame {
                Some((client_buf, frame)) => (client_buf, frame),
                None => {
                    debug!("Peer went away before sending a frame");
                    break 'read_frames;
                }
            };

            debug!(?frame, "Received");

            let max_frame_size = max_frame_size.load(Ordering::Relaxed);
            if frame.len > max_frame_size {
                return Err(H2ConnectionError::FrameTooLarge {
                    frame_type: frame.frame_type,
                    frame_size: frame.len,
                    max_frame_size,
                });
            }

            let mut payload;
            (client_buf, payload) = match read_and_parse(
                nom::bytes::streaming::take(frame.len as usize),
                &mut transport_r,
                client_buf,
                frame.len as usize,
            )
            .await?
            {
                Some((client_buf, payload)) => (client_buf, payload),
                None => {
                    return Err(H2ConnectionError::IncompleteFrame {
                        frame_type: frame.frame_type,
                        frame_size: frame.len,
                    })
                }
            };

            let has_padding = match frame.frame_type {
                FrameType::Data(flags) => flags.contains(DataFlags::Padded),
                FrameType::Headers(flags) => flags.contains(HeadersFlags::Padded),
                _ => false,
            };

            if has_padding {
                if payload.is_empty() {
                    return Err(H2ConnectionError::PaddedFrameEmpty {
                        frame_type: frame.frame_type,
                    });
                }

                let padding_length_roll;
                (padding_length_roll, payload) = payload.split_at(1);
                let padding_length = padding_length_roll[0] as usize;
                if payload.len() < padding_length {
                    return Err(H2ConnectionError::PaddedFrameTooShort {
                        frame_type: frame.frame_type,
                        padding_length,
                        frame_size: frame.len,
                    });
                }

                // padding is on the end of the payload
                let at = payload.len() - padding_length;
                (payload, _) = payload.split_at(at);
            }

            if tx.send((frame, payload)).await.is_err() {
                debug!("h2 io loop: receiver dropped, closing connection");
                return Ok(());
            }
        }

        Ok(())
    }

    async fn process_loop(
        &mut self,
        mut rx: mpsc::Receiver<(Frame, Roll)>,
    ) -> Result<(), H2ConnectionError> {
        loop {
            let (frame, mut payload) = match rx.recv().await {
                Some(t) => t,
                None => {
                    debug!("h2 process task: peer hung up");
                    break;
                }
            };

            match frame.frame_type {
                FrameType::Data(flags) => {
                    let ss = self.state.streams.get_mut(&frame.stream_id).ok_or(
                        H2ConnectionError::StreamClosed {
                            stream_id: frame.stream_id,
                        },
                    )?;

                    match ss {
                        StreamState::Open(tx) => {
                            if tx
                                .send(Ok(PieceOrTrailers::Piece(payload.into())))
                                .await
                                .is_err()
                            {
                                warn!(
                                    "TODO: The body is being ignored, we should reset the stream"
                                );
                            }

                            if flags.contains(DataFlags::EndStream) {
                                *ss = StreamState::HalfClosedRemote;
                            }
                        }
                        StreamState::HalfClosedRemote => {
                            debug!(
                                stream_id = %frame.stream_id,
                                "Received data for closed stream"
                            );
                            self.send_rst(frame.stream_id, H2StreamError::StreamClosed)
                                .await
                        }
                    }
                }
                FrameType::Headers(flags) => {
                    if flags.contains(HeadersFlags::Priority) {
                        let pri_spec;
                        (payload, pri_spec) = PrioritySpec::parse(payload)
                            .finish()
                            .map_err(|err| eyre::eyre!("parsing error: {err:?}"))?;
                        debug!(exclusive = %pri_spec.exclusive, stream_dependency = ?pri_spec.stream_dependency, weight = %pri_spec.weight, "received priority, exclusive");

                        if pri_spec.stream_dependency == frame.stream_id {
                            return Err(H2ConnectionError::HeadersInvalidPriority {
                                stream_id: frame.stream_id,
                            });
                        }
                    }

                    let headers_or_trailers;
                    let mode;

                    match self.state.streams.get_mut(&frame.stream_id) {
                        None => {
                            headers_or_trailers = HeadersOrTrailers::Headers;
                            debug!(
                                stream_id = %frame.stream_id,
                                last_stream_id = %self.state.last_stream_id,
                                next_stream_count = %self.state.streams.len() + 1,
                                "Receiving headers",
                            );

                            if frame.stream_id.is_server_initiated() {
                                return Err(H2ConnectionError::ClientSidShouldBeOdd);
                            }

                            if frame.stream_id <= self.state.last_stream_id {
                                debug!(
                                    frame_stream_id = %frame.stream_id,
                                    last_stream_id = %self.state.last_stream_id,
                                    "Received headers for invalid stream ID"
                                );

                                // this stream may have existed, but it no longer does:
                                return Err(H2ConnectionError::StreamClosed {
                                    stream_id: frame.stream_id,
                                });
                            } else {
                                // TODO: if we're shutting down, ignore streams higher
                                // than the last one we accepted.

                                let max_concurrent_streams =
                                    self.state.self_settings.max_concurrent_streams;
                                let num_streams_if_accept = self.state.streams.len() + 1;
                                if num_streams_if_accept > max_concurrent_streams as _ {
                                    // reset the stream, indicating we refused it
                                    self.send_rst(frame.stream_id, H2StreamError::RefusedStream)
                                        .await;

                                    // but we still need to skip over any continuation frames
                                    mode = ReadHeadersMode::Skip;
                                } else {
                                    self.state.last_stream_id = frame.stream_id;
                                    mode = ReadHeadersMode::Process;
                                }
                            }
                        }
                        Some(StreamState::Open(_)) => {
                            headers_or_trailers = HeadersOrTrailers::Trailers;
                            debug!("Receiving trailers for stream {}", frame.stream_id);

                            if flags.contains(HeadersFlags::EndStream) {
                                // good, that's what we expect
                                mode = ReadHeadersMode::Process;
                            } else {
                                // ignore trailers, we're not accepting the stream
                                mode = ReadHeadersMode::Skip;

                                self.send_rst(frame.stream_id, H2StreamError::TrailersNotEndStream)
                                    .await
                            }
                        }
                        Some(StreamState::HalfClosedRemote) => {
                            return Err(H2ConnectionError::StreamClosed {
                                stream_id: frame.stream_id,
                            });
                        }
                    }

                    self.read_headers(
                        headers_or_trailers,
                        mode,
                        flags,
                        frame.stream_id,
                        payload,
                        &mut rx,
                    )
                    .await?;
                }
                FrameType::Priority => {
                    let pri_spec = match PrioritySpec::parse(payload) {
                        Ok((_rest, pri_spec)) => pri_spec,
                        Err(e) => {
                            todo!("handle connection error: invalid priority frame {e}")
                        }
                    };
                    debug!(?pri_spec, "received priority frame");

                    if pri_spec.stream_dependency == frame.stream_id {
                        return Err(H2ConnectionError::HeadersInvalidPriority {
                            stream_id: frame.stream_id,
                        });
                    }
                }
                FrameType::RstStream => match self.state.streams.remove(&frame.stream_id) {
                    None => {
                        return Err(H2ConnectionError::RstStreamForUnknownStream {
                            stream_id: frame.stream_id,
                        })
                    }
                    Some(ss) => match ss {
                        StreamState::Open(body_tx) => {
                            _ = body_tx
                                .send(Err(H2StreamError::ReceivedRstStream.into()))
                                .await;
                        }
                        StreamState::HalfClosedRemote => {
                            // good
                        }
                    },
                },
                FrameType::Settings(s) => {
                    if frame.stream_id != StreamId::CONNECTION {
                        return Err(H2ConnectionError::SettingsWithNonZeroStreamId {
                            stream_id: frame.stream_id,
                        });
                    }

                    if s.contains(SettingsFlags::Ack) {
                        debug!("Peer has acknowledged our settings, cool");
                        if !payload.is_empty() {
                            return Err(H2ConnectionError::SettingsAckWithPayload {
                                len: payload.len() as _,
                            });
                        }
                    } else {
                        let (_, settings) = Settings::parse(payload)
                            .finish()
                            .map_err(|err| eyre::eyre!("parsing error: {err:?}"))?;
                        let new_max_header_table_size = settings.header_table_size;
                        debug!(?settings, "Received settings");
                        self.state.peer_settings = settings;

                        if self
                            .ev_tx
                            .send(H2ConnEvent::AcknowledgeSettings {
                                new_max_header_table_size,
                            })
                            .await
                            .is_err()
                        {
                            return Err(eyre::eyre!(
                                "could not send H2 acknowledge settings event"
                            )
                            .into());
                        }
                    }
                }
                FrameType::PushPromise => {
                    return Err(H2ConnectionError::ClientSentPushPromise);
                }
                FrameType::Ping(flags) => {
                    if frame.stream_id != StreamId::CONNECTION {
                        return Err(H2ConnectionError::PingFrameWithNonZeroStreamId {
                            stream_id: frame.stream_id,
                        });
                    }

                    if frame.len != 8 {
                        return Err(H2ConnectionError::PingFrameInvalidLength { len: frame.len });
                    }

                    if flags.contains(PingFlags::Ack) {
                        // TODO: check that payload matches the one we sent?
                        return Ok(());
                    }

                    if self.ev_tx.send(H2ConnEvent::Ping(payload)).await.is_err() {
                        return Err(eyre::eyre!("could not send H2 ping event").into());
                    }
                }
                FrameType::GoAway => {
                    self.goaway_recv = true;

                    // TODO: this should probably have other effects than setting
                    // this flag.
                }
                FrameType::WindowUpdate => match frame.stream_id.0 {
                    0 => {
                        debug!("TODO: ignoring connection-wide window update");
                    }
                    _ => match self.state.streams.get_mut(&frame.stream_id) {
                        Some(_ss) => {
                            debug!("TODO: handle window update for stream {}", frame.stream_id)
                        }
                        None => {
                            return Err(H2ConnectionError::WindowUpdateForUnknownStream {
                                stream_id: frame.stream_id,
                            });
                        }
                    },
                },
                FrameType::Continuation(_flags) => {
                    return Err(H2ConnectionError::UnexpectedContinuationFrame {
                        stream_id: frame.stream_id,
                    });
                }
                FrameType::Unknown(ft) => {
                    trace!(
                        "ignoring unknown frame with type 0x{:x}, flags 0x{:x}",
                        ft.ty,
                        ft.flags
                    );
                }
            }
        }

        Ok(())
    }

    async fn send_rst(&mut self, stream_id: StreamId, e: H2StreamError) {
        let error_code = e.as_known_error_code();
        debug!("Sending rst because: {e} (known error code: {error_code:?})");

        if self
            .ev_tx
            .send(H2ConnEvent::RstStream {
                stream_id,
                error_code,
            })
            .await
            .is_err()
        {
            debug!("error sending rst");
        }

        self.state.streams.remove(&stream_id);
    }

    async fn read_headers(
        &mut self,
        headers_or_trailers: HeadersOrTrailers,
        mode: ReadHeadersMode,
        flags: BitFlags<HeadersFlags, u8>,
        stream_id: StreamId,
        payload: Roll,
        rx: &mut mpsc::Receiver<(Frame, Roll)>,
    ) -> Result<(), H2ConnectionError> {
        let end_stream = flags.contains(HeadersFlags::EndStream);

        enum Data {
            Single(Roll),
            Multi(SmallVec<[Roll; 2]>),
        }

        let data = if flags.contains(HeadersFlags::EndHeaders) {
            // good, no continuation frames needed
            Data::Single(payload)
        } else {
            // read continuation frames

            #[allow(unused, clippy::let_unit_value)]
            let flags = (); // don't accidentally use the `flags` variable

            let mut fragments = smallvec![payload];

            loop {
                let (continuation_frame, continuation_payload) = match rx.recv().await {
                    Some(t) => t,
                    None => {
                        // even though this error is "for a stream", it's a
                        // connection error, because it means the peer doesn't
                        // know how to speak HTTP/2.
                        return Err(H2ConnectionError::ExpectedContinuationFrame {
                            stream_id,
                            frame_type: None,
                        });
                    }
                };

                if stream_id != continuation_frame.stream_id {
                    return Err(H2ConnectionError::ExpectedContinuationForStream {
                        stream_id,
                        continuation_stream_id: continuation_frame.stream_id,
                    });
                }

                let cont_flags = match continuation_frame.frame_type {
                    FrameType::Continuation(flags) => flags,
                    other => {
                        return Err(H2ConnectionError::ExpectedContinuationFrame {
                            stream_id,
                            frame_type: Some(other),
                        })
                    }
                };

                // add fragment
                fragments.push(continuation_payload);

                if cont_flags.contains(ContinuationFlags::EndHeaders) {
                    // we're done
                    break;
                }
            }

            Data::Multi(fragments)
        };

        if matches!(mode, ReadHeadersMode::Skip) {
            // that's all we need to do: we're not actually validating the
            // headers, we already send a RST
            return Ok(());
        }

        let mut method: Option<Method> = None;
        let mut scheme: Option<Scheme> = None;
        let mut path: Option<PieceStr> = None;
        let mut authority: Option<Authority> = None;

        let mut headers = Headers::default();

        // TODO: find a way to propagate errors from here - probably will have to change
        // the function signature in fluke-hpack, or just write to some captured
        // error
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
                        if method.replace(Method::from(value)).is_some() {
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
                                            // this case but rejecting duplicates seems reasonable.)
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

        match data {
            Data::Single(payload) => {
                self.hpack_dec
                    .decode_with_cb(&payload[..], on_header_pair)
                    .map_err(|e| H2ConnectionError::CompressionError(format!("{e:?}")))?;
            }
            Data::Multi(fragments) => {
                let total_len = fragments.iter().map(|f| f.len()).sum();
                // this is a slow path, let's do a little heap allocation. we could
                // be using `RollMut` for this, but it would probably need to resize
                // a bunch
                let mut payload = Vec::with_capacity(total_len);
                for frag in &fragments {
                    payload.extend_from_slice(&frag[..]);
                }
                self.hpack_dec
                    .decode_with_cb(&payload[..], on_header_pair)
                    .map_err(|e| H2ConnectionError::CompressionError(format!("{e:?}")))?;
            }
        };

        match headers_or_trailers {
            HeadersOrTrailers::Headers => {
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
                        tx: self.ev_tx.clone(),
                        state: EncoderState::ExpectResponseHeaders,
                    },
                    // TODO: why tf is this state encoded twice? is that really
                    // necessary? I know it's for typestates and H2Encoder needs
                    // to look up its state at runtime I guess, but.. that's not great?
                    state: ExpectResponseHeaders,
                };

                let (piece_tx, piece_rx) = mpsc::channel::<H2BodyItem>(1); // TODO: is 1 a sensible value here?

                let req_body = H2Body {
                    // FIXME: that's not right. h2 requests can still specify
                    // a content-length
                    content_length: if end_stream { Some(0) } else { None },
                    eof: end_stream,
                    rx: piece_rx,
                };

                fluke_maybe_uring::spawn({
                    let driver = self.driver.clone();
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

                self.state.streams.insert(
                    stream_id,
                    if end_stream {
                        StreamState::HalfClosedRemote
                    } else {
                        StreamState::Open(piece_tx)
                    },
                );
            }
            HeadersOrTrailers::Trailers => {
                match self.state.streams.get_mut(&stream_id) {
                    Some(StreamState::Open(body_tx)) => {
                        if body_tx
                            .send(Ok(PieceOrTrailers::Trailers(Box::new(headers))))
                            .await
                            .is_err()
                        {
                            // the body is being ignored, but there's no point in
                            // resetting the stream since we just got the end of it
                        }
                    }
                    _ => {
                        unreachable!("stream state should be open when we receive trailers")
                    }
                }
                self.state.streams.remove(&stream_id);
            }
        }

        Ok(())
    }
}

enum ReadHeadersMode {
    // we're accepting the stream or processing trailers, we want to
    // process the headers we read.
    Process,
    // we're refusing the stream, we want to skip over the headers we read.
    Skip,
}
