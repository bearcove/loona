//! HTTP/2 parser
//!
//! HTTP/2 https://httpwg.org/specs/rfc9113.html
//! HTTP semantics https://httpwg.org/specs/rfc9110.html

use std::fmt;

use enum_repr::EnumRepr;
use enumflags2::{bitflags, BitFlags};
use nom::{
    combinator::{map, map_res},
    number::streaming::{be_u24, be_u8},
    sequence::tuple,
    IResult,
};

use crate::{Roll, WriteOwned};

/// This is sent by h2 clients after negotiating over ALPN, or when doing h2c.
pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub fn preface(i: Roll) -> IResult<Roll, ()> {
    let (i, _) = nom::bytes::streaming::tag(PREFACE)(i)?;
    Ok((i, ()))
}

/// See https://httpwg.org/specs/rfc9113.html#FrameTypes
#[EnumRepr(type = "u8")]
#[derive(Debug)]
pub enum RawFrameType {
    Data = 0,
    Headers = 1,
    Priority = 2,
    RstStream = 3,
    Settings = 4,
    PushPromise = 5,
    Ping = 6,
    GoAway = 7,
    WindowUpdate = 8,
    Continuation = 9,
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
    Continuation,
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

impl FrameType {
    pub(crate) fn encode(&self) -> (RawFrameType, u8) {
        match self {
            FrameType::Data(f) => (RawFrameType::Data, f.bits()),
            FrameType::Headers(f) => (RawFrameType::Headers, f.bits()),
            FrameType::Priority => (RawFrameType::Priority, 0),
            FrameType::RstStream => (RawFrameType::RstStream, 0),
            FrameType::Settings(f) => (RawFrameType::Settings, f.bits()),
            FrameType::PushPromise => (RawFrameType::PushPromise, 0),
            FrameType::Ping(f) => (RawFrameType::Ping, f.bits()),
            FrameType::GoAway => (RawFrameType::GoAway, 0),
            FrameType::WindowUpdate => (RawFrameType::WindowUpdate, 0),
            FrameType::Continuation => (RawFrameType::Continuation, 0),
        }
    }

    fn decode(ty: RawFrameType, flags: u8) -> Self {
        match ty {
            RawFrameType::Data => FrameType::Data(BitFlags::<DataFlags>::from_bits_truncate(flags)),
            RawFrameType::Headers => {
                FrameType::Headers(BitFlags::<HeadersFlags>::from_bits_truncate(flags))
            }
            RawFrameType::Priority => FrameType::Priority,
            RawFrameType::RstStream => FrameType::RstStream,
            RawFrameType::Settings => {
                FrameType::Settings(BitFlags::<SettingsFlags>::from_bits_truncate(flags))
            }
            RawFrameType::PushPromise => FrameType::PushPromise,
            RawFrameType::Ping => FrameType::Ping(BitFlags::<PingFlags>::from_bits_truncate(flags)),
            RawFrameType::GoAway => FrameType::GoAway,
            RawFrameType::WindowUpdate => FrameType::WindowUpdate,
            RawFrameType::Continuation => FrameType::Continuation,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(u32);

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
        let (i, (len, frame_type, flags, (reserved, stream_id))) = tuple((
            be_u24,
            map_res(be_u8, |u| {
                RawFrameType::from_repr(u).ok_or(nom::error::ErrorKind::OneOf)
            }),
            be_u8,
            parse_reserved_and_stream_id,
        ))(i)?;

        let frame_type = FrameType::decode(frame_type, flags);

        let frame = Frame {
            frame_type,
            reserved,
            stream_id,
            len,
        };
        Ok((i, frame))
    }

    /// Write a frame (without payload)
    pub async fn write(&self, w: &impl WriteOwned) -> eyre::Result<()> {
        let mut header = vec![0u8; 9];
        {
            use byteorder::{BigEndian, WriteBytesExt};
            let mut header = &mut header[..];
            header.write_u24::<BigEndian>(self.len as _)?;
            let (ty, flags) = self.frame_type.encode();
            header.write_u8(ty.repr())?;
            header.write_u8(flags)?;
            // TODO: do we ever need to write the reserved bit?
            header.write_u32::<BigEndian>(self.stream_id.0)?;
        }

        let (res, _) = w.write_all(header).await;
        res?;
        Ok(())
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
pub(crate) struct HeadersPriority {
    pub exclusive: bool,
    pub stream_dependency: StreamId,
    // 0-255 => 1-256
    pub weight: u8,
}

pub(crate) fn parse_headers_priority(i: Roll) -> IResult<Roll, HeadersPriority> {
    map(
        tuple((parse_reserved_and_stream_id, be_u8)),
        |((exclusive, stream_dependency), weight)| HeadersPriority {
            exclusive: exclusive != 0,
            stream_dependency,
            weight,
        },
    )(i)
}
