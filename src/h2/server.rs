use std::{collections::HashMap, rc::Rc};

use http::{header::HeaderName, Version};
use pretty_hex::PrettyHex;
use tokio::sync::mpsc;
use tracing::debug;

use crate::{
    h2::{
        body::H2Body,
        encode::{H2ConnEvent, H2Encoder, H2EventPayload},
        parse::{DataFlags, Frame, FrameType, HeadersFlags, SettingsFlags, StreamId},
    },
    util::read_and_parse,
    ExpectResponseHeaders, Headers, Method, Piece, PieceStr, ReadWriteOwned, Request, Responder,
    Roll, RollMut, ServerDriver,
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
    Headers(Headers),
    Body(mpsc::Sender<eyre::Result<Roll>>),
}

pub async fn serve(
    transport: impl ReadWriteOwned,
    conf: Rc<ServerConf>,
    mut client_buf: RollMut,
    driver: Rc<impl ServerDriver + 'static>,
) -> eyre::Result<()> {
    debug!("TODO: enforce max_streams {}", conf.max_streams);

    let state = ConnState::default();

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

        h2_read_loop(ev_tx, transport, client_buf, state)
    };

    let write_task = h2_write_loop(driver, ev_tx, ev_rx, transport);
    tokio::try_join!(read_task, write_task)?;
    Ok(())
}

async fn h2_read_loop(
    ev_tx: mpsc::Sender<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
    mut client_buf: RollMut,
    state: ConnState,
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

        match frame.frame_type {
            FrameType::Data(flags) => {
                if flags.contains(DataFlags::Padded) {
                    todo!("handle padded data frame");
                }

                let mut remain = frame.len as usize;
                while remain > 0 {
                    debug!("reading frame payload, {remain} remains");

                    if client_buf.is_empty() {
                        let res;
                        (res, client_buf) = client_buf.read_into(remain, transport.as_ref()).await;
                        let n = res?;
                        debug!("read {n} bytes for frame payload");
                    }
                    let roll = client_buf.take_at_most(remain).unwrap();
                    debug!("read {} of frame payload", roll.len());
                    remain -= roll.len();
                }
                debug!("done reading frame payload");

                // TODO: handle `EndStream`
            }
            FrameType::Headers(flags) => {
                if flags.contains(HeadersFlags::Padded) {
                    todo!("padded headers are not supported");
                }

                if state.streams.contains_key(&frame.stream_id) {
                    todo!("handle connection error: received headers for existing stream");
                }

                let payload;
                (client_buf, payload) = match read_and_parse(
                    nom::bytes::streaming::take(frame.len as usize),
                    transport.as_ref(),
                    client_buf,
                    frame.len as usize,
                )
                .await?
                {
                    Some((client_buf, frame)) => (client_buf, frame),
                    None => {
                        debug!("h2 client closed connection while reading settings body");
                        return Ok(());
                    }
                };

                if (ev_tx
                    .send(H2ConnEvent::ClientFrame(frame, payload.into()))
                    .await)
                    .is_err()
                {
                    return Err(eyre::eyre!("could not send H2 event"));
                }

                if !flags.contains(HeadersFlags::EndHeaders) {
                    todo!("handle continuation frames");
                }

                if flags.contains(HeadersFlags::EndStream) {
                    todo!("handle requests with no request body");
                }

                // at this point we've fully read the headers, we can start
                // reading the body.
            }
            FrameType::Priority => todo!(),
            FrameType::RstStream => todo!(),
            FrameType::Settings(s) => {
                let _payload;
                (client_buf, _payload) = match read_and_parse(
                    nom::bytes::streaming::take(frame.len as usize),
                    transport.as_ref(),
                    client_buf,
                    frame.len as usize,
                )
                .await?
                {
                    Some((client_buf, frame)) => (client_buf, frame),
                    None => {
                        debug!("h2 client closed connection while reading settings body");
                        return Ok(());
                    }
                };

                if s.contains(SettingsFlags::Ack) {
                    debug!("Peer has acknowledge our settings, cool");
                } else {
                    // TODO: actually apply settings

                    if ev_tx.send(H2ConnEvent::AcknowledgeSettings).await.is_err() {
                        return Err(eyre::eyre!("could not send H2 event"));
                    }
                }
            }
            FrameType::PushPromise => todo!("push promise not implemented"),
            FrameType::Ping => todo!(),
            FrameType::GoAway => todo!(),
            FrameType::WindowUpdate => {
                debug!("ignoring window update");

                let _payload;
                (client_buf, _payload) = match read_and_parse(
                    nom::bytes::streaming::take(frame.len as usize),
                    transport.as_ref(),
                    client_buf,
                    frame.len as usize,
                )
                .await?
                {
                    Some((client_buf, frame)) => (client_buf, frame),
                    None => {
                        debug!("h2 client closed connection while reading window update body");
                        return Ok(());
                    }
                };
            }
            FrameType::Continuation => todo!(),
        }
    }
}

async fn h2_write_loop(
    driver: Rc<impl ServerDriver + 'static>,
    ev_tx: mpsc::Sender<H2ConnEvent>,
    mut ev_rx: mpsc::Receiver<H2ConnEvent>,
    transport: Rc<impl ReadWriteOwned>,
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
            H2ConnEvent::ClientFrame(frame, payload) => match frame.frame_type {
                FrameType::Headers(flags) => {
                    if flags.contains(HeadersFlags::Padded) {
                        todo!("padded headers are not supported");
                    }
                    if !flags.contains(HeadersFlags::EndHeaders) {
                        todo!("handle continuation frames");
                    }
                    if flags.contains(HeadersFlags::EndStream) {
                        todo!("handle requests with no request body");
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
                        },
                        state: ExpectResponseHeaders,
                    };

                    // TODO: send received pieces somewhere
                    let (_piece_tx, piece_rx) = mpsc::channel::<Piece>(1);

                    let req_body = H2Body {
                        // FIXME: that's not right
                        content_length: None,
                        eof: false,
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
                                    // TODO: actually handle that error
                                    debug!("Handler returned an error: {e:?}")
                                }
                            }
                        }
                    });
                    debug!("Calling handler with the given body... it returned!");
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
                            headers.push((name.as_str().as_bytes(), value));
                        }
                        let headers_encoded = hpack_enc.encode(headers);
                        frame.len = headers_encoded.len() as u32;
                        frame.write(transport.as_ref()).await?;

                        let (res, _headers_encoded) = transport.write_all(headers_encoded).await;
                        res?;
                    }
                    H2EventPayload::BodyChunk(_) => todo!("send body chunk"),
                    H2EventPayload::BodyEnd => todo!("end body"),
                }
            }
        }
    }
    Ok(())
}
