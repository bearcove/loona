#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]

mod util;

mod types;
pub use types::*;

pub mod h1;
pub mod h2;

mod responder;
pub use responder::*;

pub use fluke_buffet as buffet;
pub use fluke_maybe_uring as maybe_uring;

/// re-exported so consumers can use whatever forked version we use
pub use http;

#[allow(async_fn_in_trait)] // we never require Send
pub trait ServerDriver {
    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>>;
}
