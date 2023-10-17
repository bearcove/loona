# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
	#!/bin/bash -eux
	just build-testbed
	source <(cargo llvm-cov show-env --export-prefix)
	cargo llvm-cov clean --workspace
	cargo nextest run --manifest-path crates/fluke-hpack/Cargo.toml --features interop-tests --release
	cargo nextest run --manifest-path crates/fluke/Cargo.toml --profile ci
	cargo nextest run --manifest-path test-crates/fluke-curl-tests/Cargo.toml --profile ci
	cargo run --manifest-path test-crates/fluke-h2spec/Cargo.toml -- generic -j 'target/h2spec-generic.xml'
	cargo run --manifest-path test-crates/fluke-h2spec/Cargo.toml -- hpack -j 'target/h2spec-hpack.xml'
	cargo run --manifest-path test-crates/fluke-h2spec/Cargo.toml -- http2 -j 'target/h2spec-http2.xml'
	cargo llvm-cov report --lcov --output-path coverage.lcov
	codecov

cov:
	cargo llvm-cov nextest --lcov --output-path lcov.info
	cargo llvm-cov report --html

# Run all tests with cargo nextest
test *args:
	just build-testbed
	RUST_BACKTRACE=1 cargo nextest run {{args}}

build-testbed:
	cargo build --release --manifest-path test-crates/hyper-testbed/Cargo.toml
	
single-test *args:
	just test --no-capture {{args}}

bench *args:
	RUST_BACKTRACE=1 cargo bench {{args}} -- --plotting-backend plotters

h2spec *args:
	#!/bin/bash -eux
	export RUST_LOG="${RUST_LOG:-fluke=debug,fluke_hpack=info}"
	export RUST_BACKTRACE=1
	cargo run --bin fluke-h2spec -- {{args}}

check:
	cargo clippy --all-targets --all-features
