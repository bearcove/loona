use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

/// Set up a global tracing subscriber.
///
/// This won't play well outside of cargo-nextest or running a single
/// test, which is a limitation we accept.
pub(crate) fn setup_tracing() {
    let filter_layer = Targets::new()
        .with_default(Level::DEBUG)
        .with_target("hring::io", Level::TRACE)
        .with_target("want", Level::INFO);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}
