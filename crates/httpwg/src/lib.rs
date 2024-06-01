use eyre::eyre;
use multimap::MultiMap;
use rfc9113::DEFAULT_FRAME_SIZE;
use std::{rc::Rc, time::Duration};

use enumflags2::{bitflags, BitFlags};
use fluke_buffet::{IntoHalves, Piece, PieceList, Roll, RollMut, WriteOwned};
use fluke_h2_parse::{
    enumflags2,
    nom::{self, Finish},
    DataFlags, Frame, FrameType, GoAway, HeadersFlags, IntoPiece, KnownErrorCode, PingFlags,
    RstStream, Settings, SettingsFlags, StreamId, PREFACE,
};
use tokio::time::Instant;
use tracing::{debug, trace};

use crate::rfc9113::default_settings;

pub mod rfc9113;

pub type Headers = MultiMap<Piece, Piece>;

pub struct Conn<IO: IntoHalves + 'static> {
    w: <IO as IntoHalves>::Write,
    scratch: RollMut,
    pub ev_rx: tokio::sync::mpsc::Receiver<Ev>,
    config: Rc<Config>,
    hpack_enc: fluke_hpack::Encoder<'static>,
    // FIXME: we should decode header fragments for _all_ HEADERS frames,
    // even the ones we discard. I don't know if test cases should be
    // responsible for that, or the connection itself.
    hpack_dec: fluke_hpack::Decoder<'static>,
    pub max_frame_size: usize,
}

pub enum Ev {
    Frame { frame: Frame, payload: Roll },
    IoError { error: std::io::Error },
}

pub enum FrameWaitOutcome {
    Success(Frame, Roll),
    Timeout {
        wanted: BitFlags<FrameT>,
        last_frame: Option<Frame>,
        waited: Duration,
    },
    Eof {
        wanted: BitFlags<FrameT>,
        last_frame: Option<Frame>,
    },
    IoError {
        wanted: BitFlags<FrameT>,
        last_frame: Option<Frame>,
        error: std::io::Error,
    },
}

impl FrameWaitOutcome {
    pub fn unwrap(self) -> (Frame, Roll) {
        match self {
            FrameWaitOutcome::Success(frame, payload) => (frame, payload),
            FrameWaitOutcome::Timeout {
                wanted,
                last_frame,
                waited,
            } => {
                panic!(
                    "Wanted ({wanted:?}), timed out after {waited:?}. Last frame: {last_frame:?}"
                );
            }
            FrameWaitOutcome::Eof { wanted, last_frame } => {
                panic!("Wanted ({wanted:?}), peer hung up. Last frame: {last_frame:?}");
            }
            FrameWaitOutcome::IoError {
                wanted,
                last_frame,
                error,
            } => {
                panic!("Wanted ({wanted:?}), got I/O error {error}. Last frame: {last_frame:?}")
            }
        }
    }
}

/// A "hollow" variant of [FrameType], with no associated data.
/// Useful to expect a certain frame type
#[bitflags]
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameT {
    Data,
    Headers,
    Priority,
    RstStream,
    Settings,
    PushPromise,
    Ping,
    GoAway,
    WindowUpdate,
    Continuation,
    Unknown,
}

impl From<FrameType> for FrameT {
    fn from(value: FrameType) -> Self {
        match value {
            FrameType::Data(_) => Self::Data,
            FrameType::Headers(_) => Self::Headers,
            FrameType::Priority => Self::Priority,
            FrameType::RstStream => Self::RstStream,
            FrameType::Settings(_) => Self::Settings,
            FrameType::PushPromise => Self::PushPromise,
            FrameType::Ping(_) => Self::Ping,
            FrameType::GoAway => Self::GoAway,
            FrameType::WindowUpdate => Self::WindowUpdate,
            FrameType::Continuation(_) => Self::Continuation,
            FrameType::Unknown(_) => Self::Unknown,
        }
    }
}

// A hollow variant of [KnownErrorCode]
#[bitflags]
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ErrorC {
    NoError,
    ProtocolError,
    InternalError,
    FlowControlError,
    SettingsTimeout,
    StreamClosed,
    FrameSizeError,
    RefusedStream,
    Cancel,
    CompressionError,
    ConnectError,
    EnhanceYourCalm,
    InadequateSecurity,
    Http1_1Required,
}

impl From<KnownErrorCode> for ErrorC {
    fn from(value: KnownErrorCode) -> Self {
        match value {
            KnownErrorCode::NoError => Self::NoError,
            KnownErrorCode::ProtocolError => Self::ProtocolError,
            KnownErrorCode::InternalError => Self::InternalError,
            KnownErrorCode::FlowControlError => Self::FlowControlError,
            KnownErrorCode::SettingsTimeout => Self::SettingsTimeout,
            KnownErrorCode::StreamClosed => Self::StreamClosed,
            KnownErrorCode::FrameSizeError => Self::FrameSizeError,
            KnownErrorCode::RefusedStream => Self::RefusedStream,
            KnownErrorCode::Cancel => Self::Cancel,
            KnownErrorCode::CompressionError => Self::CompressionError,
            KnownErrorCode::ConnectError => Self::ConnectError,
            KnownErrorCode::EnhanceYourCalm => Self::EnhanceYourCalm,
            KnownErrorCode::InadequateSecurity => Self::InadequateSecurity,
            KnownErrorCode::Http1_1Required => Self::Http1_1Required,
        }
    }
}

impl<IO: IntoHalves> Conn<IO> {
    pub fn new(config: Rc<Config>, io: IO) -> Self {
        let (mut r, w) = io.into_halves();

        let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<Ev>(1);
        let mut eof = false;
        let recv_fut = async move {
            let mut res_buf = RollMut::alloc()?;
            'read: loop {
                if !eof {
                    res_buf.reserve()?;
                    let res;
                    (res, res_buf) = res_buf.read_into(16384, &mut r).await;
                    let n = res?;
                    if n == 0 {
                        debug!("reached EOF");
                        eof = true;
                    } else {
                        trace!(%n, "read bytes (reading frame header)");
                    }
                }

                if eof && res_buf.is_empty() {
                    break 'read;
                }

                match Frame::parse(res_buf.filled()) {
                    Ok((rest, frame)) => {
                        res_buf.keep(rest);
                        debug!("< {frame:?}");

                        // read frame payload
                        let frame_len = frame.len as usize;
                        res_buf.reserve_at_least(frame_len)?;

                        while res_buf.len() < frame_len {
                            let res;
                            (res, res_buf) = res_buf.read_into(16384, &mut r).await;
                            let n = res?;
                            trace!(%n, len = %res_buf.len(), "read bytes (reading frame payload)");

                            if n == 0 {
                                eof = true;
                                if res_buf.len() < frame_len {
                                    panic!(
                                        "peer frame header, then incomplete payload, then hung up"
                                    )
                                }
                            }
                        }

                        let payload = if frame_len == 0 {
                            Roll::empty()
                        } else {
                            res_buf.take_at_most(frame_len).unwrap()
                        };
                        assert_eq!(payload.len(), frame_len);

                        trace!(%frame_len, "got frame payload");
                        ev_tx.send(Ev::Frame { frame, payload }).await.unwrap();
                    }
                    Err(nom::Err::Incomplete(_)) => {
                        if eof {
                            panic!(
                                "peer sent incomplete frame header then hung up (buf len: {})",
                                res_buf.len()
                            )
                        }

                        continue;
                    }
                    Err(nom::Err::Failure(err) | nom::Err::Error(err)) => {
                        debug!(?err, "got parse error");
                        break;
                    }
                }
            }

            Ok::<_, eyre::Report>(())
        };
        fluke_buffet::spawn(async move { recv_fut.await.unwrap() });

        Self {
            w,
            scratch: RollMut::alloc().unwrap(),
            ev_rx,
            config,
            hpack_enc: Default::default(),
            hpack_dec: Default::default(),
            max_frame_size: DEFAULT_FRAME_SIZE as usize,
        }
    }

    pub async fn write_frame(&mut self, frame: Frame, payload: impl IntoPiece) -> eyre::Result<()> {
        let payload = payload.into_piece(&mut self.scratch)?;
        let frame = frame.with_len(payload.len().try_into().unwrap());

        let header = frame.into_piece(&mut self.scratch)?;
        self.w
            .writev_all_owned(PieceList::single(header).followed_by(payload))
            .await?;
        Ok(())
    }

    pub async fn write_ping(&mut self, ack: bool, payload: impl IntoPiece) -> eyre::Result<()> {
        self.write_frame(
            FrameType::Ping(if ack {
                PingFlags::Ack.into()
            } else {
                Default::default()
            })
            .into_frame(StreamId::CONNECTION),
            payload,
        )
        .await
    }

    pub async fn write_settings(&mut self, settings: Settings) -> eyre::Result<()> {
        self.write_frame(
            FrameType::Settings(Default::default()).into_frame(StreamId::CONNECTION),
            settings,
        )
        .await
    }

    /// Waits for a certain kind of frame
    pub async fn wait_for_frame(&mut self, types: impl Into<BitFlags<FrameT>>) -> FrameWaitOutcome {
        let deadline = Instant::now() + self.config.timeout;

        let types = types.into();
        let mut last_frame: Option<Frame> = None;

        loop {
            match tokio::time::timeout_at(deadline, self.ev_rx.recv()).await {
                Err(_) => {
                    return FrameWaitOutcome::Timeout {
                        wanted: types,
                        last_frame,
                        waited: self.config.timeout,
                    };
                }
                Ok(maybe_ev) => match maybe_ev {
                    None => {
                        return FrameWaitOutcome::Eof {
                            wanted: types,
                            last_frame,
                        }
                    }
                    Some(ev) => match ev {
                        Ev::Frame { frame, payload } => {
                            if types.contains(FrameT::from(frame.frame_type)) {
                                return FrameWaitOutcome::Success(frame, payload);
                            } else {
                                last_frame = Some(frame)
                            }
                        }
                        Ev::IoError { error } => {
                            return FrameWaitOutcome::IoError {
                                wanted: types,
                                last_frame,
                                error,
                            }
                        }
                    },
                },
            }
        }
    }

    /// Waits for a PING frame with Ack flag and the specified payload.
    /// It will NOT ignore other PING frames, if the first frame it
    /// receives doesn't have the expected payload, it will return an error.
    pub async fn verify_ping_frame_with_ack(&mut self, payload: &[u8]) -> eyre::Result<()> {
        let (frame, received_payload) = self.wait_for_frame(FrameT::Ping).await.unwrap();
        assert!(frame.is_ack(), "expected PING frame to have ACK flag");

        assert_eq!(received_payload, payload, "unexpected PING payload");

        Ok(())
    }

    pub async fn handshake(&mut self) -> eyre::Result<()> {
        // perform an HTTP/2 handshake as a client
        self.w.write_all_owned(PREFACE).await?;

        self.write_settings(default_settings()).await?;

        let (frame, payload) = self.wait_for_frame(FrameT::Settings).await.unwrap();
        assert!(
            !frame.is_ack(),
            "server should send their settings first thing (no ack)"
        );

        {
            // parse settings
            let (_rest, settings) = Settings::parse(payload).finish().unwrap();
            self.max_frame_size = settings.max_frame_size as _;
        }

        self.write_frame(
            Frame::new(
                FrameType::Settings(SettingsFlags::Ack.into()),
                StreamId::CONNECTION,
            ),
            (),
        )
        .await?;

        // and wait until the server acknowledges our settings
        let (frame, _payload) = self.wait_for_frame(FrameT::Settings).await.unwrap();
        assert!(frame.is_ack(), "server should acknowledge our settings");

        Ok(())
    }

    pub async fn send(&mut self, buf: impl Into<Piece>) -> eyre::Result<()> {
        self.w.write_all_owned(buf.into()).await?;
        Ok(())
    }

    async fn verify_connection_error(
        &mut self,
        codes: impl Into<BitFlags<ErrorC>>,
    ) -> eyre::Result<()> {
        let codes = codes.into();

        match self.wait_for_frame(FrameT::GoAway).await {
            FrameWaitOutcome::Success(_frame, payload) => {
                let (_, goaway) = GoAway::parse(payload).finish().unwrap();
                let error_c: ErrorC = KnownErrorCode::try_from(goaway.error_code)
                    .map_err(|_| eyre::eyre!(
                        "Expected GOAWAY with one of {codes:?}, but got unknown error code {} (0x{:x})",
                        goaway.error_code.as_repr(), goaway.error_code.as_repr()
                    ))?
                    .into();

                if codes.contains(error_c) {
                    // that's what we expected!
                    return Ok(());
                }
                Err(eyre::eyre!(
                    "Expected GOAWAY with one of {codes:?}, but got {error_c:?}"
                ))
            }
            FrameWaitOutcome::Timeout { last_frame, .. } => Err(eyre!(
                "Timed out while waiting for connection error, last frame: ({last_frame:?})"
            )),
            FrameWaitOutcome::Eof { .. } => {
                // that's fine
                Ok(())
            }
            FrameWaitOutcome::IoError { .. } => {
                // TODO: that's fine if it's a connection reset, we should probably check
                Ok(())
            }
        }
    }

    /// VerifyHeadersFrame verifies whether a HEADERS frame with specified stream ID has received.
    pub async fn verify_headers_frame(&mut self, stream_id: StreamId) -> eyre::Result<()> {
        let (frame, _payload) = self.wait_for_frame(FrameT::Headers).await.unwrap();
        assert_eq!(frame.stream_id, stream_id, "unexpected stream ID");
        Ok(())
    }

    pub async fn verify_stream_error(
        &mut self,
        codes: impl Into<BitFlags<ErrorC>>,
    ) -> eyre::Result<()> {
        let codes = codes.into();

        match self
            .wait_for_frame(FrameT::GoAway | FrameT::RstStream)
            .await
        {
            FrameWaitOutcome::Success(frame, payload) => {
                match frame.frame_type {
                    FrameType::GoAway => {
                        let (_, goaway) = GoAway::parse(payload).finish().unwrap();
                        let error_c: ErrorC = KnownErrorCode::try_from(goaway.error_code)
                            .map_err(|_| eyre::eyre!(
                                "Expected GOAWAY or RSTSTREAM with one of {codes:?}, but got unknown error code {} (0x{:x})",
                                goaway.error_code.as_repr(), goaway.error_code.as_repr()
                            ))?
                            .into();

                        if codes.contains(error_c) {
                            // that's what we expected!
                            Ok(())
                        } else {
                            Err(eyre::eyre!(
                                "Expected GOAWAY or RSTSTREAM with one of {codes:?}, but got {error_c:?}"
                            ))
                        }
                    }
                    FrameType::RstStream => {
                        let (_, rststream) = RstStream::parse(payload).finish().unwrap();
                        let error_code = KnownErrorCode::try_from(rststream.error_code)
                            .map_err(|_| eyre::eyre!(
                                "Expected GOAWAY or RSTSTREAM with one of {codes:?}, but got unknown error code {} (0x{:x})",
                                rststream.error_code.as_repr(), rststream.error_code.as_repr()
                            ))?;

                        let error_c: ErrorC = error_code.into();

                        if codes.contains(error_c) {
                            // that's what we expected!
                            Ok(())
                        } else {
                            Err(eyre::eyre!(
                                "Expected GOAWAY or RSTSTREAM with one of {codes:?}, but got {error_c:?}"
                            ))
                        }
                    }
                    _ => unreachable!(),
                }
            }
            FrameWaitOutcome::Timeout { last_frame, .. } => Err(eyre!(
                "Timed out while waiting for stream error, last frame: ({last_frame:?})"
            )),
            FrameWaitOutcome::Eof { .. } => {
                // that's fine
                Ok(())
            }
            FrameWaitOutcome::IoError { .. } => {
                // TODO: that's fine if it's a connection reset, we should probably check
                Ok(())
            }
        }
    }

    fn common_headers(&self) -> Headers {
        let (scheme, default_port) = if self.config.tls {
            ("https", self.config.port == 443)
        } else {
            ("http", self.config.port == 80)
        };

        let authority = if default_port {
            self.config.host.clone()
        } else {
            format!("{}:{}", self.config.host, self.config.port)
        };

        let mut headers = Headers::default();
        headers.insert(":method".into(), "POST".into());
        headers.insert(":scheme".into(), scheme.into());
        headers.insert(":path".into(), self.config.path.clone().into_bytes().into());
        headers.insert(":authority".into(), authority.into_bytes().into());
        headers
    }

    pub fn encode_headers(&mut self, headers: &Headers) -> eyre::Result<Piece> {
        // wasteful, but we're doing tests so shrug.
        let mut fragment = Vec::new();
        for (k, v) in headers.iter() {
            self.hpack_enc
                .encode_header_into((k.as_ref(), v.as_ref()), &mut fragment)?;
        }
        Ok(fragment.into())
    }

    pub async fn write_headers(
        &mut self,
        block_fragment: Piece,
        stream_id: StreamId,
        flags: impl Into<BitFlags<HeadersFlags>>,
    ) -> eyre::Result<()> {
        let flags = flags.into();
        let frame = Frame::new(FrameType::Headers(flags), stream_id);
        self.write_frame(frame, block_fragment).await?;
        Ok(())
    }

    pub async fn write_data(
        &mut self,
        stream_id: StreamId,
        end_stream: bool,
        data: impl Into<Piece>,
    ) -> eyre::Result<()> {
        let frame = Frame::new(
            FrameType::Data(if end_stream {
                DataFlags::EndStream.into()
            } else {
                Default::default()
            }),
            stream_id,
        );
        self.write_frame(frame, data.into()).await?;
        Ok(())
    }

    fn dummy_headers(&self, len: usize) -> Headers {
        let mut headers = Headers::default();
        let dummy = dummy_bytes(self.config.max_header_len);

        for i in 0..len {
            let name = format!("x-dummy{}", i);
            headers.insert(name.into_bytes().into(), dummy.clone().into());
        }

        headers
    }
}

/// Parameters for tests
pub struct Config {
    /// which host to connect to
    pub host: String,

    /// which port to connect to
    pub port: u16,

    /// which path to request
    pub path: String,

    /// whether to use TLS
    pub tls: bool,

    /// how long to wait for a frame
    pub timeout: Duration,

    /// maximum length of a header
    pub max_header_len: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 80,
            path: "/".into(),
            tls: false,

            max_header_len: 4000,

            timeout: Duration::from_secs(1),
        }
    }
}

// DummyString returns a dummy string with specified length.
pub fn dummy_string(len: usize) -> String {
    "x".repeat(len)
}

// DummyBytes returns an array of bytes with specified length.
pub fn dummy_bytes(len: usize) -> Vec<u8> {
    vec![b'x'; len]
}
