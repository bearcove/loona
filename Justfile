# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
	#!/bin/bash -eux
	just build-testbed
	cargo llvm-cov --no-report nextest
	cargo llvm-cov --no-report run --manifest-path h2spec-server/Cargo.toml -- -j 'target/junit.xml'
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
	cargo build --release --manifest-path hyper-testbed/Cargo.toml
	
single-test *args:
	just test --no-capture {{args}}

bench *args:
	RUST_BACKTRACE=1 cargo bench {{args}} -- --plotting-backend plotters

h2spec *args:
	echo "This requires h2spec to be installed: https://github.com/summerwind/h2spec"
	cargo run --manifest-path h2spec-server/Cargo.toml -- {{args}}

check:
	cargo clippy --all-targets
