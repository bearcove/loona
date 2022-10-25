use std::future::Future;

pub(crate) mod fixed_driver;
pub(crate) mod tracing_common;

pub(crate) fn run(test: impl Future<Output = eyre::Result<()>>) {
    tokio_uring::start(async {
        tracing_common::setup_tracing();

        if let Err(e) = test.await {
            panic!("Error: {e:?}");
        }

        std::process::exit(0);
    });
}
