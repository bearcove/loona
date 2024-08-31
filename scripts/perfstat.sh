#!/usr/bin/env -S bash -euo pipefail

. /root/.cargo/env

# Change to the script's directory
cd "$(dirname "$0")"

# Create a new process group
set -m

# Set trap to kill the process group on script exit
trap 'kill -TERM -$$' EXIT

# Create directory if it doesn't exist
mkdir -p /tmp/loona-perfstat

# Kill older processes
for pidfile in /tmp/loona-perfstat/*.PID; do
    if [ -f "$pidfile" ]; then
        pid=$(cat "$pidfile")
        if [ "$pid" != "$$" ]; then
            kill "$pid" 2>/dev/null || true
        fi
        rm -f "$pidfile"
    fi
done

#PERF_EVENTS="cpu-clock,context-switches,cycles,instructions,branches,branch-misses,cache-references,cache-misses,page-faults,$(paste -sd ',' syscalls)"
PERF_EVENTS="cpu-clock,cycles,branch-misses,cache-misses,page-faults,$(paste -sd ',' syscalls)"

LOONA_DIR=~/bearcove/loona

# Build the servers
cargo build --release --manifest-path="$LOONA_DIR/Cargo.toml" -F tracing/release_max_level_info

# Set protocol, default to h2c
PROTO=${PROTO:-h2c}
export PROTO

OUR_PUBLIC_IP=$(curl -4 ifconfig.me)
if [[ "$PROTO" == "tls" ]]; then
    HTTP_OR_HTTPS="https"
else
    HTTP_OR_HTTPS="http"
fi

# Launch hyper server
ADDR=0.0.0.0 PORT=8001 "$LOONA_DIR/target/release/httpwg-hyper" &
HYPER_PID=$!
echo $HYPER_PID > /tmp/loona-perfstat/hyper.PID
echo "hyper PID: $HYPER_PID"

# Launch loona server
ADDR=0.0.0.0 PORT=8002 "$LOONA_DIR/target/release/httpwg-loona" &
LOONA_PID=$!
echo $LOONA_PID > /tmp/loona-perfstat/loona.PID
echo "loona PID: $LOONA_PID"

HYPER_ADDR="${HTTP_OR_HTTPS}://${OUR_PUBLIC_IP}:8001"
LOONA_ADDR="${HTTP_OR_HTTPS}://${OUR_PUBLIC_IP}:8002"

# Declare h2load args based on PROTO
declare -a H2LOAD_ARGS
if [[ "$PROTO" == "h1" ]]; then
    echo "Error: h1 is not supported"
    exit 1
elif [[ "$PROTO" == "h2c" ]]; then
    H2LOAD_ARGS=()
elif [[ "$PROTO" == "tls" ]]; then
    ALPN_LIST=${ALPN_LIST:-"h2,http/1.1"}
    H2LOAD_ARGS=(--alpn-list="$ALPN_LIST")
else
    echo "Error: Unknown PROTO '$PROTO'"
    exit 1
fi

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

H2LOAD="/nix/var/nix/profiles/default/bin/h2load"

ENDPOINT="${ENDPOINT:-/repeat-4k-blocks/128}"
RPS="${RPS:-2}"
CONNS="${CONNS:-40}"
STREAMS="${STREAMS:-8}"
WARM_UP_TIME="${WARM_UP_TIME:-5}"
DURATION="${DURATION:-20}"
TIMES="${TIMES:-1}"

# Set MODE to 'stat' if not specified
MODE=${MODE:-stat}

if [[ "$MODE" == "record" ]]; then
    PERF_CMD="perf record -F 99 -e $PERF_EVENTS -p"
elif [[ "$MODE" == "stat" ]]; then
    PERF_CMD="perf stat -e $PERF_EVENTS -p"
else
    echo "Error: Unknown MODE '$MODE'"
    exit 1
fi

echo -e "\033[1;34m📊 Benchmark parameters: RPS=$RPS, CONNS=$CONNS, STREAMS=$STREAMS, WARM_UP_TIME=$WARM_UP_TIME, DURATION=$DURATION, TIMES=$TIMES\033[0m"

for server in "${!servers[@]}"; do
    read -r PID ADDR <<< "${servers[$server]}"
    echo -e "\033[1;36mLoona Git SHA: $(cd ~/bearcove/loona && git rev-parse --short HEAD)\033[0m"
    echo -e "\033[1;33m🚀 Benchmarking \033[1;32m$(cat /proc/$PID/cmdline | tr '\0' ' ')\033[0m"
    remote_command=("$H2LOAD" "${H2LOAD_ARGS[@]}" --rps "$RPS" -c "$CONNS" -m "$STREAMS" --duration "$DURATION" "${ADDR}${ENDPOINT}")

    if [[ "$MODE" == "record" ]]; then
        samply record -p "$PID" &
        SAMPLY_PID=$!
        echo $SAMPLY_PID > /tmp/loona-perfstat/samply.PID
        ssh brat "${remote_command[@]}"
        kill -INT $SAMPLY_PID
        wait $SAMPLY_PID
    else
        for ((i=1; i<=$TIMES; i++)); do
        echo "==================================================="
        echo -e "\033[1;35m🏃 Run $i of $TIMES for $server server 🏃\033[0m"
        echo "==================================================="
            ssh brat "${remote_command[@]}" &
            SSH_PID=$!
            sleep "$WARM_UP_TIME"
            ACTUAL_DURATION=$((DURATION - WARM_UP_TIME - 1))
            echo "Starting perf for $ACTUAL_DURATION seconds..."
            perf stat -e "$PERF_EVENTS" -p "$PID" -- sleep "$ACTUAL_DURATION"
            echo "Starting perf... done!"
            wait $SSH_PID
        done
    fi
done
