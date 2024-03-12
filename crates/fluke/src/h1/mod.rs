//! HTTP/1.1 <https://httpwg.org/specs/rfc9112.html>
//! HTTP semantics <https://httpwg.org/specs/rfc9110.html>

mod client;
pub use client::*;

mod server;
pub use server::*;

pub(crate) mod body;
pub(crate) mod encode;
pub(crate) mod parse;
