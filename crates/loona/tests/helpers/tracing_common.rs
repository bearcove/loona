use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

/// Set up a global tracing subscriber.
///
/// This won't play well outside of cargo-nextest or running a single
/// test, which is a limitation we accept.
pub(crate) fn setup_tracing() {
    let targets = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log.parse::<Targets>().unwrap()
    } else {
        Targets::new()
            .with_default(Level::INFO)
            .with_target("loona", Level::DEBUG)
            .with_target("want", Level::INFO)
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_file(false)
        .with_line_number(false);

    tracing_subscriber::registry()
        .with(targets)
        .with(fmt_layer)
        .init();
}
