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

quick-cov:
	SKIP_H2SPEC=1 scripts/cov.sh

# Run all tests with cargo nextest
test *args:
	just build-testbed
	RUST_BACKTRACE=1 cargo nextest run {{args}}

curl-tests *args:
	just build-testbed
	RUST_BACKTRACE=1 cargo nextest run --no-capture -p fluke-curl-tests {{args}}

build-testbed:
	cargo build --release -p fluke-hyper-testbed

single-test *args:
	just test --no-capture {{args}}

bench *args:
	RUST_BACKTRACE=1 cargo bench {{args}} -- --plotting-backend plotters

h2spec *args:
	#!/bin/bash -eux
	export RUST_LOG="${RUST_LOG:-fluke=debug,fluke_hpack=info}"
	export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
	cargo run -p fluke-h2spec -- {{args}}

check:
	#!/bin/bash -eu
	cargo clippy --all-targets --all-features

tls-sample:
	cargo run -p fluke-tls-sample
