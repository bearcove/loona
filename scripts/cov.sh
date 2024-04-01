#!/bin/bash -eux

export RUSTUP_TOOLCHAIN=nightly-2024-03-31
export RUSTFLAGS='-C instrument-coverage -Z coverage-options=branch --cfg=coverage --cfg=trybuild_no_target'

export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${PWD}/target-cov"

rustup component add llvm-tools

export COVERAGE_DIR="${PWD}/coverage"
rm -rf "${COVERAGE_DIR}"

mkdir -p "${COVERAGE_DIR}/raw"
# must be set before building
export LLVM_PROFILE_FILE="${COVERAGE_DIR}/raw/fluke-%p-%12m.profraw"

declare -a RUSTC_TARGETS
RUSTC_TARGETS=$(rustc --print target-list)
RUSTC_SYSROOT=$(rustc --print sysroot)
# go through each target in the target list, find which one is
# at the end of RUSTC_SYSROOT
RUSTC_TARGET_TRIPLE="unknown"
for target in $RUSTC_TARGETS; do
  if [[ $RUSTC_SYSROOT == *$target ]]; then
    RUSTC_TARGET_TRIPLE=$target
    break
  fi
done

# extract target triple. this is wrong in case of cross-compilation but ah-well.
LLVM_TOOLS_PATH=${RUSTC_SYSROOT}/lib/rustlib/${RUSTC_TARGET_TRIPLE}/bin
LLVM_PROFDATA="${LLVM_TOOLS_PATH}/llvm-profdata"
"${LLVM_PROFDATA}" --version
LLVM_COV="${LLVM_TOOLS_PATH}/llvm-cov"
"${LLVM_COV}" --version

cargo nextest run --verbose --profile ci --cargo-profile ci --features fluke-hpack/interop-tests

cargo build --profile ci --manifest-path crates/fluke-h2spec/Cargo.toml
# skip if SKIP_H2SPEC is set to 1
if [[ "${SKIP_H2SPEC:-0}" == "1" ]]; then
  echo "Skipping h2spec suites"
else
  for suite in generic hpack http2; do
    "${CARGO_TARGET_DIR}"/ci/fluke-h2spec "${suite}" -j "target/h2spec-${suite}.xml"
  done
fi

# merge all profiles
"${LLVM_PROFDATA}" merge -sparse "${COVERAGE_DIR}/raw"/*.profraw -o "${COVERAGE_DIR}/fluke.profdata"

# build llvm-cov argument list
cover_args=()
cover_args+=(--instr-profile "${COVERAGE_DIR}/fluke.profdata")
cover_args+=(--ignore-filename-regex "rustc|.cargo|non_uring")
cover_args+=("${CARGO_TARGET_DIR}"/ci/fluke-h2spec)

set +x
objects=("${CARGO_TARGET_DIR}"/ci/deps/*)
for object in "${objects[@]}"; do
  # skip directories
  [[ -d $object ]] && continue

  # skip debug files '.d'
  [[ $object =~ \.d$ ]] && continue

  # skip rust metadata files '.rmeta'
  [[ $object =~ \.rmeta$ ]] && continue

  cover_args+=(-object "$object")
done

"${LLVM_COV}" export --format=lcov "${cover_args[@]}" > coverage/lcov.info
"${LLVM_COV}" show --format=html --output-dir=coverage/html "${cover_args[@]}"
"${LLVM_COV}" report --format=text "${cover_args[@]}"
