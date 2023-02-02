use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

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
        body::{H2Body, PieceOrTrailers},
        encode::{EncoderState, H2ConnEvent, H2Encoder, H2EventPayload},
        parse::{
            self, ContinuationFlags, DataFlags, Frame, FrameType, HeadersFlags, KnownErrorCode,
            PingFlags, PrioritySpec, SettingsFlags, StreamId,
        },
    },
    util::read_and_parse,
    ExpectResponseHeaders, Headers, Method, Request, Responder, ServerDriver,
};
use hring_buffet::{Piece, PieceList, PieceStr, ReadWriteOwned, Roll, RollMut};

use super::body::{H2BodyItem, H2BodySender};

/// HTTP/2 server configuration
pub struct ServerConf {
    pub max_streams: u32,
}

impl Default for ServerConf {
    fn default() -> Self {
        Self { max_streams: 32 }
    }
}

#[derive(Default)]
struct ConnState {
    streams: HashMap<StreamId, StreamState>,
}

#[derive(Default, Clone, Copy)]
enum ContinuationState {
    #[default]
    Idle,
    ContinuingHeaders(StreamId),
}

struct StreamState {
    rx_stage: StreamRxStage,
}

enum StreamRxStage {
    Headers(HeadersData),
    Body(H2BodySender),
    Trailers(H2BodySender, HeadersData),
    Done,
}

struct HeadersData {
    /// If true, no DATA frames follow, cf. https://httpwg.org/specs/rfc9113.html#HttpFraming
    end_stream: bool,

    /// The field block fragments
    fragments: SmallVec<[Roll; 2]>,
}

pub async fn serve(
    transport: impl ReadWriteOwned,
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: Rc<impl ServerDriver + 'static>,
) -> eyre::Result<()> {
    debug!("TODO: enforce max_streams {}", conf.max_streams);

    let state = ConnState::default();
    let state = Rc::new(RefCell::new(state));

    const MAX_FRAME_LEN: usize = 64 * 1024;

    let transport = Rc::new(transport);

    (client_buf, _) = match read_and_parse(
        parse::preface,
        transport.as_ref(),
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
        let frame = Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        );
        transport
            .write_all(frame.into_roll(&mut out_scratch)?)
            .await?;
        debug!("sent settings frame");
    }

    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<H2ConnEvent>(32);

    let read_task = h2_read_loop(
        driver.clone(),
        ev_tx.clone(),
        transport.clone(),
        client_buf,
        state.clone(),
    );

    let write_task = h2_write_loop(ev_rx, transport, out_scratch);

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

async fn h2_read_loop(
    driver: Rc<impl ServerDriver + 'static>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
    mut client_buf: RollMut,
    state: Rc<RefCell<ConnState>>,
) -> eyre::Result<()> {
    let mut hpack_dec = hring_hpack::Decoder::new();
    let mut continuation_state = ContinuationState::Idle;

    loop {
        let frame;
        (client_buf, frame) =
            match read_and_parse(Frame::parse, transport.as_ref(), client_buf, 32 * 1024).await? {
                Some((client_buf, frame)) => (client_buf, frame),
                None => {
                    debug!("h2 client closed connection");
                    return Err(eyre::Report::from(ConnectionClosed));
                }
            };

        debug!(?frame, "Received");

        // TODO: there might be optimizations to be done for `Data` frames later
        // on, but for now, let's unconditionally read the payload (if it's not
        // empty).
        let mut payload: Roll = if frame.len == 0 {
            Roll::empty()
        } else {
            let payload_roll;
            (client_buf, payload_roll) = match read_and_parse(
                nom::bytes::streaming::take(frame.len as usize),
                transport.as_ref(),
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

        match continuation_state {
            ContinuationState::Idle => {
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
                            let mut state = state.borrow_mut();
                            let stream = state
                                .streams
                                .get_mut(&frame.stream_id)
                                // TODO: proper error handling (connection error)
                                .expect("received data for unknown stream");
                            match &mut stream.rx_stage {
                                StreamRxStage::Headers(_) => {
                                    // TODO: proper error handling (stream error)
                                    panic!("expected headers, received data")
                                }
                                StreamRxStage::Trailers(..) => {
                                    // TODO: proper error handling (stream error)
                                    panic!("expected trailers, received data")
                                }
                                StreamRxStage::Body(tx) => {
                                    // TODO: we can get rid of that clone sometimes
                                    let tx = tx.clone();
                                    if flags.contains(DataFlags::EndStream) {
                                        stream.rx_stage = StreamRxStage::Done;
                                    }
                                    tx
                                }
                                StreamRxStage::Done => {
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
                        let headers_or_trailers;
                        {
                            let state = state.borrow();
                            // TODO: validate state a little better than that?
                            if state.streams.contains_key(&frame.stream_id) {
                                if !flags.contains(HeadersFlags::EndStream) {
                                    todo!(
                                        "handle connection error: trailers must have EndStream, this just looks like duplicate headers for stream {}",
                                        frame.stream_id
                                    );
                                }

                                headers_or_trailers = HeadersOrTrailers::Trailers;
                            } else {
                                headers_or_trailers = HeadersOrTrailers::Headers;
                            }
                        };

                        let padding_length = if flags.contains(HeadersFlags::Padded) {
                            if payload.is_empty() {
                                todo!("handle connection error: padded headers frame, but no padding length");
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
                        }

                        if padding_length > 0 {
                            if payload.len() < padding_length {
                                todo!(
                            "handle connection error: padded headers frame, but not enough padding"
                        );
                            }

                            let at = payload.len() - padding_length;
                            (payload, _) = payload.split_at(at);
                        }

                        debug!(
                            "Receiving {headers_or_trailers:?} for stream {}",
                            frame.stream_id
                        );
                        let headers_data = HeadersData {
                            end_stream: flags.contains(HeadersFlags::EndStream),
                            fragments: smallvec![payload],
                        };

                        if flags.contains(HeadersFlags::EndHeaders) {
                            match end_headers(
                                &ev_tx,
                                frame.stream_id,
                                &headers_data,
                                &driver,
                                &mut hpack_dec,
                                headers_or_trailers,
                            ) {
                                Err(e) => {
                                    // TODO: we should also reset the stream here, right?
                                    send_goaway(
                                        &ev_tx,
                                        &state,
                                        e,
                                        KnownErrorCode::CompressionError,
                                    )
                                    .await;
                                }
                                Ok(ehr) => {
                                    if let Some(trailers) = ehr.trailers {
                                        let body_tx = {
                                            let mut state = state.borrow_mut();
                                            let stream =
                                                state.streams.get_mut(&frame.stream_id).unwrap();

                                            let mut stage = StreamRxStage::Done;
                                            std::mem::swap(&mut stage, &mut stream.rx_stage);
                                            match stage {
                                                StreamRxStage::Body(sender) => sender,
                                                _ => unreachable!(),
                                            }
                                        };
                                        if body_tx
                                            .send(Ok(PieceOrTrailers::Trailers(trailers)))
                                            .await
                                            .is_err()
                                        {
                                            warn!("TODO: The body is being ignored, we should reset the stream");
                                        }
                                    } else {
                                        let mut state = state.borrow_mut();
                                        state.streams.insert(
                                            frame.stream_id,
                                            StreamState {
                                                rx_stage: ehr.next_stage,
                                            },
                                        );
                                    }
                                }
                            }
                        } else {
                            debug!(
                                "expecting more {headers_or_trailers:?} for stream {}",
                                frame.stream_id
                            );
                            continuation_state =
                                ContinuationState::ContinuingHeaders(frame.stream_id);

                            {
                                let mut state = state.borrow_mut();
                                match headers_or_trailers {
                                    HeadersOrTrailers::Headers => {
                                        state.streams.insert(
                                            frame.stream_id,
                                            StreamState {
                                                rx_stage: StreamRxStage::Headers(headers_data),
                                            },
                                        );
                                    }
                                    HeadersOrTrailers::Trailers => {
                                        let ss = state
                                            .streams
                                            .get_mut(&frame.stream_id)
                                            .expect("stream should exist when receiving trailers");
                                        let mut rx_stage = StreamRxStage::Done;
                                        std::mem::swap(&mut rx_stage, &mut ss.rx_stage);

                                        let body_tx = match rx_stage {
                                            StreamRxStage::Body(tx) => tx,
                                            _ => todo!("handle connection error: trailers must follow headers"),
                                        };
                                        ss.rx_stage =
                                            StreamRxStage::Trailers(body_tx, headers_data);
                                    }
                                }
                            }
                        }
                    }
                    FrameType::Priority => {
                        let pri_spec = match PrioritySpec::parse(payload) {
                            Ok((_rest, pri_spec)) => pri_spec,
                            Err(e) => todo!("handle connection error: invalid priority frame {e}"),
                        };
                        debug!(?pri_spec, "received priority frame");
                    }
                    FrameType::RstStream => todo!(),
                    FrameType::Settings(s) => {
                        if s.contains(SettingsFlags::Ack) {
                            debug!("Peer has acknowledged our settings, cool");
                        } else {
                            // TODO: actually apply settings

                            if ev_tx.send(H2ConnEvent::AcknowledgeSettings).await.is_err() {
                                return Err(eyre::eyre!(
                                    "could not send H2 acknowledge settings event"
                                ));
                            }
                        }
                    }
                    FrameType::PushPromise => todo!("push promise not implemented"),
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
                            continue;
                        }

                        if ev_tx.send(H2ConnEvent::Ping(payload)).await.is_err() {
                            return Err(eyre::eyre!("could not send H2 ping event"));
                        }
                    }
                    FrameType::GoAway => todo!(),
                    FrameType::WindowUpdate => {
                        debug!("ignoring window update");
                    }
                    FrameType::Continuation(_flags) => {
                        send_goaway(
                            &ev_tx,
                            &state,
                            eyre::eyre!(
                                "received unexpected continuation frame for stream {}",
                                frame.stream_id
                            ),
                            KnownErrorCode::ProtocolError,
                        )
                        .await
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
            ContinuationState::ContinuingHeaders(expected_stream_id) => {
                match frame.frame_type {
                    FrameType::Continuation(flags) => {
                        if frame.stream_id != expected_stream_id {
                            send_goaway(
                                &ev_tx,
                                &state,
                                eyre::eyre!("continuation frame for wrong stream"),
                                KnownErrorCode::ProtocolError,
                            )
                            .await;
                            continue;
                        }

                        let res = {
                            let mut state = state.borrow_mut();
                            let ss = match state.streams.get_mut(&frame.stream_id) {
                                Some(stream) => stream,
                                None => {
                                    todo!("handle connection error: continuation frame for unknown stream");
                                }
                            };
                            match &mut ss.rx_stage {
                                StreamRxStage::Headers(headers_data) => {
                                    headers_data.fragments.push(payload);
                                    if flags.contains(ContinuationFlags::EndHeaders) {
                                        end_headers(
                                            &ev_tx,
                                            frame.stream_id,
                                            headers_data,
                                            &driver,
                                            &mut hpack_dec,
                                            HeadersOrTrailers::Headers,
                                        )
                                        .map(Some)
                                    } else {
                                        debug!(
                                            "have {} field block fragments so far, will read CONTINUATION frames for headers",
                                            headers_data.fragments.len()
                                        );
                                        Ok(None)
                                    }
                                }
                                StreamRxStage::Trailers(_body_tx, header_data) => {
                                    header_data.fragments.push(payload);
                                    if flags.contains(ContinuationFlags::EndHeaders) {
                                        end_headers(
                                            &ev_tx,
                                            frame.stream_id,
                                            header_data,
                                            &driver,
                                            &mut hpack_dec,
                                            HeadersOrTrailers::Trailers,
                                        )
                                        .map(Some)
                                    } else {
                                        debug!(
                                            "have {} field block fragments so far, will read CONTINUATION frames for trailers",
                                            header_data.fragments.len()
                                        );
                                        Ok(None)
                                    }
                                }
                                _ => {
                                    todo!("handle connection error: continuation frame for non-headers stream");
                                }
                            }
                        };

                        match res {
                            Err(e) => {
                                // TODO: we should also reset the stream state here, right?
                                send_goaway(&ev_tx, &state, e, KnownErrorCode::CompressionError)
                                    .await;
                            }
                            Ok(Some(ehr)) => {
                                // we're not reading continuation frames anymore
                                continuation_state = ContinuationState::Idle;

                                if let Some(trailers) = ehr.trailers {
                                    let body_tx = {
                                        let mut state = state.borrow_mut();
                                        let stream =
                                            state.streams.get_mut(&frame.stream_id).unwrap();

                                        let mut stage = StreamRxStage::Done;
                                        std::mem::swap(&mut stage, &mut stream.rx_stage);
                                        match stage {
                                            StreamRxStage::Trailers(sender, _) => sender,
                                            _ => unreachable!(),
                                        }
                                    };
                                    if body_tx
                                        .send(Ok(PieceOrTrailers::Trailers(trailers)))
                                        .await
                                        .is_err()
                                    {
                                        warn!("TODO: The body is being ignored, we should reset the stream");
                                    }
                                } else {
                                    let mut state = state.borrow_mut();
                                    let ss = state.streams.get_mut(&frame.stream_id).unwrap();
                                    ss.rx_stage = ehr.next_stage;
                                }
                            }
                            _ => {
                                // don't care
                            }
                        }
                    }
                    other => {
                        send_goaway(
                            &ev_tx,
                            &state,
                            eyre::eyre!("expected continuation frame, got {:?}", other),
                            KnownErrorCode::ProtocolError,
                        )
                        .await
                    }
                }
            }
        }
    }
}

async fn send_goaway(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    state: &Rc<RefCell<ConnState>>,
    e: eyre::Report,
    error_code: KnownErrorCode,
) {
    warn!("connection error: {e:?}");
    debug!("error_code = {error_code:?}");

    // FIXME: this is almost definitely wrong. we must separate
    // client-initiated streams from server-initiated streams, and
    // take stream state into account
    let last_stream_id = state
        .borrow()
        .streams
        .keys()
        .copied()
        .max()
        .unwrap_or(StreamId::CONNECTION);
    debug!("last_stream_id = {last_stream_id}");
    // TODO: is this a good idea?
    let additional_debug_data = format!("hpack error: {e:?}").into_bytes();

    if ev_tx
        .send(H2ConnEvent::GoAway {
            error_code,
            last_stream_id,
            additional_debug_data: Piece::Vec(additional_debug_data),
        })
        .await
        .is_err()
    {
        debug!("error sending goaway");
    }
}

async fn h2_write_loop(
    mut ev_rx: mpsc::Receiver<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
    mut out_scratch: RollMut,
) -> eyre::Result<()> {
    let mut hpack_enc = hring_hpack::Encoder::new();

    while let Some(ev) = ev_rx.recv().await {
        trace!("h2_write_loop: received H2 event");
        match ev {
            H2ConnEvent::AcknowledgeSettings => {
                debug!("acknowledging new settings");
                let frame = Frame::new(
                    FrameType::Settings(SettingsFlags::Ack.into()),
                    StreamId::CONNECTION,
                );
                transport
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

                        transport
                            .writev_all(PieceList::default().with(frame_roll).with(fragment_block))
                            .await
                            .wrap_err("writing headers")?;
                    }
                    H2EventPayload::BodyChunk(chunk) => {
                        let flags = BitFlags::<DataFlags>::default();
                        let frame = Frame::new(FrameType::Data(flags), ev.stream_id)
                            .with_len(chunk.len().try_into().unwrap());
                        let frame_roll = frame.into_roll(&mut out_scratch)?;
                        transport
                            .writev_all(PieceList::default().with(frame_roll).with(chunk))
                            .await
                            .wrap_err("writing bodychunk")?;
                    }
                    H2EventPayload::BodyEnd => {
                        let flags = DataFlags::EndStream;
                        let frame = Frame::new(FrameType::Data(flags.into()), ev.stream_id);
                        transport
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
                transport
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(payload),
                    )
                    .await
                    .wrap_err("writing pong")?;
            }
            H2ConnEvent::GoAway {
                error_code,
                last_stream_id,
                additional_debug_data,
            } => {
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

                transport
                    .writev_all(
                        PieceList::default()
                            .with(frame.into_roll(&mut out_scratch)?)
                            .with(header)
                            .with(additional_debug_data),
                    )
                    .await
                    .wrap_err("writing goaway")?;
            }
        }
    }

    Ok(())
}

// FIXME: this function is horrible to use, we can do better.  It should have a
// `&mut State` and do whatever writes to the state are needed, then return a
// struct that has an async method that does.. whatever async stuff is needed,
// such as:
//   - Sending a GOAWAY
//   - Sending trailers to [H2Body]
//
// This will reduce code duplication between the "immediate EndHeaders" and
// "continuation frames" codepaths and make everything a lot less confusing
fn end_headers(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    stream_id: StreamId,
    data: &HeadersData,
    driver: &Rc<impl ServerDriver + 'static>,
    hpack_dec: &mut hring_hpack::Decoder,
    headers_or_trailers: HeadersOrTrailers,
) -> eyre::Result<EndHeadersResult> {
    let mut method: Option<Method> = None;
    let mut scheme: Option<Scheme> = None;
    let mut path: Option<PieceStr> = None;
    let mut authority: Option<Authority> = None;

    let mut headers = Headers::default();

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
            hpack_dec
                .decode_with_cb(&payload[..], on_header_pair)
                .map_err(|e| eyre::eyre!("hpack error: {e:?}"))?;
        }
    };

    let mut ehr = EndHeadersResult {
        stream_id,
        next_stage: StreamRxStage::Done,
        trailers: None,
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
                    tx: ev_tx.clone(),
                    state: EncoderState::ExpectResponseHeaders,
                },
                state: ExpectResponseHeaders,
            };

            let (piece_tx, piece_rx) = mpsc::channel::<H2BodyItem>(1); // TODO: is 1 a sensible value here?

            ehr.next_stage = if data.end_stream {
                StreamRxStage::Done
            } else {
                StreamRxStage::Body(piece_tx)
            };

            let req_body = H2Body {
                // FIXME: that's not right. h2 requests can still specify
                // a content-length
                content_length: if data.end_stream { Some(0) } else { None },
                eof: data.end_stream,
                rx: piece_rx,
            };

            tokio_uring::spawn({
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
        }
        HeadersOrTrailers::Trailers => {
            ehr.trailers = Some(Box::new(headers));
        }
    }

    Ok(ehr)
}

struct EndHeadersResult {
    #[allow(dead_code)]
    stream_id: StreamId,
    next_stage: StreamRxStage,
    trailers: Option<Box<Headers>>,
}

#[derive(Debug, Clone, Copy)]
enum HeadersOrTrailers {
    Headers,
    Trailers,
}
