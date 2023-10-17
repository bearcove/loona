use std::{borrow::Cow, rc::Rc};

use fluke_buffet::{Piece, PieceStr, Roll, RollMut};
use fluke_maybe_uring::io::ReadOwned;
use http::{
    header,
    uri::{Authority, PathAndQuery, Scheme},
    HeaderName, Version,
};
use nom::Finish;
use smallvec::smallvec;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{
    h2::{
        parse::{Frame, FrameType, PrioritySpec},
        types::{ConnectionClosed, H2ConnectionError},
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
    types::{
        ConnState, ContinuationState, H2ConnEvent, H2Error, H2Result, H2StreamError, HeadersData,
        HeadersOrTrailers, StreamStage,
    },
};

/// Reads and processes h2 frames from the client.
pub(crate) struct H2ReadContext<D: ServerDriver + 'static> {
    driver: Rc<D>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    state: ConnState,
    hpack_dec: fluke_hpack::Decoder<'static>,
    // TODO: kill, cf. https://github.com/hapsoc/fluke/issues/121
    continuation_state: ContinuationState,
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
            continuation_state: ContinuationState::Idle,
        }
    }

    pub(crate) async fn read_loop(
        &mut self,
        mut client_buf: RollMut,
        mut transport_r: impl ReadOwned,
    ) -> eyre::Result<()> {
        loop {
            let frame;
            (client_buf, frame) =
                match read_and_parse(Frame::parse, &mut transport_r, client_buf, 128).await? {
                    Some((client_buf, frame)) => (client_buf, frame),
                    None => {
                        // TODO: this can be fine, if we've sent a GO_AWAY
                        debug!("h2 client closed connection");
                        return Err(eyre::Report::from(ConnectionClosed));
                    }
                };

            debug!(?frame, "Received");

            match &frame.frame_type {
                FrameType::Headers(..) | FrameType::Data(..) => {
                    let max_frame_size = self.state.self_settings.max_frame_size;
                    debug!(
                        "frame len = {}, max_frame_size = {max_frame_size}",
                        frame.len
                    );
                    if frame.len > max_frame_size {
                        self.send_goaway(H2ConnectionError::FrameTooLarge {
                            frame_type: frame.frame_type,
                            frame_size: frame.len,
                            max_frame_size,
                        })
                        .await;
                        return Err(eyre::Report::from(ConnectionClosed));
                    }
                }
                _ => {
                    // muffin.
                }
            }

            // TODO: there might be optimizations to be done for `Data` frames later
            // on, but for now, let's unconditionally read the payload (if it's not
            // empty).
            let payload: Roll = if frame.len == 0 {
                Roll::empty()
            } else {
                let payload_roll;
                (client_buf, payload_roll) = match read_and_parse(
                    nom::bytes::streaming::take(frame.len as usize),
                    &mut transport_r,
                    client_buf,
                    frame.len as usize,
                )
                .await?
                {
                    Some((client_buf, payload)) => (client_buf, payload),
                    None => {
                        debug!(
                            "h2 client closed connection while reading payload for {:?}",
                            frame.frame_type
                        );
                        return Ok(());
                    }
                };
                payload_roll
            };

            if let Err(e) = self.process_frame(frame, payload).await {
                match e {
                    H2Error::Connection(e) => {
                        self.send_goaway(e).await;
                        // TODO: keep track that we sent a goaway.
                        // TODO: don't let the inner functions send a goaway themselves,
                        // so all goaways have to go through here?
                        // keep looping
                    }
                    H2Error::Stream(id, e) => {
                        self.send_rst(id, e).await;
                    }
                }
            }
        }
    }

    // TODO: return `H2Error` from this, which can accommodate both connection-level
    // and stream-level errors.
    // cf.
    async fn process_frame(&mut self, frame: Frame, payload: Roll) -> H2Result {
        match self.continuation_state {
            ContinuationState::Idle => self.process_idle_frame(frame, payload).await,
            ContinuationState::ContinuingHeaders(expected_stream_id) => {
                // TODO: kill, cf. https://github.com/hapsoc/fluke/issues/121
                self.process_continuation_frame(frame, payload, expected_stream_id)
                    .await
            }
        }
    }

    async fn process_idle_frame(&mut self, frame: Frame, mut payload: Roll) -> H2Result {
        match frame.frame_type {
            FrameType::Data(flags) => {
                if flags.contains(DataFlags::Padded) {
                    if payload.is_empty() {
                        todo!("handle connection error: padded data frame, but no padding length");
                    }

                    let padding_length_roll;
                    (padding_length_roll, payload) = payload.split_at(1);
                    let padding_length = padding_length_roll[0] as usize;
                    if payload.len() < padding_length {
                        todo!(
                            "handle connection error: padded headers frame, but not enough padding"
                        );
                    }

                    let at = payload.len() - padding_length;
                    (payload, _) = payload.split_at(at);
                }

                let body_tx = {
                    let stage = self
                        .state
                        .streams
                        .get_mut(&frame.stream_id)
                        // TODO: proper error handling (connection error)
                        .expect("received data for unknown stream");
                    match stage {
                        StreamStage::Headers(_) => {
                            // TODO: proper error handling (stream error)
                            panic!("expected headers, received data")
                        }
                        StreamStage::Trailers(..) => {
                            // TODO: proper error handling (stream error)
                            panic!("expected trailers, received data")
                        }
                        StreamStage::Open(tx) => {
                            // TODO: we can get rid of that clone sometimes
                            let tx = tx.clone();
                            if flags.contains(DataFlags::EndStream) {
                                *stage = StreamStage::HalfClosedRemote;
                            }
                            tx
                        }
                        StreamStage::HalfClosedRemote | StreamStage::Closed => {
                            return Err(H2StreamError::ReceivedDataForClosedStream
                                .for_stream(frame.stream_id));
                        }
                    }
                };

                if body_tx
                    .send(Ok(PieceOrTrailers::Piece(payload.into())))
                    .await
                    .is_err()
                {
                    warn!("TODO: The body is being ignored, we should reset the stream");
                }
            }
            FrameType::Headers(flags) => {
                // TODO: if we're shutting down, ignore streams higher
                // than the last one we accepted.

                if frame.stream_id.is_server_initiated() {
                    self.send_goaway(H2ConnectionError::ClientSidShouldBeOdd)
                        .await;
                    return Ok(());
                }

                if frame.stream_id < self.state.last_stream_id {
                    self.send_goaway(H2ConnectionError::ClientSidShouldBeIncreasing)
                        .await;
                }
                self.state.last_stream_id = frame.stream_id;

                let padding_length = if flags.contains(HeadersFlags::Padded) {
                    if payload.is_empty() {
                        self.send_goaway(H2ConnectionError::PaddedFrameEmpty).await;
                        return Ok(());
                    }

                    let padding_length_roll;
                    (padding_length_roll, payload) = payload.split_at(1);
                    padding_length_roll[0] as usize
                } else {
                    0
                };

                if flags.contains(HeadersFlags::Priority) {
                    let pri_spec;
                    (payload, pri_spec) = PrioritySpec::parse(payload)
                        .finish()
                        .map_err(|err| eyre::eyre!("parsing error: {err:?}"))?;
                    debug!(exclusive = %pri_spec.exclusive, stream_dependency = ?pri_spec.stream_dependency, weight = %pri_spec.weight, "received priority, exclusive");

                    if pri_spec.stream_dependency == frame.stream_id {
                        self.send_goaway(H2ConnectionError::HeadersInvalidPriority {
                            stream_id: frame.stream_id,
                        })
                        .await;
                        return Ok(());
                    }
                }

                if padding_length > 0 {
                    if payload.len() < padding_length {
                        self.send_goaway(H2ConnectionError::PaddedFrameTooShort)
                            .await;
                    }

                    let at = payload.len() - padding_length;
                    (payload, _) = payload.split_at(at);
                }

                let headers_data = HeadersData {
                    end_stream: flags.contains(HeadersFlags::EndStream),
                    fragments: smallvec![payload],
                };

                match self.state.streams.get_mut(&frame.stream_id) {
                    Some(stage) => {
                        debug!("Receiving trailers for stream {}", frame.stream_id);

                        if !flags.contains(HeadersFlags::EndStream) {
                            todo!(
                                            "handle connection error: trailers must have EndStream, this just looks like duplicate headers for stream {}",
                                            frame.stream_id
                                        );
                        }

                        let prev_stage = std::mem::replace(stage, StreamStage::HalfClosedRemote);
                        match prev_stage {
                            StreamStage::Open(body_tx) => {
                                *stage = StreamStage::Trailers(body_tx, headers_data);
                            }
                            // FIXME: that's a connection error
                            _ => unreachable!(),
                        }
                    }
                    None => {
                        debug!(
                            "Receiving headers for stream {} (if we accept this stream we'll have {} total)",
                            frame.stream_id,
                            self.state.streams.len() + 1
                        );

                        let num_streams_if_accept = self.state.streams.len() + 1;
                        if num_streams_if_accept
                            > self.state.self_settings.max_concurrent_streams as _
                        {
                            self.send_goaway(H2ConnectionError::MaxConcurrentStreamsExceeded {
                                max_concurrent_streams: self
                                    .state
                                    .self_settings
                                    .max_concurrent_streams,
                            })
                            .await;
                            return Ok(());
                        }
                        self.state
                            .streams
                            .insert(frame.stream_id, StreamStage::Headers(headers_data));
                    }
                }

                if flags.contains(HeadersFlags::EndHeaders) {
                    self.end_headers(frame.stream_id).await?;
                } else {
                    debug!(
                        "expecting more headers/trailers for stream {}",
                        frame.stream_id
                    );
                    self.continuation_state = ContinuationState::ContinuingHeaders(frame.stream_id);
                }
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
                    self.send_goaway(H2ConnectionError::HeadersInvalidPriority {
                        stream_id: frame.stream_id,
                    })
                    .await;
                    return Ok(());
                }
            }
            FrameType::RstStream => todo!("implement RstStream"),
            FrameType::Settings(s) => {
                if s.contains(SettingsFlags::Ack) {
                    debug!("Peer has acknowledged our settings, cool");
                } else {
                    // TODO: actually apply settings
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
                        return Err(
                            eyre::eyre!("could not send H2 acknowledge settings event").into()
                        );
                    }
                }
            }
            FrameType::PushPromise => {
                self.send_goaway(H2ConnectionError::ClientSentPushPromise)
                    .await;
                return Ok(());
            }
            FrameType::Ping(flags) => {
                if frame.stream_id != StreamId::CONNECTION {
                    todo!("handle connection error: ping frame with non-zero stream id");
                }

                if frame.len != 8 {
                    todo!("handle connection error: ping frame with invalid length");
                }

                if flags.contains(PingFlags::Ack) {
                    // TODO: check that payload matches the one we sent?

                    debug!("received ping ack");
                    return Ok(());
                }

                if self.ev_tx.send(H2ConnEvent::Ping(payload)).await.is_err() {
                    return Err(eyre::eyre!("could not send H2 ping event").into());
                }
            }
            FrameType::GoAway => todo!(),
            FrameType::WindowUpdate => match frame.stream_id.0 {
                0 => {
                    debug!("TODO: ignoring connection-wide window update");
                }
                _ => match self.state.streams.get_mut(&frame.stream_id) {
                    Some(_ss) => {
                        debug!("TODO: handle window update for stream {}", frame.stream_id)
                    }
                    None => {
                        self.send_goaway(H2ConnectionError::WindowUpdateForUnknownStream {
                            stream_id: frame.stream_id,
                        })
                        .await;
                    }
                },
            },
            FrameType::Continuation(_flags) => {
                self.send_goaway(H2ConnectionError::UnexpectedContinuationFrame {
                    stream_id: frame.stream_id,
                })
                .await;
            }
            FrameType::Unknown(ft) => {
                trace!(
                    "ignoring unknown frame with type 0x{:x}, flags 0x{:x}",
                    ft.ty,
                    ft.flags
                );
            }
        }

        Ok(())
    }

    async fn process_continuation_frame(
        &mut self,
        frame: Frame,
        payload: Roll,
        expected_stream_id: StreamId,
    ) -> H2Result {
        let flags = match frame.frame_type {
            FrameType::Continuation(flags) => flags,
            other => {
                return Err(H2ConnectionError::ExpectedContinuationFrame {
                    stream_id: expected_stream_id,
                    frame_type: other,
                }
                .into())
            }
        };

        if frame.stream_id != expected_stream_id {
            return Err(H2ConnectionError::ExpectedContinuationForStream {
                stream_id: expected_stream_id,
                continuation_stream_id: frame.stream_id,
            }
            .into());
        }

        // unwrap rationale: we just checked that this is a
        // continuation of a stream we've already learned about.
        let ss = self.state.streams.get_mut(&frame.stream_id).unwrap();
        match ss {
            StreamStage::Headers(data) | StreamStage::Trailers(_, data) => {
                data.fragments.push(payload);
            }
            _ => {
                // FIXME: store `HeadersData` in
                // `ContinuationState` directly so this
                // branch doesn't even exist.
                unreachable!()
            }
        }

        if flags.contains(ContinuationFlags::EndHeaders) {
            self.end_headers(frame.stream_id).await
        } else {
            debug!(
                "expecting more headers/trailers for stream {}",
                frame.stream_id
            );
            Ok(())
        }
    }

    async fn send_goaway(&self, err: H2ConnectionError) {
        // TODO: this should change the global server state: we should ignore
        // any streams higher than the last stream id we've seen after
        // we've done that.

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

    async fn send_rst(&mut self, stream_id: StreamId, e: H2StreamError) {
        let error_code = e.as_known_error_code();
        warn!("sending rst because: {e} (known error code: {error_code:?})");

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

    // TODO: this can all be, again, greatly simplified because continuation
    // frames are supposed to follow the headers frames directly, so we don't
    // need any of that in the state machine, we can just keep popping more
    // frames.
    async fn end_headers(&mut self, stream_id: StreamId) -> H2Result {
        let stage = self
            .state
            .streams
            .get_mut(&stream_id)
            // FIXME: don't panic
            .expect("stream stage must exist");

        let mut method: Option<Method> = None;
        let mut scheme: Option<Scheme> = None;
        let mut path: Option<PieceStr> = None;
        let mut authority: Option<Authority> = None;

        let mut headers = Headers::default();

        let mut decode_data =
            |headers_or_trailers: HeadersOrTrailers, data: &HeadersData| -> H2Result {
                // TODO: find a way to propagate errors from here - probably will have to change
                // the function signature in fluke-hpack.
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
                        self.hpack_dec
                            .decode_with_cb(&payload[..], on_header_pair)
                            .map_err(|e| H2ConnectionError::CompressionError(format!("{e:?}")))?;
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
                        self.hpack_dec
                            .decode_with_cb(&payload[..], on_header_pair)
                            .map_err(|e| H2ConnectionError::CompressionError(format!("{e:?}")))?;
                    }
                };

                Ok(())
            };

        // FIXME: don't panic on unexpected states here
        let mut prev_stage = StreamStage::Closed;
        std::mem::swap(&mut prev_stage, stage);

        match prev_stage {
            StreamStage::Headers(data) => {
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
                    content_length: if data.end_stream { Some(0) } else { None },
                    eof: data.end_stream,
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

                *stage = if data.end_stream {
                    StreamStage::HalfClosedRemote
                } else {
                    StreamStage::Open(piece_tx)
                };
            }
            StreamStage::Open(_) => unreachable!(),
            StreamStage::Trailers(body_tx, data) => {
                decode_data(HeadersOrTrailers::Trailers, &data)?;

                if body_tx
                    .send(Ok(PieceOrTrailers::Trailers(Box::new(headers))))
                    .await
                    .is_err()
                {
                    warn!("TODO: The body is being ignored, we should reset the stream");
                }
            }
            StreamStage::HalfClosedRemote => unreachable!(),
            StreamStage::Closed => unreachable!(),
        }

        Ok(())
    }
}
