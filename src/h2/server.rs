use std::{cell::RefCell, collections::HashMap, rc::Rc};

use enumflags2::BitFlags;
use http::{header::HeaderName, Version};
use nom::Finish;
use pretty_hex::PrettyHex;
use tokio::sync::mpsc;
use tracing::{debug, trace};

use crate::{
    h2::{
        body::H2Body,
        encode::{EncoderState, H2ConnEvent, H2Encoder, H2EventPayload},
        parse::{
            parse_headers_priority, DataFlags, Frame, FrameType, HeadersFlags, PingFlags,
            SettingsFlags, StreamId,
        },
    },
    util::read_and_parse,
    ExpectResponseHeaders, Headers, Method, Piece, PieceStr, ReadWriteOwned, Request, Responder,
    RollMut, ServerDriver,
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

pub(crate) mod parse;

#[derive(Default)]
struct ConnState {
    streams: HashMap<StreamId, StreamState>,
}

struct StreamState {
    rx_stage: StreamRxStage,
}

enum StreamRxStage {
    Headers(Request),
    Body(mpsc::Sender<eyre::Result<Piece>>),
    Done,
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

    // we have to send a settings frame
    {
        let frame = Frame::new(
            FrameType::Settings(Default::default()),
            StreamId::CONNECTION,
        );
        frame.write(transport.as_ref()).await?;
        debug!("sent settings frame");
    }

    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<H2ConnEvent>(32);

    let read_task = {
        let ev_tx = ev_tx.clone();
        let transport = transport.clone();
        let state = state.clone();

        h2_read_loop(ev_tx, transport, client_buf, state)
    };

    let write_task = h2_write_loop(driver, ev_tx, ev_rx, transport, state);
    tokio::try_join!(read_task, write_task)?;
    Ok(())
}

async fn h2_read_loop(
    ev_tx: mpsc::Sender<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
    mut client_buf: RollMut,
    state: Rc<RefCell<ConnState>>,
) -> eyre::Result<()> {
    loop {
        let frame;
        (client_buf, frame) =
            match read_and_parse(Frame::parse, transport.as_ref(), client_buf, 32 * 1024).await? {
                Some((client_buf, frame)) => (client_buf, frame),
                None => {
                    debug!("h2 client closed connection");
                    return Ok(());
                }
            };

        debug!(?frame, "received h2 frame");

        // TODO: there might be optimizations to be done for `Data` frames later
        // on, but for now, let's unconditionally read the payload (if it's not
        // empty).
        let payload = if frame.len == 0 {
            Piece::Static(&[])
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

            payload_roll.into()
        };

        match frame.frame_type {
            FrameType::Data(flags) => {
                if flags.contains(DataFlags::Padded) {
                    todo!("handle padded data frame");
                }

                if (ev_tx.send(H2ConnEvent::ClientFrame(frame, payload)).await).is_err() {
                    return Err(eyre::eyre!("could not send H2 event"));
                }
            }
            FrameType::Headers(flags) => {
                if flags.contains(HeadersFlags::Padded) {
                    todo!("padded headers are not supported");
                }

                {
                    let state = state.borrow();
                    if state.streams.contains_key(&frame.stream_id) {
                        todo!("handle connection error: received headers for existing stream");
                    }
                }

                if (ev_tx.send(H2ConnEvent::ClientFrame(frame, payload)).await).is_err() {
                    return Err(eyre::eyre!("could not send H2 event"));
                }
            }
            FrameType::Priority => todo!(),
            FrameType::RstStream => todo!(),
            FrameType::Settings(s) => {
                if s.contains(SettingsFlags::Ack) {
                    debug!("Peer has acknowledged our settings, cool");
                } else {
                    // TODO: actually apply settings

                    if ev_tx.send(H2ConnEvent::AcknowledgeSettings).await.is_err() {
                        return Err(eyre::eyre!("could not send H2 acknowledge settings event"));
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

                if ev_tx.send(H2ConnEvent::Ping(payload.into())).await.is_err() {
                    return Err(eyre::eyre!("could not send H2 ping event"));
                }
            }
            FrameType::GoAway => todo!(),
            FrameType::WindowUpdate => {
                debug!("ignoring window update");
            }
            FrameType::Continuation => todo!(),
            FrameType::Unknown(ft) => {
                trace!(
                    "ignoring unknown frame with type 0x{:x}, flags 0x{:x}",
                    ft.ty,
                    ft.flags
                );
            }
        }
    }
}

async fn h2_write_loop(
    driver: Rc<impl ServerDriver + 'static>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    mut ev_rx: mpsc::Receiver<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
    state: Rc<RefCell<ConnState>>,
) -> eyre::Result<()> {
    let mut hpack_dec = hpack::Decoder::new();
    let mut hpack_enc = hpack::Encoder::new();

    while let Some(ev) = ev_rx.recv().await {
        match ev {
            H2ConnEvent::AcknowledgeSettings => {
                debug!("acknowledging new settings");
                let res_frame = Frame::new(
                    FrameType::Settings(SettingsFlags::Ack.into()),
                    StreamId::CONNECTION,
                );
                res_frame.write(transport.as_ref()).await?;
            }
            H2ConnEvent::ClientFrame(frame, mut payload) => match frame.frame_type {
                FrameType::Headers(flags) => {
                    if flags.contains(HeadersFlags::Padded) {
                        todo!("padded headers are not supported");
                    }

                    if flags.contains(HeadersFlags::Priority) {
                        let roll = match payload {
                            Piece::Roll(roll) => roll,
                            Piece::Static(_) => unreachable!(),
                            Piece::Vec(_) => unreachable!(),
                            Piece::HeaderName(_) => unreachable!(),
                        };
                        let roll_out;
                        let prio;
                        // TODO: proper error handling (stream error probably)
                        (roll_out, prio) = parse_headers_priority(roll)
                            .finish()
                            .map_err(|err| eyre::eyre!("parsing error: {err:?}"))?;
                        payload = Piece::Roll(roll_out);
                        debug!(exclusive = %prio.exclusive, stream_dependency = ?prio.stream_dependency, weight = %prio.weight, "received priority, exclusive");
                    }

                    let mut path: Option<PieceStr> = None;
                    let mut method: Option<Method> = None;

                    let mut headers = Headers::default();
                    debug!("receiving headers for stream {}", frame.stream_id);

                    hpack_dec
                        .decode_with_cb(&payload[..], |key, value| {
                            debug!(
                                "got key: {:?}\nvalue: {:?}",
                                key.hex_dump(),
                                value.hex_dump()
                            );

                            if &key[..1] == b":" {
                                // it's a pseudo-header!
                                match &key[1..] {
                                    b"path" => {
                                        let value: PieceStr =
                                            Piece::from(value.to_vec()).to_string().unwrap();
                                        path = Some(value);
                                    }
                                    b"method" => {
                                        // TODO: error handling
                                        let value: PieceStr =
                                            Piece::from(value.to_vec()).to_string().unwrap();
                                        method = Some(Method::try_from(value).unwrap());
                                    }
                                    other => {
                                        debug!("ignoring pseudo-header {:?}", other.hex_dump());
                                    }
                                }
                            } else {
                                // TODO: what do we do in case of malformed header names?
                                // ignore it? return a 400?
                                let name = HeaderName::from_bytes(&key[..])
                                    .expect("malformed header name");
                                let value: Piece = value.to_vec().into();
                                headers.append(name, value);
                            }
                        })
                        .map_err(|e| eyre::eyre!("hpack error: {e:?}"))?;

                    // TODO: proper error handling (return 400)
                    let path = path.unwrap();
                    let method = method.unwrap();

                    // TODO: this might not be the end of headers
                    if !flags.contains(HeadersFlags::EndHeaders) {
                        todo!("handle continuation frames");
                    }

                    let req = Request {
                        method,
                        path,
                        version: Version::HTTP_2,
                        headers,
                    };

                    let responder = Responder {
                        encoder: H2Encoder {
                            stream_id: frame.stream_id,
                            tx: ev_tx.clone(),
                            state: EncoderState::ExpectResponseHeaders,
                        },
                        state: ExpectResponseHeaders,
                    };

                    let (piece_tx, piece_rx) = mpsc::channel::<eyre::Result<Piece>>(1);
                    let is_end_stream = flags.contains(HeadersFlags::EndStream);

                    {
                        let mut state = state.borrow_mut();

                        let ss = if is_end_stream {
                            StreamState {
                                rx_stage: StreamRxStage::Done,
                            }
                        } else {
                            StreamState {
                                rx_stage: StreamRxStage::Body(piece_tx),
                            }
                        };
                        state.streams.insert(frame.stream_id, ss);
                    }

                    let req_body = H2Body {
                        // FIXME: that's not right. h2 requests can still specify
                        // a content-length
                        content_length: if is_end_stream { Some(0) } else { None },
                        eof: is_end_stream,
                        rx: piece_rx,
                    };

                    debug!("Calling handler with the given body");
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
                    debug!("Calling handler with the given body... it returned!");
                }
                FrameType::Data(flags) => {
                    if flags.contains(DataFlags::Padded) {
                        todo!("padded data is not supported");
                    }

                    let tx = {
                        let mut state = state.borrow_mut();
                        let stream = state
                            .streams
                            .get_mut(&frame.stream_id)
                            // TODO: proper error handling (connection error)
                            .expect("received data for unknown stream");
                        match &mut stream.rx_stage {
                            StreamRxStage::Headers(_) => {
                                // TODO: proper error handling (stream error)
                                panic!("received data for stream before headers")
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
                    if tx.send(Ok(payload)).await.is_err() {
                        return Err(eyre::eyre!("ev_tx receiver dropped"));
                    }
                }
                _ => {
                    todo!("handle client frame: {frame:?}");
                }
            },
            H2ConnEvent::ServerEvent(ev) => {
                debug!("Sending server event: {ev:?}");

                match ev.payload {
                    H2EventPayload::Headers(res) => {
                        debug!("Sending headers on stream {}", ev.stream_id);
                        let flags = HeadersFlags::EndHeaders;
                        let mut frame = Frame::new(FrameType::Headers(flags.into()), ev.stream_id);

                        // TODO: don't allocate so much for headers
                        // TODO: limt header size
                        let mut headers: Vec<(&[u8], &[u8])> = vec![];
                        headers.push((b":status", res.status.as_str().as_bytes()));
                        for (name, value) in res.headers.iter() {
                            if name == http::header::TRANSFER_ENCODING {
                                // do not set transfer-encoding: chunked when doing HTTP/2
                                continue;
                            }
                            headers.push((name.as_str().as_bytes(), value));
                        }
                        let headers_encoded = hpack_enc.encode(headers);
                        frame.len = headers_encoded.len() as u32;
                        frame.write(transport.as_ref()).await?;

                        let (res, _headers_encoded) = transport.write_all(headers_encoded).await;
                        res?;
                    }
                    H2EventPayload::BodyChunk(piece) => {
                        let flags = BitFlags::<DataFlags>::default();
                        let frame = Frame::new(FrameType::Data(flags), ev.stream_id)
                            .with_len(piece.len().try_into().unwrap());
                        frame.write(transport.as_ref()).await?;
                        let (res, _) = transport.write_all(piece).await;
                        res?;
                    }
                    H2EventPayload::BodyEnd => {
                        let flags = DataFlags::EndStream;
                        let frame = Frame::new(FrameType::Data(flags.into()), ev.stream_id);
                        frame.write(transport.as_ref()).await?;
                    }
                }
            }
            H2ConnEvent::Ping(payload) => {
                // send pong frame
                let flags = PingFlags::Ack.into();
                let frame = Frame::new(FrameType::Ping(flags), StreamId::CONNECTION)
                    .with_len(payload.len() as u32);
                frame.write(transport.as_ref()).await?;
                let (res, _) = transport.write_all(payload).await;
                res?;
            }
        }
    }
    Ok(())
}
