# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test:
    #!/bin/bash -eux
    just build-testbed
    just cov

cov:
	scripts/cov.sh

# Run all tests with cargo nextest
test *args:
	just build-testbed
	export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
	cargo nextest run {{args}}

build-testbed:
	cargo build --release -p fluke-hyper-testbed

single-test *args:
	just test --no-capture {{args}}

check:
	#!/bin/bash -eu
	cargo clippy --all-targets --all-features

tls-sample:
	cargo run -p fluke-tls-sample

httpwg-gen:
    cargo run --release --package httpwg-gen
