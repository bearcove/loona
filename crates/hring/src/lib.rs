#![allow(incomplete_features)]
#![feature(type_alias_impl_trait)]
#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

mod util;

mod types;
pub use types::*;

pub mod h1;
pub mod h2;

use std::future::Future;

mod responder;
pub use responder::*;

pub use hring_buffet as buffet;

/// re-exported so consumers can use whatever forked version we use
pub use http;

/// re-exported so consumers can use whatever forked version we use
#[cfg(feature = "tokio-uring")]
pub use tokio_uring;

/// Spawns a new asynchronous task, returning a [`JoinHandle`] for it.
///
/// Spawning a task enables the task to execute concurrently to other tasks.
/// There is no guarantee that a spawned task will execute to completion. When a
/// runtime is shutdown, all outstanding tasks are dropped, regardless of the
/// lifecycle of that task.
///
/// This function must be called from the context of a `tokio-uring` runtime,
/// or a tokio local set (at the time of this writing, they're the same thing).
pub fn spawn<T: Future + 'static>(task: T) -> tokio::task::JoinHandle<T::Output> {
    tokio::task::spawn_local(task)
}

pub trait ServerDriver {
    async fn handle<E: Encoder>(
        &self,
        req: Request,
        req_body: &mut impl Body,
        respond: Responder<E, ExpectResponseHeaders>,
    ) -> eyre::Result<Responder<E, ResponseDone>>;
}
