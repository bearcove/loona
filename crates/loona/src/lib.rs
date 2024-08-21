mod types;
mod util;

use error::BoxError;
pub use types::*;

pub mod h1;
pub mod h2;

mod responder;
pub use responder::*;

pub use buffet;

/// re-exported so consumers can use whatever forked version we use
pub use http;

pub mod error;

#[allow(async_fn_in_trait)] // we never require Send
pub trait ServerDriver {
    type Error: std::error::Error + 'static;

    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> Result<Responder<E, ResponseDone>, Self::Error>;
}
