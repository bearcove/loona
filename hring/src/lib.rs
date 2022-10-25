#![allow(incomplete_features)]
#![feature(thread_local)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

pub mod bufpool;
pub mod io;
pub mod parse;
pub mod proto;
pub mod types;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;
