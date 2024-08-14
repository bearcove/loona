use std::rc::Rc;

use luring::IoUringAsync;

/// Returns the thread-local IoUringAsync instance
pub fn get_ring() -> Rc<IoUringAsync> {
    luring::get_ring()
}
