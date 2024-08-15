use httpwg_loona::Proto;
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

fn main() {
    setup_tracing_and_error_reporting();

    let port: u16 = std::env::var("PORT")
        .unwrap_or("8001".to_string())
        .parse()
        .unwrap();
    let proto = match std::env::var("TEST_PROTO")
        .unwrap_or("h1".to_string())
        .as_str()
    {
        "h1" => Proto::H1,
        "h2" => Proto::H2,
        _ => panic!("TEST_PROTO must be either 'h1' or 'h2'"),
    };
    eprintln!("Using {proto:?} protocol (export TEST_PROTO=h1 or TEST_PROTO=h2 to override)");

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

    let server_handle = std::thread::spawn(move || {
        httpwg_loona::do_main(
            port,
            proto,
            httpwg_loona::Mode::Server {
                ready_tx,
                cancel_rx,
            },
        );
    });

    let ready = ready_rx.blocking_recv().unwrap();
    eprintln!("I listen on {}", ready.port);

    server_handle.join().unwrap();
    drop(cancel_tx);
}

fn setup_tracing_and_error_reporting() {
    color_eyre::install().unwrap();

    let targets = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        rust_log.parse::<Targets>().unwrap()
    } else {
        Targets::new()
            .with_default(Level::INFO)
            .with_target("loona", Level::DEBUG)
            .with_target("httpwg", Level::DEBUG)
            .with_target("want", Level::INFO)
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(true)
        .with_file(false)
        .with_line_number(false)
        .without_time();

    tracing_subscriber::registry()
        .with(targets)
        .with(fmt_layer)
        .init();
}
