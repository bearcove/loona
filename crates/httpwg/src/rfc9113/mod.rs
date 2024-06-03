//! RFC 9113 describes an optimized expression of the
//! semantics of the Hypertext Transfer Protocol (HTTP), referred to as
//! HTTP version 2 (HTTP/2).
//!
//! HTTP/2 enables a more efficient use of network resources and a reduced
//! latency by introducing field compression and allowing multiple concurrent
//! exchanges on the same connection.
//!
//! This document obsoletes RFCs 7540 and 8740.
//!
//! cf. <https://httpwg.org/specs/rfc9113.html>

use fluke_h2_parse::{Setting, SettingPairs};

pub const DEFAULT_WINDOW_SIZE: u32 = 65536;
pub const DEFAULT_FRAME_SIZE: u32 = 16384;

pub fn default_settings() -> SettingPairs<'static> {
    SettingPairs::from(
        [
            (Setting::InitialWindowSize, DEFAULT_WINDOW_SIZE),
            (Setting::MaxFrameSize, DEFAULT_FRAME_SIZE),
        ]
        .as_ref(),
    )
}

pub mod _3_starting_http2;
pub mod _4_http_frames;
pub mod _5_streams_and_multiplexing;
pub mod _6_frame_definitions;
pub mod _7_error_codes;
