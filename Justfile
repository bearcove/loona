# just manual: https://github.com/casey/just#readme

_default:
	just --list

# Run all tests with nextest and cargo-llvm-cov
ci-test: #!/bin/bash -eux
    just build-testbed
    just cov
    just httpwg-over-tcp

cov:
    #!/bin/bash -eux
    just build-testbed
    export RUSTUP_TOOLCHAIN=nightly-2024-05-26
    rm -rf coverage
    mkdir -p coverage
    cargo llvm-cov nextest --branch --ignore-filename-regex '.*crates/(httpwg|fluke-hyper-testbed|fluke-tls-sample|fluke-sample-h2-server).*' --html --output-dir=coverage
    cargo llvm-cov report --lcov --output-path 'coverage/lcov.info'

build-testbed:
	cargo build --release -p fluke-hyper-testbed

t *args:
    just test {{args}}

# Run all tests with cargo nextest
test *args:
	#!/bin/bash
	just build-testbed httpwg-gen
	export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
	cargo nextest run {{args}}

test1 test:
	#!/bin/bash
	export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
	cargo nextest run --no-capture -E 'test(/{{test}}$/)'

check:
	#!/bin/bash -eu
	cargo clippy --all-targets --all-features

tls-sample:
	cargo run -p fluke-tls-sample

httpwg-gen:
    cargo run --release --package httpwg-gen

httpwg-over-tcp *args:
    #!/usr/bin/env -S bash -eux
    cargo build --release \
        --package fluke-httpwg-server \
        --package httpwg-cli
    export TEST_PROTO=h2
    export PORT=8001
    ./target/release/httpwg \
        --address localhost:8001 \
        {{args}} \
        -- ./target/release/fluke-httpwg-server

instruments:
    #!/usr/bin/env -S bash -eux
    cargo instruments \
        --bench "encoding" \
        --template time \
        --profile profiling \
        -- \
        --bench 'format_content_length/format_content_length/itoa/buffet' \
        --profile-time 10

samply:
    #!/usr/bin/env -S bash -eux
    cargo build \
        --package fluke-httpwg-server \
        --profile profiling \
        --features tracing/release_max_level_info
    export TEST_PROTO=h2
    export PORT=8002
    target/profiling/fluke-httpwg-server
