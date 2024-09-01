#!/usr/bin/env -S PYTHONUNBUFFERED=1 FORCE_COLOR=1 uv run --with 'termcolor~=2.4.0' --with 'pandas~=2.2.2' --with 'openpyxl~=3.1.5'

import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from termcolor import colored
import pandas as pd

# Change to the script's directory
os.chdir(os.path.dirname(os.path.abspath(__file__)))

# Set trap to kill the process group on script exit
def kill_group():
    os.killpg(0, signal.SIGTERM)

def abort_and_cleanup(signum, frame):
    print(colored("\nAborting and cleaning up...", "red"))
    kill_group()
    sys.exit(1)

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

PERF_EVENTS = [
    "cycles",
    "instructions",
    "branches",
    "branch-misses",
    "cache-references",
    "cache-misses",
    "page-faults",
    "syscalls:sys_enter_write",
    "syscalls:sys_enter_writev",
    "syscalls:sys_enter_epoll_wait",
    "syscalls:sys_enter_io_uring_enter"
]
LOONA_DIR = os.path.expanduser("~/bearcove/loona")

# Build the servers
subprocess.run(["cargo", "build", "--release", "--manifest-path", f"{LOONA_DIR}/Cargo.toml", "-F", "tracing/release_max_level_info"], check=True)

# Set protocol, default to h2c
PROTO = os.environ.get("PROTO", "h2c")
os.environ["PROTO"] = PROTO

# Get the default route interface
default_route = subprocess.check_output(["ip", "route", "show", "0.0.0.0/0"]).decode().strip()
default_interface = default_route.split()[4]

# Get the IP address of the default interface
ip_addr_output = subprocess.check_output(["ip", "addr", "show", "dev", default_interface]).decode().strip()

OUR_PUBLIC_IP = None
for line in ip_addr_output.split('\n'):
    if 'inet ' in line:
        OUR_PUBLIC_IP = line.split()[1].split('/')[0]
        break

if not OUR_PUBLIC_IP:
    print(colored("Error: Could not determine our public IP address", "red"))
    sys.exit(1)

print(f"📡 Our public IP address is {OUR_PUBLIC_IP}")

HTTP_OR_HTTPS = "https" if PROTO == "tls" else "http"

# Launch hyper server
hyper_env = os.environ.copy()
hyper_env.update({"ADDR": "0.0.0.0", "PORT": "8001"})
hyper_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-hyper"], env=hyper_env, preexec_fn=os.setpgrp)
HYPER_PID = hyper_process.pid
with open("/tmp/loona-perfstat/hyper.PID", "w") as f:
    f.write(str(HYPER_PID))

# Launch loona server
loona_env = os.environ.copy()
loona_env.update({"ADDR": "0.0.0.0", "PORT": "8002"})
loona_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-loona"], env=loona_env, preexec_fn=os.setpgrp)
LOONA_PID = loona_process.pid
with open("/tmp/loona-perfstat/loona.PID", "w") as f:
    f.write(str(LOONA_PID))

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

LOONA_GIT_SHA = subprocess.check_output(['git', 'rev-parse', '--short', 'HEAD'], cwd=os.path.expanduser('~/bearcove/loona')).decode().strip()

print(colored(f"📊 Benchmark parameters: RPS={RPS}, CONNS={CONNS}, STREAMS={STREAMS}, WARMUP={WARMUP}, DURATION={DURATION}, TIMES={TIMES}", "blue"))

try:
    for server, (PID, ADDR) in servers.items():
        print(colored(f"🚀 Benchmarking {open(f'/proc/{PID}/cmdline').read().replace(chr(0), ' ').strip()}", "yellow"))
        h2load_cmd = [H2LOAD] + H2LOAD_ARGS + ["--rps", RPS, "-c", CONNS, "-m", STREAMS, "--duration", str(DURATION), f"{ADDR}{ENDPOINT}"]

        if MODE == "record":
            samply_process = subprocess.Popen(["samply", "record", "-p", str(PID)])
            with open("/tmp/loona-perfstat/samply.PID", "w") as f:
                f.write(str(samply_process.pid))
            subprocess.run(["ssh", "brat"] + h2load_cmd, check=True)
            samply_process.send_signal(signal.SIGINT)
            samply_process.wait()
        else:
            for i in range(1, TIMES + 1):
                print(colored(f"🏃 Run {i} of {TIMES} for {server} server 🏃 (will take {DURATION} seconds)", "magenta"))
                ssh_process = subprocess.Popen(["ssh", "brat"] + h2load_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, preexec_fn=os.setpgrp)
                time.sleep(WARMUP)
                MEASURE_DURATION = DURATION - WARMUP - 1
                perf_cmd = ["perf", "stat", "-x", ",", "-e", ",".join(PERF_EVENTS), "-p", str(PID), "--", "sleep", str(MEASURE_DURATION)]
                perf_process = subprocess.Popen(perf_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, preexec_fn=os.setpgrp)
                perf_stdout, perf_stderr = perf_process.communicate()

                perf_output = perf_stdout.decode('utf-8') + perf_stderr.decode('utf-8')
                perf_lines = [line.strip().split(',') for line in perf_output.split('\n') if line.strip() and not line.startswith('#')]

                # Create a dictionary to store the data
                data = {}
                for line in perf_lines:
                    if len(line) >= 3:
                        value, _, label = line[:3]
                        data[label] = value

                # Create a DataFrame from the dictionary
                df = pd.DataFrame(data, index=[0]).T
                df.columns = ['Value']
                df.index.name = 'Event'

                # Save the DataFrame as an Excel file
                excel_filename = f"/tmp/loona-perfstat/{server}_run_{i}.xlsx"
                df.to_excel(excel_filename)
                print(colored(f"Saved performance data to {excel_filename}", "green"))

                ssh_process.wait()

                if ssh_process.returncode != 0:
                    output, error = ssh_process.communicate()
                    print("h2load command failed. Output:")
                    print(output.decode())
                    print("Error:")
                    print(error.decode())
except KeyboardInterrupt:
    print(colored("\nKeyboard interrupt detected. Cleaning up...", "red"))
    abort_and_cleanup(None, None)

print(colored("✨ All done, good bye! 👋", "cyan"))
