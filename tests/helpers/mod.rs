use std::future::Future;

pub(crate) mod sample_hyper_server;
pub(crate) mod tracing_common;

pub(crate) fn run(test: impl Future<Output = eyre::Result<()>>) {
    tokio_uring::start(async {
        tracing_common::setup_tracing();
        color_eyre::install().unwrap();

        if let Err(e) = test.await {
            panic!("Error: {}", e);
        }
    });
}
