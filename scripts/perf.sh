#!/usr/bin/env bash
# Measure the journeys Plug owns, against the installed app and its daemon.
#
#   scripts/perf.sh                 live connect journey plus field numbers
#   scripts/perf.sh --runs 10       more connect samples (default 5)
#   scripts/perf.sh --days 3        field window in daily log files (default 1)
#   scripts/perf.sh --call TOOL     also time a read-only tool call end to end
#
# Live journey: spawn `plug connect`, initialize, tools/list twice, ping, and
# optionally one tool call. Each sample is a fresh connector, as a host would
# start one. Field numbers come from the daemon logs: tool call latency per
# server (upstream time; Plug's own share is the live ping and call overhead),
# daemon startup until every server is up, and the gap a daemon swap leaves
# between the old process's SIGTERM and the new one starting.
#
# Read-only: it never starts, stops, or restarts the daemon.
set -euo pipefail

runs=5
days=1
call=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --runs) runs="$2"; shift 2 ;;
    --days) days="$2"; shift 2 ;;
    --call) call="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

plug="${PLUG_BIN:-$HOME/.local/bin/plug}"
logs="$HOME/Library/Logs/plug"

exec python3 - "$plug" "$logs" "$runs" "$days" "$call" <<'PY'
import collections, glob, json, os, subprocess, sys, time

plug, logs, runs, days, call = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]

def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * len(values)))] if values else float("nan")

def row(label, values, unit="ms"):
    print(f"  {label:<28} p50 {pct(values, .5):8.1f}  p75 {pct(values, .75):8.1f}  "
          f"max {max(values) if values else float('nan'):8.1f} {unit}  (n={len(values)})")

def connect_sample():
    t0 = time.perf_counter()
    proc = subprocess.Popen([plug, "connect"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True)
    def send(msg):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
    def recv(ident):
        for line in proc.stdout:
            msg = json.loads(line)
            if msg.get("id") == ident:
                return msg, len(line)
        raise RuntimeError("plug connect closed stdout")
    sample = {}
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "plug-perf", "version": "0"}}})
    recv(1)
    sample["ready"] = (time.perf_counter() - t0) * 1e3
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    for key, ident in (("tools/list first", 2), ("tools/list again", 3)):
        start = time.perf_counter()
        send({"jsonrpc": "2.0", "id": ident, "method": "tools/list", "params": {}})
        response, size = recv(ident)
        sample[key] = (time.perf_counter() - start) * 1e3
        sample["tools"] = len(response["result"]["tools"])
        sample["bytes"] = size
    pings = []
    for i in range(20):
        start = time.perf_counter()
        send({"jsonrpc": "2.0", "id": 100 + i, "method": "ping"})
        recv(100 + i)
        pings.append((time.perf_counter() - start) * 1e3)
    sample["ping"] = pct(pings, .5)
    if call:
        start = time.perf_counter()
        send({"jsonrpc": "2.0", "id": 200, "method": "tools/call",
              "params": {"name": call, "arguments": {}}})
        recv(200)
        sample["call"] = (time.perf_counter() - start) * 1e3
    start = time.perf_counter()
    proc.stdin.close()
    proc.wait(timeout=10)
    sample["exit"] = (time.perf_counter() - start) * 1e3
    return sample

print(f"Live connect journey ({runs} fresh connectors)")
samples = [connect_sample() for _ in range(runs)]
for key in ("ready", "tools/list first", "tools/list again", "ping", "call", "exit"):
    values = [s[key] for s in samples if key in s]
    if values:
        row({"ready": "spawn to initialize", "exit": "stdin close to exit"}.get(key, key), values)
print(f"  tools/list: {samples[-1]['tools']} tools, {samples[-1]['bytes'] / 1e6:.2f} MB")

files = sorted(glob.glob(os.path.join(logs, "plug.log.*")))[-days:]
events = []
for path in files:
    with open(path, errors="replace") as handle:
        for line in handle:
            try:
                events.append(json.loads(line))
            except ValueError:
                pass

print(f"\nField ({len(files)} daily log file(s))")
by_server = collections.defaultdict(list)
for event in events:
    fields = event.get("fields", {})
    if fields.get("message") == "proxy tool call completed" and "duration_ms" in fields:
        by_server[fields.get("server", "?")].append(fields["duration_ms"])
everything = [v for values in by_server.values() for v in values]
if everything:
    row("tool calls, all servers", everything)
    for server, values in sorted(by_server.items(), key=lambda kv: -len(kv[1]))[:8]:
        row(f"  {server}", values)

def stamp(event):
    ts = event["timestamp"].rstrip("Z")
    main, _, frac = ts.partition(".")
    return time.mktime(time.strptime(main, "%Y-%m-%dT%H:%M:%S")) + float("0." + (frac or "0"))

startups, swaps = [], []
started_at, sigterm_at = None, None
for event in events:
    message = event.get("fields", {}).get("message", "")
    if message == "received SIGTERM":
        sigterm_at = stamp(event)
    elif message == "daemon started":
        started_at = stamp(event)
        if sigterm_at is not None and started_at - sigterm_at < 120:
            swaps.append((started_at - sigterm_at) * 1e3)
        sigterm_at = None
    elif message == "server startup complete" and started_at is not None:
        startups.append((stamp(event) - started_at) * 1e3)
        started_at = None
if startups:
    row("daemon start to all servers", startups)
if swaps:
    row("swap: SIGTERM to new daemon", swaps)
PY
