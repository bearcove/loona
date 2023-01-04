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
	cargo llvm-cov report --html

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

h2spec-server:
	cargo run --manifest-path h2spec-server/Cargo.toml

h2spec:
	echo "This requires h2spec to be installed: https://github.com/summerwind/h2spec"
	echo "...and the h2spec server to be running: just h2spec-server"
	h2spec -p 8888 -o 1

check:
	cargo clippy --all-targets
