use std::{
    cell::RefCell, collections::HashMap, ffi::OsString, net::SocketAddr, rc::Rc, time::Duration,
};

use buffet::{net::TcpStream, IntoHalves};
use httpwg::{Config, Conn};
use tracing::Level;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Default, Debug)]
struct Args {
    /// the binary to run tests against (and any args to pass to it)
    server_binary: Vec<String>,

    /// the address/port the binary will listen on
    server_address: Option<SocketAddr>,

    /// the timeout for connections (in milliseconds)
    connect_timeout: Option<u64>,

    /// which tests to run
    filter: Option<String>,

    /// whether to print verbose output
    verbose: bool,
}

pub trait IntoStringResult {
    fn into_string_result(self) -> eyre::Result<String>;
}

impl IntoStringResult for OsString {
    fn into_string_result(self) -> eyre::Result<String> {
        self.into_string()
            .map_err(|_| eyre::eyre!("OsString contained invalid UTF-8"))
    }
}

fn parse_args() -> eyre::Result<Args> {
    let mut args: Args = Default::default();
    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next().unwrap() {
        match arg {
            lexopt::Arg::Long("address") | lexopt::Arg::Short('a') => {
                let value = parser.value()?.into_string_result()?;
                args.server_address = Some(match value.parse() {
                    Ok(addr) => addr,
                    Err(_) => {
                        use std::net::ToSocketAddrs;
                        let addrs: Vec<_> = value.to_socket_addrs()?.collect();
                        addrs
                            .iter()
                            // prefer IPv4 addresses but we'll take what we can get
                            .find(|addr| addr.is_ipv4())
                            .or_else(|| addrs.first())
                            .cloned()
                            .ok_or_else(|| {
                                eyre::eyre!("Failed to parse/resolve address: {}", value)
                            })?
                    }
                });
            }
            lexopt::Arg::Long("connect-timeout") | lexopt::Arg::Short('t') => {
                args.connect_timeout = Some(
                    parser
                        .value()?
                        .into_string_result()?
                        .parse()
                        .map_err(|e| eyre::eyre!("Failed to parse connect timeout: {}", e))?,
                );
            }
            lexopt::Arg::Long("filter") | lexopt::Arg::Short('f') => {
                args.filter = Some(parser.value()?.into_string_result()?);
            }
            lexopt::Arg::Long("verbose") | lexopt::Arg::Short('v') => {
                args.verbose = true;
            }
            lexopt::Arg::Value(value) => {
                args.server_binary.push(value.into_string_result()?);
            }
            _ => return Err(arg.unexpected().into()),
        }
    }
    Ok(args)
}

fn print_usage() -> eyre::Result<()> {
    eprintln!(
        "Usage: httpwg-test-suite [OPTIONS] [-- SERVER [ARGS]]

Options:
    -a, --address <ADDRESS>    The address/port the server will listen on
    -t, --connect-timeout <MS> The timeout for connections in milliseconds
    -f, --filter <FILTER>      Which tests to run
    -v, --verbose              Print verbose output

Arguments:
    SERVER                     The server to run tests against
    [ARGS]                     Any additional arguments to pass to the server

Examples:
    httpwg-test-suite -a 127.0.0.1:8080 -- ./my_server
    httpwg-test-suite -f 'RFC 9113' -- ./my_server --go-fast
"
    );
    Ok(())
}

fn main() -> eyre::Result<()> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("Failed to parse arguments: {}", e);

            print_usage()?;
            return Ok(());
        }
    };
    setup_tracing_and_error_reporting();
    buffet::start(async move { async_main(args).await })?;

    Ok(())
}

async fn async_main(mut args: Args) -> eyre::Result<()> {
    let cat = catalog::<buffet::net::TcpStream>();

    let addr = match args.server_address {
        Some(addr) => addr,
        None => {
            eprintln!("No address specified");
            print_usage()?;
            std::process::exit(1);
        }
    };
    let connect_timeout = match args.connect_timeout {
        Some(timeout) => Duration::from_millis(timeout),
        None => Duration::from_millis(250),
    };
    let conf = Rc::new(Config {
        timeout: connect_timeout,
        ..Default::default()
    });

    eprintln!("Will run tests against {addr} with a connect timeout of {connect_timeout:?}");

    // this works around an oddity of Just when forwarding positional arguments
    args.server_binary.retain(|s| !s.is_empty());

    let mut server_name = format!("a server listening on {addr}");

    if !args.server_binary.is_empty() {
        let binary_and_args = args.server_binary;
        let binary_name = &binary_and_args[0];
        server_name = format!("{binary_name} listening on {addr}");

        eprintln!(
            "Launching ({}) now and waiting until it listens on {addr}",
            binary_and_args.join(" ::: ")
        );
        let mut iter = binary_and_args.into_iter();
        let mut cmd = std::process::Command::new(iter.next().unwrap());
        for arg in iter {
            cmd.arg(arg);
        }
        #[cfg(target_os = "linux")]
        unsafe {
            // avoid zombie children on unix: no matter how the test runner dies,
            // the server will die with it.
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                let ret = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                if ret != 0 {
                    panic!("prctl failed");
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().expect("Failed to launch server");
        eprintln!("Server started");
        std::thread::spawn(move || {
            let status = child.wait().unwrap();
            eprintln!("Server exited with status: {status:?}");
        });
    } else {
        eprintln!("No server binary specified");
    };

    eprintln!("Waiting until server is listening on {addr}");
    let start = std::time::Instant::now();
    let duration = Duration::from_secs(1);
    while start.elapsed() < duration {
        match tokio::time::timeout(Duration::from_millis(100), TcpStream::connect(addr)).await {
            Ok(Ok(_)) => break,
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    if start.elapsed() >= duration {
        panic!("Server did not start listening within 3 seconds");
    }

    let mut local_set = tokio::task::LocalSet::new();

    let sequential = std::env::var("SEQUENTIAL")
        .map(|v| v == "1")
        .unwrap_or(false);

    let mut num_tests = 0;
    let num_passed: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

    let start_time = std::time::Instant::now();

    for (rfc, sections) in cat {
        for (section, tests) in sections {
            for (test, boxed_test) in tests {
                let test_name = format!("{rfc} :: {section} :: {test}");
                if let Some(filter) = &args.filter {
                    if !test_name.contains(filter) {
                        continue;
                    }
                }

                num_tests += 1;
                let stream =
                    tokio::time::timeout(Duration::from_millis(250), TcpStream::connect(addr))
                        .await
                        .unwrap()
                        .unwrap();
                let conn = Conn::new(conf.clone(), stream);
                let num_passed = num_passed.clone();
                let test = async move {
                    if args.verbose {
                        eprintln!("🔷 Running test: {}", test_name);
                    }
                    match boxed_test(conn).await {
                        Ok(()) => {
                            eprintln!("✅ Test passed: {}", test_name);
                            {
                                *num_passed.borrow_mut() += 1;
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Test failed: {}\n{e:?}", test_name);
                        }
                    }
                };
                local_set.spawn_local(async move {
                    {
                        test.await;
                    }
                });
                if sequential {
                    (&mut local_set).await;
                }
            }
        }
    }

    local_set.await;
    let num_passed = *num_passed.borrow();

    eprintln!(
        "🚄 Passed \x1b[1;32m{}/{}\x1b[0m tests in \x1b[1;33m{:.2}\x1b[0m seconds against \x1b[1;36m{}\x1b[0m",
        num_passed,
        num_tests,
        start_time.elapsed().as_secs_f32(),
        server_name,
    );

    if num_passed != num_tests {
        eprintln!("❌ Some tests failed");
        std::process::exit(1);
    }

    Ok(())
}

type Catalog<IO> =
    HashMap<&'static str, HashMap<&'static str, HashMap<&'static str, BoxedTest<IO>>>>;

#[allow(unused)]
fn print_catalog<IO: IntoHalves>(cat: &Catalog<IO>) {
    for (rfc, sections) in cat {
        println!("📕 {}", rfc);
        for (section, tests) in sections {
            println!("  🔷 {}", section);
            for test in tests.keys() {
                println!("    📄 {}", test);
            }
        }
    }
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

httpwg_macros::gen_catalog!(catalog);
