use std::rc::Rc;

use fluke_io_uring_async::IoUringAsync;

thread_local! {
    static URING: Rc<IoUringAsync> = {
        // FIXME: magic values
        Rc::new(IoUringAsync::new(8).unwrap())
    };
}

/// Returns the thread-local IoUringAsync instance
pub fn get_ring() -> Rc<IoUringAsync> {
    let mut u = None;
    URING.with(|u_| u = Some(u_.clone()));
    u.unwrap()
}
