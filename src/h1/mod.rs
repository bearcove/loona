//! HTTP/1.1 https://www.rfc-editor.org/rfc/rfc9112.
//! HTTP semantics https://www.rfc-editor.org/rfc/rfc9110

mod client;
pub use client::*;

mod server;
pub use server::*;

pub(crate) mod body;
pub(crate) mod encode;
pub(crate) mod parse;
