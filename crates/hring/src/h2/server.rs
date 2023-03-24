use std::{collections::HashMap, rc::Rc};

use enumflags2::BitFlags;
use eyre::Context;
use futures_util::TryFutureExt;
use nom::Finish;
use smallvec::{smallvec, SmallVec};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::{
    h2::{
        body::PieceOrTrailers,
        encode::{H2ConnEvent, H2EventPayload},
        end_headers,
        parse::{
            self, ContinuationFlags, DataFlags, Frame, FrameType, HeadersFlags, KnownErrorCode,
            PingFlags, PrioritySpec, SettingsFlags, StreamId,
        },
    },
    util::read_and_parse,
    ServerDriver,
};
use hring_buffet::{Piece, PieceList, Roll, RollMut};
use maybe_uring::io::{ReadOwned, WriteOwned};

use super::{body::H2BodySender, parse::Settings};

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
pub(crate) struct ConnState {
    pub(crate) streams: HashMap<StreamId, StreamState>,
    pub(crate) self_settings: Settings,
    pub(crate) peer_settings: Settings,
}

impl ConnState {
    /// Returns the last stream ID.
    ///
    /// FIXME: this is bad for multiple reasons: it goes through
    /// all streams, and it doesn't care about the direction of the streams.
    pub(crate) fn last_stream_id(&self) -> StreamId {
        self.streams
            .keys()
            .copied()
            .max()
            .unwrap_or(StreamId::CONNECTION)
    }
}

#[derive(Default, Clone, Copy)]
enum ContinuationState {
    #[default]
    Idle,
    ContinuingHeaders(StreamId),
}

pub(crate) struct StreamState {
    pub(crate) rx_stage: StreamRxStage,
}

pub(crate) enum StreamRxStage {
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

    let read_task = h2_read_loop(
        driver.clone(),
        ev_tx.clone(),
        transport_r,
        client_buf,
        state,
    );

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

async fn h2_read_loop(
    driver: Rc<impl ServerDriver + 'static>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    mut transport_r: impl ReadOwned,
    mut client_buf: RollMut,
    mut state: ConnState,
) -> eyre::Result<()> {
    let mut hpack_dec = hring_hpack::Decoder::new();
    hpack_dec.set_max_allowed_table_size(Settings::default().header_table_size.try_into().unwrap());

    let mut continuation_state = ContinuationState::Idle;

    loop {
        let frame;
        (client_buf, frame) =
            match read_and_parse(Frame::parse, &mut transport_r, client_buf, 32 * 1024).await? {
                Some((client_buf, frame)) => (client_buf, frame),
                None => {
                    debug!("h2 client closed connection");
                    return Err(eyre::Report::from(ConnectionClosed));
                }
            };

        debug!(?frame, "Received");

        match &frame.frame_type {
            FrameType::Headers(..) | FrameType::Data(..) => {
                let max_frame_size = state.self_settings.max_frame_size;
                debug!(
                    "frame len = {}, max_frame_size = {max_frame_size}",
                    frame.len
                );
                if frame.len > max_frame_size {
                    let e = eyre::eyre!(
                        "frame too large: {} > {}",
                        frame.len,
                        state.self_settings.max_frame_size
                    );

                    send_goaway(&ev_tx, &state, e, KnownErrorCode::FrameSizeError).await;
                }
            }
            _ => {
                // muffin.
            }
        }

        // TODO: there might be optimizations to be done for `Data` frames later
        // on, but for now, let's unconditionally read the payload (if it's not
        // empty).
        let mut payload: Roll = if frame.len == 0 {
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
                        if frame.stream_id.is_server_initiated() {
                            let e =
                                eyre::eyre!("client is trying to initiate even-numbered stream");
                            send_goaway(&ev_tx, &state, e, KnownErrorCode::ProtocolError).await;
                            continue;
                        }

                        if frame.stream_id.0 < state.last_stream_id().0 {
                            let e =
                                eyre::eyre!("client is trying to initiate stream with id lower than the last one it initiated");
                            send_goaway(&ev_tx, &state, e, KnownErrorCode::ProtocolError).await;
                            continue;
                        }

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

                            if pri_spec.stream_dependency == frame.stream_id {
                                let e = eyre::eyre!("invalid priority: stream depends on itself");
                                send_goaway(&ev_tx, &state, e, KnownErrorCode::ProtocolError).await;
                                continue;
                            }
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

                        let headers_data = HeadersData {
                            end_stream: flags.contains(HeadersFlags::EndStream),
                            fragments: smallvec![payload],
                        };

                        let mut res = None;

                        {
                            match state.streams.get_mut(&frame.stream_id) {
                                Some(ss) => {
                                    debug!("Receiving trailers for stream {}", frame.stream_id);

                                    if !flags.contains(HeadersFlags::EndStream) {
                                        todo!(
                                            "handle connection error: trailers must have EndStream, this just looks like duplicate headers for stream {}",
                                            frame.stream_id
                                        );
                                    }

                                    let stage =
                                        std::mem::replace(&mut ss.rx_stage, StreamRxStage::Done);
                                    match stage {
                                        StreamRxStage::Body(body_tx) => {
                                            ss.rx_stage =
                                                StreamRxStage::Trailers(body_tx, headers_data);
                                        }
                                        // FIXME: that's a connection error
                                        _ => unreachable!(),
                                    }
                                }
                                None => {
                                    debug!("Receiving headers for stream {}", frame.stream_id);
                                    state.streams.insert(
                                        frame.stream_id,
                                        StreamState {
                                            rx_stage: StreamRxStage::Headers(headers_data),
                                        },
                                    );
                                }
                            }

                            if flags.contains(HeadersFlags::EndHeaders) {
                                res = Some(end_headers::end_headers(
                                    &ev_tx,
                                    frame.stream_id,
                                    &mut state,
                                    &driver,
                                    &mut hpack_dec,
                                ));
                            } else {
                                debug!(
                                    "expecting more headers/trailers for stream {}",
                                    frame.stream_id
                                );
                                continuation_state =
                                    ContinuationState::ContinuingHeaders(frame.stream_id);
                            }
                        };

                        if let Some(res) = res {
                            res.finalize().await?;
                        }
                    }
                    FrameType::Priority => {
                        let pri_spec = match PrioritySpec::parse(payload) {
                            Ok((_rest, pri_spec)) => pri_spec,
                            Err(e) => todo!("handle connection error: invalid priority frame {e}"),
                        };
                        debug!(?pri_spec, "received priority frame");

                        if pri_spec.stream_dependency == frame.stream_id {
                            let e = eyre::eyre!("invalid priority: stream depends on itself");
                            let goaway =
                                send_goaway(&ev_tx, &state, e, KnownErrorCode::ProtocolError);
                            goaway.await;
                            continue;
                        }
                    }
                    FrameType::RstStream => todo!(),
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
                            state.peer_settings = settings;

                            if ev_tx
                                .send(H2ConnEvent::AcknowledgeSettings {
                                    new_max_header_table_size,
                                })
                                .await
                                .is_err()
                            {
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
                        let goaway = send_goaway(
                            &ev_tx,
                            &state,
                            eyre::eyre!(
                                "received unexpected continuation frame for stream {}",
                                frame.stream_id
                            ),
                            KnownErrorCode::ProtocolError,
                        );
                        goaway.await
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
            ContinuationState::ContinuingHeaders(expected_stream_id) => match frame.frame_type {
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

                    let mut res = None;

                    {
                        // unwrap rationale: we just checked that this is a
                        // continuation of a stream we've already learned about.
                        let ss = state.streams.get_mut(&frame.stream_id).unwrap();
                        match &mut ss.rx_stage {
                            StreamRxStage::Headers(data) | StreamRxStage::Trailers(_, data) => {
                                data.fragments.push(payload);
                            }
                            _ => {
                                send_goaway(
                                    &ev_tx,
                                    &state,
                                    eyre::eyre!("continuation frame for wrong stream"),
                                    KnownErrorCode::ProtocolError,
                                )
                                .await;
                                continue;
                            }
                        }

                        if flags.contains(ContinuationFlags::EndHeaders) {
                            res = Some(end_headers::end_headers(
                                &ev_tx,
                                frame.stream_id,
                                &mut state,
                                &driver,
                                &mut hpack_dec,
                            ));
                        } else {
                            debug!(
                                "expecting more headers/trailers for stream {}",
                                frame.stream_id
                            );
                        }
                    }

                    if let Some(res) = res {
                        res.finalize().await?;
                    }
                }
                other => {
                    let goaway = send_goaway(
                        &ev_tx,
                        &state,
                        eyre::eyre!("expected continuation frame, got {:?}", other),
                        KnownErrorCode::ProtocolError,
                    );
                    goaway.await
                }
            },
        }
    }
}

pub(crate) async fn send_goaway(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    state: &ConnState,
    e: eyre::Report,
    error_code: KnownErrorCode,
) {
    let last_stream_id = state.last_stream_id();
    send_goaway_inner(ev_tx, last_stream_id, e, error_code).await
}

pub(crate) async fn send_goaway_inner(
    ev_tx: &mpsc::Sender<H2ConnEvent>,
    last_stream_id: StreamId,
    e: eyre::Report,
    error_code: KnownErrorCode,
) {
    warn!("connection error: {e:?}");
    debug!("error_code = {error_code:?}");

    // TODO: is this a good idea?
    let additional_debug_data = format!("{e:?}").into_bytes();

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
    mut transport_w: impl WriteOwned,
    mut out_scratch: RollMut,
) -> eyre::Result<()> {
    let mut hpack_enc = hring_hpack::Encoder::new();

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
        }
    }

    Ok(())
}
