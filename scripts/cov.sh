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
# skip if SKIP_H2SPEC is set to 1
if [[ "${SKIP_H2SPEC:-0}" == "1" ]]; then
  echo "Skipping h2spec suites"
else
	for suite in generic hpack http2; do
		"${CARGO_TARGET_DIR}"/debug/fluke-h2spec "${suite}" -j "target/h2spec-${suite}.xml"
	done
fi

# merge all profiles
"${LLVM_PROFDATA}" merge -sparse "${COVERAGE_DIR}/raw"/*.profraw -o "${COVERAGE_DIR}/fluke.profdata"

# build llvm-cov argument list
cover_args=()
cover_args+=(--instr-profile "${COVERAGE_DIR}/fluke.profdata")
cover_args+=(--ignore-filename-regex "rustc|.cargo|test-crates|non_uring")
cover_args+=("${CARGO_TARGET_DIR}"/debug/fluke-h2spec)

# pass *all* binaries/libraries to llvm-cov, including release ones
# because the fluke-hpack conformance tests are too slow in debug.
objects=("${CARGO_TARGET_DIR}"/debug/deps/* "${CARGO_TARGET_DIR}"/release/deps/*)
for object in "${objects[@]}"; do
  # skip debug files '.d'
	[[ $object =~ \.d$ ]] && continue

	# skip rust metadata files '.rmeta'
	[[ $object =~ \.rmeta$ ]] && continue

	cover_args+=(-object "$object")
done

"${LLVM_COV}" export --format=lcov "${cover_args[@]}" > coverage/lcov.info
"${LLVM_COV}" show --format=html --output-dir=coverage/html "${cover_args[@]}"
"${LLVM_COV}" report --format=text "${cover_args[@]}"
