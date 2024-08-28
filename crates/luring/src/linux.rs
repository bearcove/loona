use io_uring::{opcode::AsyncCancel, IoUring};
use std::cell::RefCell;
use std::future::Future;
use std::os::unix::prelude::{AsRawFd, RawFd};
use std::rc::Rc;
use tokio::io::unix::AsyncFd;

thread_local! {
    // This is a thread-local for now, but it shouldn't be. This is only the case
    // for op cancellations.
    static URING: Rc<IoUringAsync> = {
        Rc::new(IoUringAsync::new_default().unwrap())
    };
}

/// Returns the thread-local IoUringAsync instance
pub fn get_ring() -> Rc<IoUringAsync> {
    let mut u = None;
    URING.with(|u_| u = Some(u_.clone()));
    u.unwrap()
}

// The IoUring Op state.
enum Lifecycle<C: cqueue::Entry> {
    // The Op has been pushed onto the submission queue, but has not yet
    // polled by the Rust async runtime. This state is somewhat confusingly named
    // in that an Op in the `submitted` state has not necessarily been
    // submitted to the io_uring with the `io_uring_submit` syscall.
    Submitted,
    // The Rust async runtime has polled the Op, but a completion
    // queue entry has not yet been received. When a completion queue entry is
    // received, the Waker can be used to trigger the Rust async runtime to poll
    // the Op.
    Waiting(std::task::Waker),
    // The Op has received a submission queue entry. The Op will
    // be Ready the next time that it is polled.
    Completed(C),
}

// An Future implementation that represents the current state of an IoUring Op.
pub struct Op<C: cqueue::Entry> {
    // Ownership over the OpInner value is moved to a new tokio
    // task when an Op is dropped.
    inner: Option<OpInner<C>>,
}

impl<C: cqueue::Entry> Future for Op<C> {
    type Output = C;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // It is safe to unwrap inner because it is only set to None after
        // the Op has been dropped.
        std::pin::Pin::new(self.inner.as_mut().unwrap()).poll(cx)
    }
}

impl<C: cqueue::Entry> Drop for Op<C> {
    fn drop(&mut self) {
        let inner = self.inner.take().unwrap();
        let guard = inner.slab.borrow();
        let index = inner.index;
        match &guard[inner.index] {
            Lifecycle::Completed(_) => {}
            _ => {
                let state_name = match &guard[inner.index] {
                    Lifecycle::Submitted => "Submitted",
                    Lifecycle::Waiting(_) => "Waiting",
                    Lifecycle::Completed(_) => "Completed",
                };
                tracing::debug!("dropping op {index} ({})", state_name);

                drop(guard);

                // submit cancel op
                let cancel = AsyncCancel::new(inner.index.try_into().unwrap()).build();
                let mut cancel_op = get_ring().push(cancel);
                let cancel_op_inner = cancel_op.inner.take().unwrap();
                std::mem::forget(cancel_op);

                tokio::task::spawn_local(async move {
                    cancel_op_inner.await;
                    inner.await;
                });
            }
        }
    }
}

pub struct OpInner<C: cqueue::Entry> {
    slab: Rc<RefCell<slab::Slab<Lifecycle<C>>>>,
    index: usize,
}

impl<C: cqueue::Entry> Future for OpInner<C> {
    type Output = C;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut guard = self.slab.borrow_mut();
        let lifecycle = &mut guard[self.index];
        match lifecycle {
            Lifecycle::Submitted => {
                tracing::trace!("polling OpInner {}... submitted!", self.index);
                *lifecycle = Lifecycle::Waiting(cx.waker().clone());
                std::task::Poll::Pending
            }
            Lifecycle::Waiting(_) => {
                tracing::trace!("polling OpInner {}... waiting!", self.index);
                *lifecycle = Lifecycle::Waiting(cx.waker().clone());
                std::task::Poll::Pending
            }
            Lifecycle::Completed(cqe) => {
                tracing::trace!("polling OpInner {}... completed!", self.index);
                std::task::Poll::Ready(cqe.clone())
            }
        }
    }
}

impl<C: cqueue::Entry> Drop for OpInner<C> {
    fn drop(&mut self) {
        let mut guard = self.slab.borrow_mut();
        let lifecycle = guard.remove(self.index);
        match lifecycle {
            Lifecycle::Completed(_) => {}
            _ => {
                if std::thread::panicking() {
                    // thread is panicking, eschewing drop cleanliness check
                } else {
                    let lifecycle_name = match lifecycle {
                        Lifecycle::Submitted => "Submitted",
                        Lifecycle::Waiting(_) => "Waiting",
                        Lifecycle::Completed(_) => "Completed",
                    };
                    let index = self.index;
                    tracing::debug!("dropping op inner {index} ({})", lifecycle_name);

                    panic!("Op drop occured before completion (index {})", self.index)
                }
            }
        };
    }
}

pub mod cqueue;
pub mod squeue;

pub struct IoUringAsync<
    S: squeue::Entry = io_uring::squeue::Entry,
    C: cqueue::Entry = io_uring::cqueue::Entry,
> {
    uring: Rc<IoUring<S, C>>,
    slab: Rc<RefCell<slab::Slab<Lifecycle<C>>>>,
}

impl<S: squeue::Entry, C: cqueue::Entry> AsRawFd for IoUringAsync<S, C> {
    fn as_raw_fd(&self) -> RawFd {
        self.uring.as_raw_fd()
    }
}

impl IoUringAsync<io_uring::squeue::Entry, io_uring::cqueue::Entry> {
    pub fn new_default() -> std::io::Result<Self> {
        let mut entries = 512;
        if let Ok(env_entries) = std::env::var("IO_URING_ENTRIES") {
            entries = env_entries
                .parse()
                .expect("$IO_URING_ENTRIES must be a number");
        }
        eprintln!(
            "==== IO_URING RING SIZE: {} (override with $IO_URING_ENTRIES)",
            entries
        );
        Self::new(entries)
    }

    pub fn new(entries: u32) -> std::io::Result<Self> {
        let mut builder = io_uring::IoUring::builder();
        let sqpoll_enabled = matches!(
            std::env::var("IO_URING_SQPOLL").as_deref(),
            Ok("1") | Ok("true")
        );
        eprintln!("==== SQPOLL: {sqpoll_enabled} (override with $IO_URING_SQPOLL=1)");

        let mut sqpoll_idle_ms = 200;
        if let Ok(env_sqpoll_idle_ms) = std::env::var("IO_URING_SQPOLL_IDLE_MS") {
            sqpoll_idle_ms = env_sqpoll_idle_ms
                .parse()
                .expect("$IO_URING_SQPOLL_IDLE_MS must be a number");
        }
        eprintln!(
            "==== SQPOLL_IDLE_MS: {} (override with $IO_URING_SQPOLL_IDLE_MS)",
            sqpoll_idle_ms
        );
        if sqpoll_enabled {
            builder.setup_sqpoll(sqpoll_idle_ms);
        }

        Ok(Self {
            uring: Rc::new(builder.build(entries)?),
            slab: Rc::new(RefCell::new(slab::Slab::new())),
        })
    }
}

impl<S: squeue::Entry, C: cqueue::Entry> IoUringAsync<S, C> {
    pub async fn listen(uring: Rc<IoUringAsync<S, C>>) {
        let async_fd = AsyncFd::new(uring).unwrap();
        loop {
            let start = std::time::Instant::now();
            tracing::trace!("waiting for uring fd to be ready...");
            let mut guard = async_fd.readable().await.unwrap();
            tracing::trace!(
                "waiting for uring fd to be ready... it is! (took {:?})",
                start.elapsed()
            );
            guard.get_inner().handle_cqe();
            guard.clear_ready();
        }
    }

    pub fn generic_new(entries: u32) -> std::io::Result<Self> {
        Ok(Self {
            uring: Rc::new(io_uring::IoUring::builder().build(entries)?),
            slab: Rc::new(RefCell::new(slab::Slab::new())),
        })
    }

    pub fn push(&self, entry: impl Into<S>) -> Op<C> {
        let mut guard = self.slab.borrow_mut();
        let index = guard.insert(Lifecycle::Submitted);
        let entry = entry.into().user_data(index.try_into().unwrap());
        while unsafe { self.uring.submission_shared().push(&entry).is_err() } {
            self.uring.submit().unwrap();
        }
        Op {
            inner: Some(OpInner {
                slab: self.slab.clone(),
                index,
            }),
        }
    }

    pub fn handle_cqe(&self) {
        let mut guard = self.slab.borrow_mut();
        while let Some(cqe) = unsafe { self.uring.completion_shared() }.next() {
            let index = cqe.user_data();
            tracing::trace!("received cqe for index {}", index);
            let lifecycle = &mut guard[index.try_into().unwrap()];
            match lifecycle {
                Lifecycle::Submitted => {
                    *lifecycle = Lifecycle::Completed(cqe);
                }
                Lifecycle::Waiting(waker) => {
                    waker.wake_by_ref();
                    *lifecycle = Lifecycle::Completed(cqe);
                }
                Lifecycle::Completed(cqe) => {
                    println!(
                        "multishot operations not implemented: {}, {}",
                        cqe.user_data(),
                        cqe.result()
                    );
                }
            }
        }
    }

    /// Submit all queued submission queue events to the kernel.
    pub fn submit(&self) -> std::io::Result<usize> {
        self.uring.submit()
    }
}

#[cfg(test)]
mod tests {
    use super::IoUringAsync;
    use io_uring::opcode::Nop;
    use send_wrapper::SendWrapper;
    use std::rc::Rc;

    #[test]
    fn example1() {
        let uring = Rc::new(IoUringAsync::new(8).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async move {
            tokio::task::LocalSet::new()
                .run_until(async {
                    tokio::task::spawn_local(IoUringAsync::listen(uring.clone()));

                    let fut1 = uring.push(Nop::new().build());
                    let fut2 = uring.push(Nop::new().build());

                    uring.submit().unwrap();

                    let cqe1 = fut1.await;
                    let cqe2 = fut2.await;

                    assert!(cqe1.result() >= 0, "nop error: {}", cqe1.result());
                    assert!(cqe2.result() >= 0, "nop error: {}", cqe2.result());
                })
                .await;
        });
    }

    #[test]
    fn example2() {
        let uring = IoUringAsync::new(8).unwrap();
        let uring = Rc::new(uring);

        // Create a new current_thread runtime that submits all outstanding submission
        // queue entries as soon as the executor goes idle.
        let uring_clone = SendWrapper::new(uring.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .on_thread_park(move || {
                uring_clone.submit().unwrap();
            })
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async move {
            tokio::task::LocalSet::new()
                .run_until(async {
                    tokio::task::spawn_local(IoUringAsync::listen(uring.clone()));

                    let cqe = uring.push(Nop::new().build()).await;
                    assert!(cqe.result() >= 0, "nop error: {}", cqe.result());
                })
                .await;
        });
    }
}
