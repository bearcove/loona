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

/// Spawns a new asynchronous task, returning a [tokio::task::JoinHandle] for
/// it.
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
    use luring::IoUringAsync;
    use send_wrapper::SendWrapper;
    use tokio::task::LocalSet;

    let u = SendWrapper::new(uring::get_ring());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_park(move || {
            let start = std::time::Instant::now();
            tracing::trace!("thread park, submitting...");
            u.submit().unwrap();
            tracing::trace!(
                "thread park, submitting... done! (took {:?})",
                start.elapsed()
            );
        })
        .build()
        .unwrap();
    let res = rt.block_on(async move {
        crate::bufpool::initialize_allocator().unwrap();
        let mut lset = LocalSet::new();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let listen_task = IoUringAsync::listen(get_ring());
        lset.spawn_local(async move {
            tokio::select! {
                _ = listen_task => {
                    tracing::trace!("IoUringAsync listen task finished");
                },
                _ = cancel_rx => {
                    tracing::trace!("IoUringAsync listen task cancelled");
                }
            }
        });

        let res = lset.run_until(task).await;

        tracing::debug!("waiting for local set (cancellations, cleanups etc.)");

        // during this poll, the async cancellations get submitted
        let cancel_submit_timeout = std::time::Duration::from_millis(0);
        if (tokio::time::timeout(cancel_submit_timeout, &mut lset).await).is_err() {
            drop(cancel_tx);

            // during this second poll, the async cancellations hopefully finish
            let cleanup_timeout = std::time::Duration::from_millis(500);
            if (tokio::time::timeout(cleanup_timeout, lset).await).is_err() {
                tracing::warn!(
                    "🥲 timed out waiting for local set (async cancellations, cleanups etc.)"
                );
            }
        }

        res
    });
    rt.shutdown_timeout(std::time::Duration::from_millis(20));
    res
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
            crate::bufpool::initialize_allocator().unwrap();
            let lset = LocalSet::new();
            lset.run_until(task).await
        })
}
