use std::rc::Rc;

use fluke_io_uring_async::IoUringAsync;

/// Returns the thread-local IoUringAsync instance
pub fn get_ring() -> Rc<IoUringAsync> {
    fluke_io_uring_async::get_ring()
}
