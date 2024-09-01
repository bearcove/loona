#!/usr/bin/env -S PYTHONUNBUFFERED=1 FORCE_COLOR=1 uv run

import ctypes
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from termcolor import colored
import pandas as pd
import tempfile

# Change to the script's directory
os.chdir(os.path.dirname(os.path.abspath(__file__)))

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
PERF_EVENTS_STRING = ",".join(PERF_EVENTS)

# Define constants for prctl
PR_SET_PDEATHSIG = 1

# Load the libc shared library
libc = ctypes.CDLL("libc.so.6")

def set_pdeathsig():
    """Set the parent death signal to SIGKILL."""
    # Call prctl with PR_SET_PDEATHSIG and SIGKILL
    libc.prctl(PR_SET_PDEATHSIG, signal.SIGKILL)

# Set trap to kill the process group on script exit
def kill_group():
    os.killpg(0, signal.SIGKILL)

def abort_and_cleanup(signum, frame):
    print(colored("\nAborting and cleaning up...", "red"))
    kill_group()
    sys.exit(1)

# Register signal handlers
signal.signal(signal.SIGINT, abort_and_cleanup)
signal.signal(signal.SIGTERM, abort_and_cleanup)

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

# Declare h2load args based on PROTO
HTTP_OR_HTTPS = "https" if PROTO == "tls" else "http"

# Launch hyper server
hyper_env = os.environ.copy()
hyper_env.update({"ADDR": "0.0.0.0", "PORT": "8001"})
hyper_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-hyper"], env=hyper_env, preexec_fn=set_pdeathsig)
HYPER_PID = hyper_process.pid
with open("/tmp/loona-perfstat/hyper.PID", "w") as f:
    f.write(str(HYPER_PID))

# Launch loona server
loona_env = os.environ.copy()
loona_env.update({"ADDR": "0.0.0.0", "PORT": "8002"})
loona_process = subprocess.Popen([f"{LOONA_DIR}/target/release/httpwg-loona"], env=loona_env, preexec_fn=set_pdeathsig)
LOONA_PID = loona_process.pid
with open("/tmp/loona-perfstat/loona.PID", "w") as f:
    f.write(str(LOONA_PID))

HYPER_ADDR = f"{HTTP_OR_HTTPS}://{OUR_PUBLIC_IP}:8001"
LOONA_ADDR = f"{HTTP_OR_HTTPS}://{OUR_PUBLIC_IP}:8002"

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

endpoint = os.environ.get("ENDPOINT", "/repeat-4k-blocks/128")
rps = os.environ.get("RPS", "2")
clients = os.environ.get("CLIENTS", "20")
streams = os.environ.get("STREAMS", "2")
warmup = int(os.environ.get("WARMUP", "5"))
duration = int(os.environ.get("DURATION", "20"))
times = int(os.environ.get("TIMES", "3"))

# Mode can be 'perfstat' or 'samply'
mode = os.environ.get("MODE", "perfstat")

loona_git_sha = subprocess.check_output(['git', 'rev-parse', '--short', 'HEAD'], cwd=os.path.expanduser('~/bearcove/loona')).decode().strip()

print(colored(f"📊 Benchmark parameters: RPS={rps}, CONNS={clients}, STREAMS={streams}, WARMUP={warmup}, DURATION={duration}, TIMES={times}", "blue"))

def gen_h2load_cmd(addr: str):
    return [
        H2LOAD,
        "--rps", rps,
        "--clients", clients,
        "--max-concurrent-streams", streams,
        "--duration", str(duration),
        f"{addr}{endpoint}",
    ]

def do_samply():
    if len(servers) > 1:
        print(colored("Error: More than one server specified.", "red"))
        print("Please use SERVER=[loona,hyper] to narrow down to a single server.")
        sys.exit(1)

    for server, (PID, ADDR) in servers.items():
        print(colored("Warning: Warmup period is not taken into account in samply mode yet.", "yellow"))

        samply_process = subprocess.Popen(["samply", "record", "-p", str(PID)], preexec_fn=set_pdeathsig)
        with open("/tmp/loona-perfstat/samply.PID", "w") as f:
            f.write(str(samply_process.pid))
            h2load_cmd = gen_h2load_cmd(ADDR)
            subprocess.run(["ssh", "brat"] + h2load_cmd, check=True)
        samply_process.send_signal(signal.SIGINT)
        samply_process.wait()

def do_perfstat():
    for server, (PID, ADDR) in servers.items():
        print(colored(f"🚀 Benchmarking {open(f'/proc/{PID}/cmdline').read().replace(chr(0), ' ').strip()}", "yellow"))

        print(colored(f"🏃 Measuring {server} 🏃 (will take {duration} seconds)", "magenta"))
        output_path = tempfile.NamedTemporaryFile(delete=False, suffix='.csv').name

        perf_cmd = [
            "perf", "stat",
            "--event", PERF_EVENTS_STRING,
            "--field-separator", ",",
            "--output", output_path,
            "--pid", str(PID),
            "--delay", str(warmup*1000),
            "--",
            "ssh", "brat",
        ] + gen_h2load_cmd(ADDR)
        if PROTO == "tls":
            perf_cmd += ["--alpn-list", "h2"]

        perf_process = subprocess.Popen(perf_cmd, preexec_fn=set_pdeathsig)

        # perf stat output format:
        # For CSV output (-x','):
        # <value>,<unit>,<event>,<run stddev>,<time elapsed>,<percent of time elapsed>,,
        # For human-readable output:
        # <value>      <event>                                   #    <derived metric>   ( +- <run stddev>% )
        #
        # Columns:
        # 1. Value: The raw count of the event
        # 2. Unit: The unit of measurement (empty for most events)
        # 3. Event: The name of the performance event being measured
        # 4. Run stddev: The standard deviation between multiple runs as a percentage (0-100)
        # 5. Time elapsed: The total time the event was measured
        # 6. Percent of time elapsed: The percentage of total time the event was active
        # 7. Derived metric: Additional calculated metrics (e.g., instructions per cycle)

        perf_output = pd.read_csv(output_path, header=None, names=['Value', 'Unit', 'Event', 'StdDev', 'TimeElapsed', 'PercentTime', 'Empty1', 'Empty2'])
        os.unlink(output_path)

        print(perf_output)

        # Clean up the data
        perf_output['Event'] = perf_output['Event'].str.strip()
        perf_output['Value'] = pd.to_numeric(perf_output['Value'], errors='coerce')
        perf_output['StdDev'] = pd.to_numeric(perf_output['StdDev'], errors='coerce')

        # Create a DataFrame directly from the cleaned data
        df = perf_output.set_index('Event')[['Value', 'Unit', 'StdDev']]

        # Save the DataFrame as an Excel file
        excel_filename = f"/tmp/loona-perfstat/{server}.xlsx"
        df.to_excel(excel_filename)
        print(colored(f"Saved performance data to {excel_filename}", "green"))

try:
    if mode == 'perfstat':
        do_perfstat()
    elif mode == 'samply':
        do_samply()
    else:
        print(colored(f"Error: Unknown mode '{mode}'", "red"))
        print(colored("Known modes:", "yellow"))
        print(colored("- perfstat", "yellow"))
        print(colored("- samply", "yellow"))
        sys.exit(1)
except KeyboardInterrupt:
    print(colored("\nKeyboard interrupt detected. Cleaning up...", "red"))
    abort_and_cleanup(None, None)

print(colored("✨ All done, good bye! 👋", "cyan"))

print(colored("Processes still running in the group:", "yellow"))
try:
    pgid = os.getpgid(0)
    ps_output = subprocess.check_output(["ps", "-o", "pid,cmd", "--no-headers", "-g", str(pgid)]).decode().strip()
    if ps_output:
        for line in ps_output.split('\n'):
            pid, cmd = line.strip().split(None, 1)
            print(f"PID: {pid}, Command: {cmd}")
    else:
        print("No processes found in the group.")
except subprocess.CalledProcessError:
    print("Error: Unable to retrieve process information.")

print(colored("Cleaning up...", "red"))
kill_group()
