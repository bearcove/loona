use std::future::Future;

pub mod buf;

#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
pub use tokio_uring;

#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
mod compat;

#[cfg(feature = "net")]
pub mod net;

pub type BufResult<T, B> = (std::io::Result<T>, B);

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

/// Equivalent to `tokio_uring::start`
#[cfg(all(target_os = "linux", feature = "tokio-uring"))]
pub fn start<F: Future>(task: F) -> F::Output {
    tokio_uring::start(task)
}

/// Equivalent to `tokio_uring::start`
#[cfg(not(all(target_os = "linux", feature = "tokio-uring")))]
pub fn start<F: Future>(task: F) -> F::Output {
    use tokio::task::LocalSet;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let local = LocalSet::new();
            local.run_until(task).await
        })
}
