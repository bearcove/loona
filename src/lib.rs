#![allow(incomplete_features)]
#![feature(thread_local)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

mod util;

mod buffet;
pub use buffet::*;

mod io;
pub use io::*;

mod types;
pub use types::*;

pub mod h1;
pub mod h2;

/// re-exported so consumers can use whatever forked version we use
pub use tokio_uring;
