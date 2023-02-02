//! HTTP/2 parser
//!
//! HTTP/2 https://httpwg.org/specs/rfc9113.html
//! HTTP semantics https://httpwg.org/specs/rfc9110.html

use std::fmt;

use enum_repr::EnumRepr;
use enumflags2::{bitflags, BitFlags};
use nom::{
    combinator::map,
    number::streaming::{be_u24, be_u8},
    sequence::tuple,
    IResult,
};

use hring_buffet::{Roll, RollMut};

/// This is sent by h2 clients after negotiating over ALPN, or when doing h2c.
pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub fn preface(i: Roll) -> IResult<Roll, ()> {
    let (i, _) = nom::bytes::streaming::tag(PREFACE)(i)?;
    Ok((i, ()))
}

/// See https://httpwg.org/specs/rfc9113.html#FrameTypes
#[EnumRepr(type = "u8")]
#[derive(Debug, Clone, Copy)]
pub enum RawFrameType {
    Data = 0x00,
    Headers = 0x01,
    Priority = 0x02,
    RstStream = 0x03,
    Settings = 0x04,
    PushPromise = 0x05,
    Ping = 0x06,
    GoAway = 0x07,
    WindowUpdate = 0x08,
    Continuation = 0x09,
}

/// Typed flags for various frame types
#[derive(Debug, Clone, Copy)]
pub enum FrameType {
    Data(BitFlags<DataFlags>),
    Headers(BitFlags<HeadersFlags>),
    Priority,
    RstStream,
    Settings(BitFlags<SettingsFlags>),
    PushPromise,
    Ping(BitFlags<PingFlags>),
    GoAway,
    WindowUpdate,
    Continuation(BitFlags<ContinuationFlags>),
    Unknown(EncodedFrameType),
}

/// See https://httpwg.org/specs/rfc9113.html#DATA
#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DataFlags {
    Padded = 0x08,
    EndStream = 0x01,
}

/// See https://httpwg.org/specs/rfc9113.html#rfc.section.6.2
#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HeadersFlags {
    Priority = 0x20,
    Padded = 0x08,
    EndHeaders = 0x04,
    EndStream = 0x01,
}

/// See https://httpwg.org/specs/rfc9113.html#SETTINGS
#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SettingsFlags {
    Ack = 0x01,
}

/// See https://httpwg.org/specs/rfc9113.html#PING
#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PingFlags {
    Ack = 0x01,
}

/// See https://httpwg.org/specs/rfc9113.html#CONTINUATION
#[bitflags]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ContinuationFlags {
    EndHeaders = 0x04,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodedFrameType {
    pub ty: u8,
    pub flags: u8,
}

impl EncodedFrameType {
    fn parse(i: Roll) -> IResult<Roll, Self> {
        let (i, (ty, flags)) = tuple((be_u8, be_u8))(i)?;
        Ok((i, Self { ty, flags }))
    }
}

impl From<(RawFrameType, u8)> for EncodedFrameType {
    fn from((ty, flags): (RawFrameType, u8)) -> Self {
        Self {
            ty: ty.repr(),
            flags,
        }
    }
}

impl FrameType {
    pub(crate) fn encode(self) -> EncodedFrameType {
        match self {
            FrameType::Data(f) => (RawFrameType::Data, f.bits()).into(),
            FrameType::Headers(f) => (RawFrameType::Headers, f.bits()).into(),
            FrameType::Priority => (RawFrameType::Priority, 0).into(),
            FrameType::RstStream => (RawFrameType::RstStream, 0).into(),
            FrameType::Settings(f) => (RawFrameType::Settings, f.bits()).into(),
            FrameType::PushPromise => (RawFrameType::PushPromise, 0).into(),
            FrameType::Ping(f) => (RawFrameType::Ping, f.bits()).into(),
            FrameType::GoAway => (RawFrameType::GoAway, 0).into(),
            FrameType::WindowUpdate => (RawFrameType::WindowUpdate, 0).into(),
            FrameType::Continuation(f) => (RawFrameType::Continuation, f.bits()).into(),
            FrameType::Unknown(ft) => ft,
        }
    }

    fn decode(ft: EncodedFrameType) -> Self {
        match RawFrameType::from_repr(ft.ty) {
            Some(ty) => match ty {
                RawFrameType::Data => {
                    FrameType::Data(BitFlags::<DataFlags>::from_bits_truncate(ft.flags))
                }
                RawFrameType::Headers => {
                    FrameType::Headers(BitFlags::<HeadersFlags>::from_bits_truncate(ft.flags))
                }
                RawFrameType::Priority => FrameType::Priority,
                RawFrameType::RstStream => FrameType::RstStream,
                RawFrameType::Settings => {
                    FrameType::Settings(BitFlags::<SettingsFlags>::from_bits_truncate(ft.flags))
                }
                RawFrameType::PushPromise => FrameType::PushPromise,
                RawFrameType::Ping => {
                    FrameType::Ping(BitFlags::<PingFlags>::from_bits_truncate(ft.flags))
                }
                RawFrameType::GoAway => FrameType::GoAway,
                RawFrameType::WindowUpdate => FrameType::WindowUpdate,
                RawFrameType::Continuation => FrameType::Continuation(
                    BitFlags::<ContinuationFlags>::from_bits_truncate(ft.flags),
                ),
            },
            None => FrameType::Unknown(ft),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub(crate) u32);

impl StreamId {
    /// Stream ID used for connection control frames
    pub const CONNECTION: Self = Self(0);
}

#[derive(Debug, thiserror::Error)]
#[error("invalid stream id: {0}")]
pub struct StreamIdOutOfRange(u32);

impl TryFrom<u32> for StreamId {
    type Error = StreamIdOutOfRange;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value & 0x8000_0000 != 0 {
            Err(StreamIdOutOfRange(value))
        } else {
            Ok(Self(value))
        }
    }
}

impl fmt::Debug for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// See https://httpwg.org/specs/rfc9113.html#FrameHeader
#[derive(Debug)]
pub struct Frame {
    pub frame_type: FrameType,
    pub reserved: u8,
    pub stream_id: StreamId,
    pub len: u32,
}

impl Frame {
    /// Create a new frame with the given type and stream ID.
    pub fn new(frame_type: FrameType, stream_id: StreamId) -> Self {
        Self {
            frame_type,
            reserved: 0,
            stream_id,
            len: 0,
        }
    }

    /// Set the frame's length.
    pub fn with_len(mut self, len: u32) -> Self {
        self.len = len;
        self
    }

    /// Parse a frame from the given slice. This also takes the payload from the
    /// slice, and copies it to the heap, which may not be ideal for a production
    /// implementation.
    pub fn parse(i: Roll) -> IResult<Roll, Self> {
        let (i, (len, frame_type, (reserved, stream_id))) = tuple((
            be_u24,
            EncodedFrameType::parse,
            parse_reserved_and_stream_id,
        ))(i)?;

        let frame = Frame {
            frame_type: FrameType::decode(frame_type),
            reserved,
            stream_id,
            len,
        };
        Ok((i, frame))
    }

    pub fn write_into(self, mut w: impl std::io::Write) -> eyre::Result<()> {
        use byteorder::{BigEndian, WriteBytesExt};
        w.write_u24::<BigEndian>(self.len as _)?;
        let ft = self.frame_type.encode();
        w.write_u8(ft.ty)?;
        w.write_u8(ft.flags)?;
        // TODO: do we ever need to write the reserved bit?
        w.write_u32::<BigEndian>(self.stream_id.0)?;

        Ok(())
    }

    pub fn into_roll(self, mut scratch: &mut RollMut) -> eyre::Result<Roll> {
        debug_assert_eq!(scratch.len(), 0);
        self.write_into(&mut scratch)?;
        Ok(scratch.take_all())
    }
}

/// See https://httpwg.org/specs/rfc9113.html#FrameHeader - the first bit
/// is reserved, and the rest is a 32-bit stream id
fn parse_reserved_and_stream_id(i: Roll) -> IResult<Roll, (u8, StreamId)> {
    fn reserved(i: (Roll, usize)) -> IResult<(Roll, usize), u8> {
        nom::bits::streaming::take(1_usize)(i)
    }

    fn stream_id(i: (Roll, usize)) -> IResult<(Roll, usize), StreamId> {
        nom::combinator::map(nom::bits::streaming::take(31_usize), StreamId)(i)
    }

    nom::bits::bits(tuple((reserved, stream_id)))(i)
}

// cf. https://httpwg.org/specs/rfc9113.html#HEADERS
#[derive(Debug)]
pub(crate) struct PrioritySpec {
    pub exclusive: bool,
    pub stream_dependency: StreamId,
    // 0-255 => 1-256
    pub weight: u8,
}

impl PrioritySpec {
    pub(crate) fn parse(i: Roll) -> IResult<Roll, Self> {
        map(
            tuple((parse_reserved_and_stream_id, be_u8)),
            |((exclusive, stream_dependency), weight)| Self {
                exclusive: exclusive != 0,
                stream_dependency,
                weight,
            },
        )(i)
    }
}

#[derive(Clone, Copy)]
pub struct ErrorCode(u32);

impl fmt::Debug for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match KnownErrorCode::from_repr(self.0) {
            Some(e) => fmt::Debug::fmt(&e, f),
            None => write!(f, "ErrorCode(0x{:02x})", self.0),
        }
    }
}

impl From<KnownErrorCode> for ErrorCode {
    fn from(e: KnownErrorCode) -> Self {
        Self(e as u32)
    }
}

#[EnumRepr(type = "u32")]
#[derive(Debug, Clone, Copy)]
pub enum KnownErrorCode {
    /// The associated condition is not a result of an error. For example, a
    /// GOAWAY might include this code to indicate graceful shutdown of a
    /// connection.
    NoError = 0x00,

    /// The endpoint detected an unspecific protocol error. This error is for
    /// use when a more specific error code is not available.
    ProtocolError = 0x01,

    /// The endpoint encountered an unexpected internal error.
    InternalError = 0x02,

    /// The endpoint detected that its peer violated the flow-control protocol.
    FlowControlError = 0x03,

    /// The endpoint sent a SETTINGS frame but did not receive a response in a
    /// timely manner. See Section 6.5.3 ("Settings Synchronization").
    /// https://httpwg.org/specs/rfc9113.html#SettingsSync
    SettingsTimeout = 0x04,

    /// The endpoint received a frame after a stream was half-closed.
    StreamClosed = 0x05,

    /// The endpoint received a frame with an invalid size.
    FrameSizeError = 0x06,

    /// The endpoint refused the stream prior to performing any application
    /// processing (see Section 8.7 for details).
    /// https://httpwg.org/specs/rfc9113.html#Reliability
    RefusedStream = 0x07,

    /// The endpoint uses this error code to indicate that the stream is no
    /// longer needed.
    Cancel = 0x08,

    /// The endpoint is unable to maintain the field section compression context
    /// for the connection.
    CompressionError = 0x09,

    /// The connection established in response to a CONNECT request (Section
    /// 8.5) was reset or abnormally closed.
    /// https://httpwg.org/specs/rfc9113.html#CONNECT
    ConnectError = 0x0a,

    /// The endpoint detected that its peer is exhibiting a behavior that might
    /// be generating excessive load.
    EnhanceYourCalm = 0x0b,

    /// The underlying transport has properties that do not meet minimum
    /// security requirements (see Section 9.2).
    /// https://httpwg.org/specs/rfc9113.html#TLSUsage
    InadequateSecurity = 0x0c,

    /// The endpoint requires that HTTP/1.1 be used instead of HTTP/2.
    Http1_1Required = 0x0d,
}

impl TryFrom<ErrorCode> for KnownErrorCode {
    type Error = ();

    fn try_from(e: ErrorCode) -> Result<Self, Self::Error> {
        KnownErrorCode::from_repr(e.0).ok_or(())
    }
}
