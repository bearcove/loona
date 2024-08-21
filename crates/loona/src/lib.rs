use std::error::Error as StdError;

mod types;
mod util;

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
pub trait ServerDriver<OurEncoder>
where
    OurEncoder: Encoder,
{
    type Error: AsRef<dyn StdError>;

    async fn handle(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<OurEncoder, ExpectResponseHeaders>,
    ) -> Result<Responder<OurEncoder, ResponseDone>, Self::Error>;
}
