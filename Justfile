# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
	#!/bin/bash -eux
	just build-testbed
	cargo llvm-cov nextest --lcov --output-path coverage.lcov
	codecov

cov:
	cargo llvm-cov nextest --lcov --output-path lcov.info

# Run all tests with cargo nextest
test *args:
	just build-testbed
	RUST_BACKTRACE=1 cargo nextest run {{args}}

build-testbed:
	cargo build --release --manifest-path hyper-testbed/Cargo.toml
	
single-test *args:
	just test --no-capture {{args}}

bench *args:
	RUST_BACKTRACE=1 cargo bench {{args}} -- --plotting-backend plotters

check:
	cargo clippy --all-targets
