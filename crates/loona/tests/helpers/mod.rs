use std::future::Future;

use b_x::BX;

pub(crate) mod tracing_common;

pub(crate) fn run(test: impl Future<Output = Result<(), BX>>) {
    loona::buffet::start(async {
        tracing_common::setup_tracing();

        if let Err(e) = test.await {
            panic!("Error: {e}");
        }
    });
}
