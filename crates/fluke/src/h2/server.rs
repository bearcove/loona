use std::{borrow::Cow, collections::HashMap, rc::Rc};

use enumflags2::BitFlags;
use eyre::Context;
use futures_util::TryFutureExt;
use http::{
    header::{self, HeaderName},
    uri::{Authority, PathAndQuery, Scheme},
    Version,
};
use nom::Finish;
use smallvec::{smallvec, SmallVec};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{
    h2::{
        body::PieceOrTrailers,
        encode::{H2ConnEvent, H2EventPayload},
        parse::{
            self, ContinuationFlags, DataFlags, Frame, FrameType, HeadersFlags, KnownErrorCode,
            PingFlags, PrioritySpec, SettingsFlags, StreamId,
        },
    },
    util::read_and_parse,
    ExpectResponseHeaders, Headers, Method, Request, Responder, ServerDriver,
};
use fluke_buffet::{Piece, PieceList, PieceStr, Roll, RollMut};
use fluke_maybe_uring::io::{ReadOwned, WriteOwned};

use super::{
    body::{H2Body, H2BodyItem, H2BodySender},
    encode::{EncoderState, H2Encoder},
    parse::Settings,
};

/// HTTP/2 server configuration
pub struct ServerConf {
    pub max_streams: u32,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self { max_streams: 32 }
    }
}

pub(crate) struct ConnState {
    pub(crate) streams: HashMap<StreamId, StreamStage>,
    pub(crate) last_stream_id: StreamId,
    pub(crate) self_settings: Settings,
    pub(crate) peer_settings: Settings,
}

impl Default for ConnState {
    fn default() -> Self {
        Self {
            streams: Default::default(),
            last_stream_id: StreamId(0),
            self_settings: Default::default(),
            peer_settings: Default::default(),
        }
    }
}

#[derive(Default, Clone, Copy)]
enum ContinuationState {
    #[default]
    Idle,
    ContinuingHeaders(StreamId),
}

pub(crate) enum StreamStage {
    Headers(HeadersData),
    Body(H2BodySender),
    Trailers(H2BodySender, HeadersData),
    Done,
}

pub(crate) struct HeadersData {
    /// If true, no DATA frames follow, cf. https://httpwg.org/specs/rfc9113.html#HttpFraming
    pub(crate) end_stream: bool,

    /// The field block fragments
    pub(crate) fragments: SmallVec<[Roll; 2]>,
}

pub async fn serve(
    (mut transport_r, mut transport_w): (impl ReadOwned, impl WriteOwned),
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: Rc<impl ServerDriver + 'static>,
) -> eyre::Result<()> {
    let mut state = ConnState::default();
    state.self_settings.max_concurrent_streams = conf.max_streams;

    (client_buf, _) = match read_and_parse(
        parse::preface,
        &mut transport_r,
        client_buf,
        parse::PREFACE.len(),
    )
    .await?
    {
        Some((client_buf, frame)) => (client_buf, frame),
        None => {
            debug!("h2 client closed connection before sending preface");
            return Ok(());
        }
    };
    debug!("read preface");

    let mut out_scratch = RollMut::alloc()?;

    // we have to send a settings frame
    {
        let payload_roll = state.self_settings.into_roll(&mut out_scratch)?;
        let frame_roll = Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        )
        .with_len(payload_roll.len().try_into().unwrap())
        .into_roll(&mut out_scratch)?;

        transport_w
            .writev_all(vec![frame_roll, payload_roll])
            .await?;
        debug!("sent settings frame");
    }

    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<H2ConnEvent>(32);

    let mut h2_read_cx = H2ReadContext::new(driver.clone(), ev_tx.clone(), state);
    let read_task = h2_read_cx.work(client_buf, transport_r);

    let write_task = h2_write_loop(ev_rx, transport_w, out_scratch);

    let res = tokio::try_join!(
        read_task.map_err(LoopError::Read),
        write_task.map_err(LoopError::Write),
    );
    if let Err(e) = &res {
        if let LoopError::Read(r) = e {
            if r.downcast_ref::<ConnectionClosed>().is_some() {
                return Ok(());
            }
        }
        debug!("caught error from one of the tasks: {e} / {e:#?}");
    }
    res?;

    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum LoopError {
    #[error("read error: {0}")]
    Read(eyre::Report),

    #[error("write error: {0}")]
    Write(eyre::Report),
}

#[derive(thiserror::Error, Debug)]
#[error("connection closed")]
struct ConnectionClosed;

async fn h2_write_loop(
    mut ev_rx: mpsc::Receiver<H2ConnEvent>,
    mut transport_w: impl WriteOwned,
    mut out_scratch: RollMut,
) -> eyre::Result<()> {
    let mut hpack_enc = fluke_hpack::Encoder::new();

    while let Some(ev) = ev_rx.recv().await {
        trace!("h2_write_loop: received H2 event");
        match ev {
            H2ConnEvent::AcknowledgeSettings {
                new_max_header_table_size,
            } => {
                debug!("Acknowledging new settings");
                hpack_enc.set_max_table_size(new_max_header_table_size.try_into().unwrap());

                let frame = Frame::new(
                    FrameType::Settings(SettingsFlags::Ack.into()),
                    StreamId::CONNECTION,
                );
                transport_w
                    .write_all(frame.into_roll(&mut out_scratch)?)
                    .await
                    .wrap_err("writing acknowledge settings")?;
            }
            H2ConnEvent::ServerEvent(ev) => {
                debug!(?ev, "Writing");

                match ev.payload {
                    H2EventPayload::Headers(res) => {
                        let flags = HeadersFlags::EndHeaders;
                        let mut frame = Frame::new(FrameType::Headers(flags.into()), ev.stream_id);

                        // TODO: don't allocate so much for headers. all `encode_into`
                        // wants is an `IntoIter`, we can definitely have a custom iterator
                        // that operates on all this instead of using a `Vec`.

                        // TODO: limit header size
                        let mut headers: Vec<(&[u8], &[u8])> = vec![];
                        headers.push((b":status", res.status.as_str().as_bytes()));
                        for (name, value) in res.headers.iter() {
                            if name == http::header::TRANSFER_ENCODING {
                                // do not set transfer-encoding: chunked when doing HTTP/2
                                continue;
                            }
                            headers.push((name.as_str().as_bytes(), value));
                        }

                        assert_eq!(out_scratch.len(), 0);
                        hpack_enc.encode_into(headers, &mut out_scratch)?;
                        let fragment_block = out_scratch.take_all();

                        frame.len = fragment_block.len() as u32;
                        let frame_roll = frame.into_roll(&mut out_scratch)?;

                        transport_w
                            .writev_all(PieceList::default().with(frame_roll).with(fragment_block))
                            .await
                            .wrap_err("writing headers")?;
                    }
                    H2EventPayload::BodyChunk(chunk) => {
                        let flags = BitFlags::<DataFlags>::default();
                        let frame = Frame::new(FrameType::Data(flags), ev.stream_id)
                            .with_len(chunk.len().try_into().unwrap());
                        let frame_roll = frame.into_roll(&mut out_scratch)?;
                        transport_w
                            .writev_all(PieceList::default().with(frame_roll).with(chunk))
                            .await
                            .wrap_err("writing bodychunk")?;
                    }
                    H2EventPayload::BodyEnd => {
                        let flags = DataFlags::EndStream;
                        let frame = Frame::new(FrameType::Data(flags.into()), ev.stream_id);
                        transport_w
                            .write_all(frame.into_roll(&mut out_scratch)?)
                            .await
                            .wrap_err("writing BodyEnd")?;
                    }
                }
            }
            H2ConnEvent::Ping(payload) => {
                // send pong frame
                let flags = PingFlags::Ack.into();
                let frame = Frame::new(FrameType::Ping(flags), StreamId::CONNECTION)
                    .with_len(payload.len() as u32);
                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(payload),
                    )
                    .await
                    .wrap_err("writing pong")?;
            }
            H2ConnEvent::GoAway {
                err,
                last_stream_id,
            } => {
                let error_code = err.as_known_error_code();
                warn!("connection error: {err} ({err:?}) (code {error_code:?})");

                // let's put something useful in debug data
                let additional_debug_data = format!("{err}").into_bytes();

                debug!(%last_stream_id, ?error_code, "Sending GoAway");
                let header = out_scratch.put_to_roll(8, |mut slice| {
                    use byteorder::{BigEndian, WriteBytesExt};
                    // TODO: do we ever need to write the reserved bit?
                    slice.write_u32::<BigEndian>(last_stream_id.0)?;
                    slice.write_u32::<BigEndian>(error_code.repr())?;

                    Ok(())
                })?;

                let frame = Frame::new(FrameType::GoAway, StreamId::CONNECTION).with_len(
                    (header.len() + additional_debug_data.len())
                        .try_into()
                        .unwrap(),
                );

                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(header)
                            .with(additional_debug_data),
                    )
                    .await
                    .wrap_err("writing goaway")?;
            }
            H2ConnEvent::RstStream {
                stream_id,
                error_code,
            } => {
                debug!(%stream_id, ?error_code, "Sending RstStream");
                let header = out_scratch.put_to_roll(4, |mut slice| {
                    use byteorder::{BigEndian, WriteBytesExt};
                    slice.write_u32::<BigEndian>(error_code.repr())?;
                    Ok(())
                })?;

                let frame = Frame::new(FrameType::RstStream, stream_id)
                    .with_len((header.len()).try_into().unwrap());

                transport_w
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(header),
                    )
                    .await
                    .wrap_err("writing rststream")?;
            }
        }
    }

    Ok(())
}

struct H2ReadContext<D: ServerDriver + 'static> {
    driver: Rc<D>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    state: ConnState,
    hpack_dec: fluke_hpack::Decoder<'static>,
    continuation_state: ContinuationState,
}

impl<D: ServerDriver + 'static> H2ReadContext<D> {
    fn new(driver: Rc<D>, ev_tx: mpsc::Sender<H2ConnEvent>, state: ConnState) -> Self {
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

    async fn work(
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

            self.process_frame(frame, payload).await?;
        }
    }

    async fn process_frame(&mut self, frame: Frame, payload: Roll) -> eyre::Result<()> {
        match self.continuation_state {
            ContinuationState::Idle => self.process_idle_frame(frame, payload).await,
            ContinuationState::ContinuingHeaders(expected_stream_id) => {
                self.process_continuation_frame(frame, payload, expected_stream_id)
                    .await
            }
        }
    }

    async fn process_idle_frame(&mut self, frame: Frame, mut payload: Roll) -> eyre::Result<()> {
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
                        StreamStage::Body(tx) => {
                            // TODO: we can get rid of that clone sometimes
                            let tx = tx.clone();
                            if flags.contains(DataFlags::EndStream) {
                                *stage = StreamStage::Done;
                            }
                            tx
                        }
                        StreamStage::Done => {
                            // TODO: proper error handling (stream error)
                            panic!("received data for stream after completion");
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

                        let prev_stage = std::mem::replace(stage, StreamStage::Done);
                        match prev_stage {
                            StreamStage::Body(body_tx) => {
                                *stage = StreamStage::Trailers(body_tx, headers_data);
                            }
                            // FIXME: that's a connection error
                            _ => unreachable!(),
                        }
                    }
                    None => {
                        debug!("Receiving headers for stream {}", frame.stream_id);
                        self.state
                            .streams
                            .insert(frame.stream_id, StreamStage::Headers(headers_data));
                    }
                }

                if flags.contains(HeadersFlags::EndHeaders) {
                    self.end_headers(frame.stream_id).await;
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
                        return Err(eyre::eyre!("could not send H2 acknowledge settings event"));
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
                    return Err(eyre::eyre!("could not send H2 ping event"));
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
    ) -> eyre::Result<()> {
        let flags = match frame.frame_type {
            FrameType::Continuation(flags) => flags,
            other => {
                self.send_goaway(H2ConnectionError::ExpectedContinuationFrame {
                    stream_id: expected_stream_id,
                    frame_type: other,
                })
                .await;
                return Ok(());
            }
        };

        if frame.stream_id != expected_stream_id {
            self.send_goaway(H2ConnectionError::ExpectedContinuationForStream {
                stream_id: expected_stream_id,
                continuation_stream_id: frame.stream_id,
            })
            .await;
            return Ok(());
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
            self.end_headers(frame.stream_id).await;
        } else {
            debug!(
                "expecting more headers/trailers for stream {}",
                frame.stream_id
            );
        }
        Ok(())
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
        warn!("stream error: {e:?}");
        let error_code = e.as_known_error_code();
        debug!("error_code = {error_code:?}");

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
    }

    async fn end_headers(&mut self, stream_id: StreamId) {
        if let Err(err) = self.end_headers_inner(stream_id).await {
            match err {
                H2Error::Connection(e) => {
                    self.send_goaway(e).await;
                }
                H2Error::Stream(e) => {
                    self.send_rst(stream_id, e).await;
                }
            }
        }
    }

    async fn end_headers_inner(&mut self, stream_id: StreamId) -> H2Result {
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
        let mut prev_stage = StreamStage::Done;
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
                    StreamStage::Done
                } else {
                    StreamStage::Body(piece_tx)
                };
            }
            StreamStage::Body(_) => unreachable!(),
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
            StreamStage::Done => unreachable!(),
        }

        Ok(())
    }
}

type H2Result<T = ()> = Result<T, H2Error>;

#[derive(Debug, thiserror::Error)]
enum H2Error {
    #[error("connection error: {0:?}")]
    Connection(#[from] H2ConnectionError),

    #[error("stream error: {0:?}")]
    Stream(#[from] H2StreamError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum H2ConnectionError {
    #[error("frame too large: {frame_type:?} frame of size {frame_size} exceeds max frame size of {max_frame_size}")]
    FrameTooLarge {
        frame_type: FrameType,
        frame_size: u32,
        max_frame_size: u32,
    },

    #[error("headers frame had invalid priority: stream {stream_id} depends on itself")]
    HeadersInvalidPriority { stream_id: StreamId },

    #[error("client tried to initiate an even-numbered stream")]
    ClientSidShouldBeOdd,

    #[error("client is trying to initiate stream with ID lower than the last one it initiated")]
    ClientSidShouldBeIncreasing,

    #[error("received frame with Padded flag but empty payload")]
    PaddedFrameEmpty,

    #[error("received frame with Padded flag but payload too short to contain padding length")]
    PaddedFrameTooShort,

    #[error("on stream {stream_id}, expected continuation frame, but got {frame_type:?}")]
    ExpectedContinuationFrame {
        stream_id: StreamId,
        frame_type: FrameType,
    },

    #[error("expected continuation from for stream {stream_id}, but got continuation for stream {continuation_stream_id}")]
    ExpectedContinuationForStream {
        stream_id: StreamId,
        continuation_stream_id: StreamId,
    },

    #[error("on stream {stream_id}, received unexpected continuation frame")]
    UnexpectedContinuationFrame { stream_id: StreamId },

    #[error("compression error: {0:?}")]
    // FIXME: let's not use String, let's just replicate the enum from `fluke-hpack` or fix it?
    CompressionError(String),

    #[error("client sent a push promise frame, clients aren't allowed to do that, cf. RFC9113 section 8.4")]
    ClientSentPushPromise,

    #[error("received window update for unknown stream {stream_id}")]
    WindowUpdateForUnknownStream { stream_id: StreamId },

    #[error("other error: {0:?}")]
    Other(#[from] eyre::Report),
}

impl H2ConnectionError {
    fn as_known_error_code(&self) -> KnownErrorCode {
        use H2ConnectionError::*;
        use KnownErrorCode as Code;

        match self {
            FrameTooLarge { .. } => Code::FrameSizeError,
            HeadersInvalidPriority { .. } => Code::ProtocolError,
            ClientSidShouldBeOdd => Code::ProtocolError,
            ClientSidShouldBeIncreasing => Code::ProtocolError,
            PaddedFrameEmpty => Code::FrameSizeError,
            PaddedFrameTooShort => Code::FrameSizeError,
            ExpectedContinuationFrame { .. } => Code::ProtocolError,
            ExpectedContinuationForStream { .. } => Code::ProtocolError,
            UnexpectedContinuationFrame { .. } => Code::ProtocolError,
            ClientSentPushPromise => Code::ProtocolError,
            CompressionError(_) => Code::CompressionError,
            WindowUpdateForUnknownStream { .. } => Code::ProtocolError,
            Other(_) => Code::InternalError,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum H2StreamError {
    #[allow(dead_code)]
    #[error("for stream {stream_id}, received {data_length} bytes in data frames but content-length announced {content_length} bytes")]
    DataLengthDoesNotMatchContentLength {
        stream_id: StreamId,
        data_length: u64,
        content_length: u64,
    },
}

impl H2StreamError {
    fn as_known_error_code(&self) -> KnownErrorCode {
        use H2StreamError::*;
        use KnownErrorCode as Code;

        match self {
            DataLengthDoesNotMatchContentLength { .. } => Code::ProtocolError,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HeadersOrTrailers {
    Headers,
    Trailers,
}
