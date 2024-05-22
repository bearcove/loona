use eyre::eyre;
use std::{rc::Rc, time::Duration};

use enumflags2::{bitflags, BitFlags};
use fluke_buffet::{IntoHalves, Piece, PieceList, Roll, RollMut, WriteOwned};
use fluke_h2_parse::{
    enumflags2, nom, Frame, FrameType, IntoPiece, KnownErrorCode, Settings, SettingsFlags, StreamId,
};
use tokio::time::Instant;
use tracing::debug;

pub mod rfc9113;

pub struct Conn<IO: IntoHalves + 'static> {
    w: <IO as IntoHalves>::Write,
    scratch: RollMut,
    pub ev_rx: tokio::sync::mpsc::Receiver<Ev>,
    config: Rc<Config>,
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
enum ErrorC {
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
                        debug!(%n, "read bytes (reading frame header)");
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
                            debug!(%n, len = %res_buf.len(), "read bytes (reading frame payload)");

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

                        debug!(%frame_len, "got frame payload");
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

    pub async fn handshake(&mut self) -> eyre::Result<()> {
        // perform an HTTP/2 handshake as a client

        let preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        self.w.write_all_owned(&preface[..]).await?;

        self.write_frame(
            Frame::new(
                fluke_h2_parse::FrameType::Settings(Default::default()),
                StreamId::CONNECTION,
            ),
            Settings::default(),
        )
        .await?;

        // now wait for the server's settings frame, which must be the first frame
        let (frame, _payload) = self.wait_for_frame(FrameT::Settings).await.unwrap();
        match frame.frame_type {
            FrameType::Settings(flags) => {
                assert!(
                    flags.contains(SettingsFlags::Ack),
                    "server should send their settings first thing (no ack)"
                );
            }
            _ => unreachable!(),
        };

        // good, good! let's acknowledge the server's settings
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
        match frame.frame_type {
            FrameType::Settings(flags) => {
                assert!(
                    flags.contains(SettingsFlags::Ack),
                    "server should acknowledge our settings"
                );
            }
            _ => unreachable!(),
        }

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
        let deadline = Instant::now() + self.config.timeout;
        let mut last_frame: Option<Frame> = None;

        loop {
            let fut = tokio::time::timeout_at(deadline, self.ev_rx.recv());
            let maybe_ev = fut.await.map_err(|_| {
                eyre!("Timed out while waiting for connection error, last frame: ({last_frame:?})")
            })?;

            let ev = match maybe_ev {
                None => {
                    // that's EOF / connection closed, it passes
                    return Ok(());
                }
                Some(frame) => frame,
            };

            let (frame, ..) = match ev {
                Ev::Frame { frame, payload } => (frame, payload),
                Ev::IoError { .. } => {
                    // that's probably connection reset? but we should check more closely
                    return Ok(());
                }
            };

            match frame.frame_type {
                FrameType::GoAway => {
                    // that's what we expected!
                    return Ok(());
                }
                _ => {
                    last_frame = Some(frame);
                }
            }
        }
    }
}

/// Parameters for tests
pub struct Config {
    /// how long to wait for a frame
    timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
        }
    }
}
