#!/usr/bin/env -S uv run --with 'termcolor'

import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from termcolor import colored

# Change to the script's directory
os.chdir(os.path.dirname(os.path.abspath(__file__)))

# Create a new process group
os.setpgrp()

# Set trap to kill the process group on script exit
def kill_group():
    os.killpg(0, signal.SIGTERM)

signal.signal(signal.SIGTERM, lambda signum, frame: kill_group())

# Create directory if it doesn't exist
Path("/tmp/loona-perfstat").mkdir(parents=True, exist_ok=True)

# Kill older processes
for pidfile in Path("/tmp/loona-perfstat").glob("*.PID"):
    if pidfile.is_file():
        with open(pidfile) as f:
            pid = f.read().strip()
        if pid != str(os.getpid()):
            try:
                os.kill(int(pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
        pidfile.unlink()

# Kill older remote processes
subprocess.run(["ssh", "brat", "pkill -9 h2load"], check=False)

PERF_EVENTS = f"cycles,instructions,branches,branch-misses,cache-references,cache-misses,page-faults,{','.join(open('syscalls').read().splitlines())}"

LOONA_DIR = os.path.expanduser("~/bearcove/loona")

# Build the servers
subprocess.run(["cargo", "build", "--release", "--manifest-path", f"{LOONA_DIR}/Cargo.toml", "-F", "tracing/release_max_level_info"], check=True)

# Set protocol, default to h2c
PROTO = os.environ.get("PROTO", "h2c")
os.environ["PROTO"] = PROTO

OUR_PUBLIC_IP = subprocess.check_output(["curl", "-4", "ifconfig.me"]).decode().strip()
HTTP_OR_HTTPS = "https" if PROTO == "tls" else "http"

# Launch hyper server
hyper_env = os.environ.copy()
hyper_env.update({"ADDR": "0.0.0.0", "PORT": "8001"})
hyper_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-hyper"], env=hyper_env)
HYPER_PID = hyper_process.pid
with open("/tmp/loona-perfstat/hyper.PID", "w") as f:
    f.write(str(HYPER_PID))
print(f"hyper PID: {HYPER_PID}")

# Launch loona server
loona_env = os.environ.copy()
loona_env.update({"ADDR": "0.0.0.0", "PORT": "8002"})
loona_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-loona"], env=loona_env)
LOONA_PID = loona_process.pid
with open("/tmp/loona-perfstat/loona.PID", "w") as f:
    f.write(str(LOONA_PID))
print(f"loona PID: {LOONA_PID}")

HYPER_ADDR = f"{HTTP_OR_HTTPS}://{OUR_PUBLIC_IP}:8001"
LOONA_ADDR = f"{HTTP_OR_HTTPS}://{OUR_PUBLIC_IP}:8002"

# Declare h2load args based on PROTO
H2LOAD_ARGS = []
if PROTO == "h1":
    print("Error: h1 is not supported")
    sys.exit(1)
elif PROTO == "h2c":
    pass
elif PROTO == "tls":
    ALPN_LIST = os.environ.get("ALPN_LIST", "h2,http/1.1")
    H2LOAD_ARGS = [f"--alpn-list={ALPN_LIST}"]
else:
    print(f"Error: Unknown PROTO '{PROTO}'")
    sys.exit(1)

servers = {
    "hyper": (HYPER_PID, HYPER_ADDR),
    "loona": (LOONA_PID, LOONA_ADDR)
}

SERVER = os.environ.get("SERVER")
if SERVER:
    if SERVER in servers:
        servers = {SERVER: servers[SERVER]}
    else:
        print(f"Error: SERVER '{SERVER}' not found in the list of servers.")
        sys.exit(1)

H2LOAD = "/nix/var/nix/profiles/default/bin/h2load"

ENDPOINT = os.environ.get("ENDPOINT", "/repeat-4k-blocks/128")
RPS = os.environ.get("RPS", "2")
CONNS = os.environ.get("CONNS", "40")
STREAMS = os.environ.get("STREAMS", "8")
WARMUP = int(os.environ.get("WARMUP", "5"))
DURATION = int(os.environ.get("DURATION", "20"))
TIMES = int(os.environ.get("TIMES", "1"))

# Set MODE to 'stat' if not specified
MODE = os.environ.get("MODE", "stat")

if MODE == "record":
    PERF_CMD = f"perf record -F 99 -e {PERF_EVENTS} -p"
elif MODE == "stat":
    PERF_CMD = f"perf stat -e {PERF_EVENTS} -p"
else:
    print(f"Error: Unknown MODE '{MODE}'")
    sys.exit(1)

print(colored(f"📊 Benchmark parameters: RPS={RPS}, CONNS={CONNS}, STREAMS={STREAMS}, WARMUP={WARMUP}, DURATION={DURATION}, TIMES={TIMES}", "blue"))

for server, (PID, ADDR) in servers.items():
    print(colored(f"Loona Git SHA: {subprocess.check_output(['git', 'rev-parse', '--short', 'HEAD'], cwd=os.path.expanduser('~/bearcove/loona')).decode().strip()}", "cyan"))
    print(colored(f"🚀 Benchmarking {open(f'/proc/{PID}/cmdline').read().replace(chr(0), ' ').strip()}", "yellow"))
    remote_command = [H2LOAD] + H2LOAD_ARGS + ["--rps", RPS, "-c", CONNS, "-m", STREAMS, "--duration", str(DURATION), f"{ADDR}{ENDPOINT}"]

    if MODE == "record":
        samply_process = subprocess.Popen(["samply", "record", "-p", str(PID)])
        with open("/tmp/loona-perfstat/samply.PID", "w") as f:
            f.write(str(samply_process.pid))
        subprocess.run(["ssh", "brat"] + remote_command, check=True)
        samply_process.send_signal(signal.SIGINT)
        samply_process.wait()
    else:
        for i in range(1, TIMES + 1):
            print("===================================================")
            print(colored(f"🏃 Run {i} of {TIMES} for {server} server 🏃", "magenta"))
            print("===================================================")
            ssh_process = subprocess.Popen(["ssh", "brat"] + remote_command)
            time.sleep(WARMUP)
            MEASURE_DURATION = DURATION - WARMUP - 1
            print(f"Starting perf for {MEASURE_DURATION} seconds...")
            subprocess.run(["perf", "stat", "-e", PERF_EVENTS, "-p", str(PID), "--", "sleep", str(MEASURE_DURATION)], check=True)
            print("Starting perf... done!")
            ssh_process.wait()
