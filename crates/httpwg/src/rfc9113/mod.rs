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

use fluke_h2_parse::Settings;

pub const DEFAULT_WINDOW_SIZE: u32 = 65536;
pub const DEFAULT_FRAME_SIZE: u32 = 16384;

pub fn default_settings() -> Settings {
    Settings {
        initial_window_size: DEFAULT_WINDOW_SIZE,
        max_frame_size: DEFAULT_FRAME_SIZE,
        ..Default::default()
    }
}

pub mod _3_4_http2_connection_preface;

pub mod _4_1_frame_format;
pub mod _4_2_frame_size;
pub mod _4_3_header_compression_and_decompression;

pub mod _5_1_stream_states;

pub mod _5_1_1_stream_identifiers;
pub mod _5_1_2_stream_concurrency;

pub mod _5_3_1_stream_dependencies;

pub mod _5_4_1_connection_error_handling;

pub mod _5_5_extending_http2;

pub mod _6_1_data;
