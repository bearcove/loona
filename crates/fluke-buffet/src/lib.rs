use std::future::Future;

mod roll;
pub use roll::*;

mod piece;
pub use piece::*;

pub mod bufpool;
use bufpool::*;

mod io;
pub use io::*;

pub mod net;

#[cfg(all(target_os = "linux", feature = "uring"))]
mod uring;

#[cfg(all(target_os = "linux", feature = "uring"))]
pub use uring::get_ring;

/// Spawns a new asynchronous task, returning a [tokio::task::JoinHandle] for it.
///
/// Spawning a task enables the task to execute concurrently to other tasks.
/// There is no guarantee that a spawned task will execute to completion. When a
/// runtime is shutdown, all outstanding tasks are dropped, regardless of the
/// lifecycle of that task.
///
/// This must be executed from within a runtime created by [crate::start]
pub fn spawn<T: Future + 'static>(task: T) -> tokio::task::JoinHandle<T::Output> {
    tokio::task::spawn_local(task)
}

/// Build a new current-thread runtime and runs the provided future on it
#[cfg(all(target_os = "linux", feature = "uring"))]
pub fn start<F: Future>(task: F) -> F::Output {
    use fluke_io_uring_async::IoUringAsync;
    use send_wrapper::SendWrapper;
    use tokio::task::LocalSet;

    let u = SendWrapper::new(uring::get_ring());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_park(move || {
            u.submit().unwrap();
        })
        .build()
        .unwrap();
    rt.block_on(async move {
        let local = LocalSet::new();
        local.spawn_local(IoUringAsync::listen(get_ring()));

        let res = local.run_until(task).await;
        tokio::time::timeout(std::time::Duration::from_millis(250), local).await;
        res
    })
}

/// Build a new current-thread runtime and runs the provided future on it
#[cfg(not(all(target_os = "linux", feature = "uring")))]
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
