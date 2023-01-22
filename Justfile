# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
	#!/bin/bash -eux
	just build-testbed
	cargo nextest run --manifest-path crates/hring-hpack/Cargo.toml --features interop-tests --release
	cargo llvm-cov --no-report nextest --profile ci
	cargo llvm-cov --no-report run --manifest-path test-crates/hring-h2spec/Cargo.toml -- -j 'target/h2spec-junit.xml'
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
	export RUST_LOG="${RUST_LOG:-debug,hpack=info}"
	export RUST_BACKTRACE=1
	cargo run --manifest-path test-crates/hring-h2spec/Cargo.toml -- {{args}}

check:
	cargo clippy --all-targets --all-features
