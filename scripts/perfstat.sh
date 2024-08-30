#!/usr/bin/env -S bash -euo pipefail

. /root/.cargo/env

# Change to the script's directory
cd "$(dirname "$0")"

#PERF_EVENTS="cpu-clock,context-switches,cycles,instructions,branches,branch-misses,cache-references,cache-misses,page-faults,$(paste -sd ',' syscalls)"
PERF_EVENTS="cpu-clock,cycles,branch-misses,cache-misses,page-faults,$(paste -sd ',' syscalls)"

LOONA_DIR=~/bearcove/loona

# Build the servers
cargo build --release --manifest-path="$LOONA_DIR/Cargo.toml" -F tracing/release_max_level_info

# Launch hyper server
export TEST_PROTO=h2 ADDR=0.0.0.0 PORT=8001
"$LOONA_DIR/target/release/httpwg-hyper" &
HYPER_PID=$!

# Launch loona server
export TEST_PROTO=h2 ADDR=0.0.0.0 PORT=8002
"$LOONA_DIR/target/release/httpwg-loona" &
LOONA_PID=$!

HYPER_ADDR="http://localhost:8001"
LOONA_ADDR="http://localhost:8002"

ENDPOINT="${ENDPOINT:-/stream-big-body}"

declare -A servers=(
    [hyper]="$HYPER_PID $HYPER_ADDR"
    [loona]="$LOONA_PID $LOONA_ADDR"
)

if [[ -n "${SERVER:-}" ]]; then
    # If SERVER is set, only benchmark that one
    if [[ -v "servers[$SERVER]" ]]; then
        servers=([${SERVER}]="${servers[$SERVER]}")
    else
        echo "Error: SERVER '$SERVER' not found in the list of servers."
        exit 1
    fi
fi

for server in "${!servers[@]}"; do
    read -r PID ADDR <<< "${servers[$server]}"
    echo -e "\033[1;36mLoona Git SHA: $(cd ~/bearcove/loona && git rev-parse --short HEAD)\033[0m"
    echo -e "\033[1;33m🚀 Benchmarking \033[1;32m$(cat /proc/$PID/cmdline | tr '\0' ' ')\033[0m"
    echo -e "\033[1;34m📊 Benchmark parameters: RPS=${RPS:-2}, CONNS=${CONNS:-40}, STREAMS=${STREAMS:-8}, NUM_REQUESTS=${NUM_REQUESTS:-100}, ENDPOINT=${ENDPOINT:-/stream-big-body}\033[0m"
    perf stat -e "$PERF_EVENTS" -p "$PID" -- h2load --rps "${RPS:-2}" -c "${CONNS:-40}" -m "${STREAMS:-8}" -n "${NUM_REQUESTS:-100}" "${ADDR}${ENDPOINT}"
done

# Kill the servers
kill $HYPER_PID $LOONA_PID
