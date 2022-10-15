# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
	#!/bin/bash -eux
	cargo llvm-cov nextest --lcov --output-path coverage.lcov
	codecov

# Run all tests with cargo nextest
test *args:
	RUST_BACKTRACE=1 cargo nextest run {{args}}
	
single-test *args:
	just test --no-capture {{args}}

bench *args:
	RUST_BACKTRACE=1 cargo bench {{args}} -- --plotting-backend plotters

check:
	cargo clippy --all-targets
