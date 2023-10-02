use std::future::Future;

pub(crate) mod tracing_common;

pub(crate) fn run(test: impl Future<Output = eyre::Result<()>>) {
    fluke_maybe_uring::start(async {
        tracing_common::setup_tracing();

        if let Err(e) = test.await {
            panic!("Error: {e:?}");
        }
    });
}
