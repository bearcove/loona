#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

mod util;

mod types;
pub use types::*;

pub mod h1;
pub mod h2;

mod responder;
pub use responder::*;

pub use hring_buffet as buffet;

/// re-exported so consumers can use whatever forked version we use
pub use http;

pub trait ServerDriver {
    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>>;
}
