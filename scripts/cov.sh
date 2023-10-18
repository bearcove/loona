#!/bin/bash -eux

export RUSTFLAGS='-C instrument-coverage --cfg=coverage --cfg=coverage_nightly --cfg=trybuild_no_target -C instrument-coverage --cfg=coverage --cfg=coverage_nightly --cfg=trybuild_no_target'

export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${PWD}/target-cov"

export COVERAGE_DIR="${PWD}/coverage"
rm -rf "${COVERAGE_DIR}"

mkdir -p "${COVERAGE_DIR}/raw"
# must be set before building
export LLVM_PROFILE_FILE="${COVERAGE_DIR}/raw/fluke-%p-%12m.profraw"

RUSTC_PATH=$(rustup which rustc)
# replace `bin/rustc` with `lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-cov`
LLVM_TOOLS_PATH=${RUSTC_PATH%/*/*}/lib/rustlib/x86_64-unknown-linux-gnu/bin
LLVM_PROFDATA="${LLVM_TOOLS_PATH}/llvm-profdata"
"${LLVM_PROFDATA}" --version
LLVM_COV="${LLVM_TOOLS_PATH}/llvm-cov"
"${LLVM_COV}" --version

cargo nextest run --verbose --profile ci --manifest-path crates/fluke-hpack/Cargo.toml --features interop-tests --release
cargo nextest run --verbose --profile ci --manifest-path crates/fluke/Cargo.toml
cargo nextest run --verbose --profile ci --manifest-path test-crates/fluke-curl-tests/Cargo.toml

cargo build --manifest-path test-crates/fluke-h2spec/Cargo.toml
for suite in generic hpack http2; do
	"${CARGO_TARGET_DIR}"/debug/fluke-h2spec "${suite}" -j "target/h2spec-${suite}.xml"
done

# merge all profiles
"${LLVM_PROFDATA}" merge -sparse "${COVERAGE_DIR}/raw"/*.profraw -o "${COVERAGE_DIR}/fluke.profdata"

# this is what the binary for `fluke-curl-tests` ends up being called.
# yes, that's unfortunate.
objects=("${CARGO_TARGET_DIR}"/debug/deps/* "${CARGO_TARGET_DIR}"/release/deps/*)

# build a new array where each item of 'objects' is prefixed with the string '-object':
args=()
for object in "${objects[@]}"; do
  # continue if it ends in '.d'
	[[ $object =~ \.d$ ]] && continue

	# continue if it ends in '.rmeta'
	[[ $object =~ \.rmeta$ ]] && continue

	args+=(-object "$object")
done

# report coverage as HTML
"${LLVM_COV}" report --instr-profile "${COVERAGE_DIR}/fluke.profdata" \
	--ignore-filename-regex "rustc|.cargo|test-crates|non_uring" \
	"${CARGO_TARGET_DIR}"/debug/fluke-h2spec \
	"${args[@]}"
