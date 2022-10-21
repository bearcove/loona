#![feature(thread_local)]
#![feature(type_alias_impl_trait)]

pub mod bufpool;
pub mod parse;
pub mod proto;
pub mod types;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;
