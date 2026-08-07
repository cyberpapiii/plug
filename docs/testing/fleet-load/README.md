# Fleet load smoke

The `load` stage holds concurrent downstream MCP sessions open through Plug's
daemon and continuously calls `Mock__echo` on the existing
`plug-test-harness` stdio mock. It does not contact external services or OAuth
vendors.

Run the production-bar smoke explicitly:

```bash
scripts/fleet-truth.sh load
```

The default is two sessions for 300 seconds. The regular
`scripts/fleet-truth.sh all` gate skips load so the default suite stays fast.
Short runs can override the dimensions:

```bash
FLEET_LOAD_DURATION_SECS=30 FLEET_LOAD_SESSIONS=2 \
  scripts/fleet-truth.sh load
```

The runner prints total calls, p50/p95/p99 call latency, and error rate. It
returns non-zero if a session stops early or any default threshold is exceeded:

- p95 latency: 250 ms
- p99 latency: 1,000 ms
- error rate: 1%

Thresholds can be tightened for threshold-gate validation with
`FLEET_LOAD_MAX_P95_MS`, `FLEET_LOAD_MAX_P99_MS`, and
`FLEET_LOAD_MAX_ERROR_RATE_PCT`. Values must be finite and non-negative.

Each run uses isolated XDG runtime and state directories, starts a dedicated
Plug daemon, and removes the daemon and temporary files on exit. This avoids
interacting with a developer's normal Plug daemon.
