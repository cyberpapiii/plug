# Fleet load baseline

Recorded on 2026-08-07 from `feat/fleet-truth-p0-04-load` in the Linux cloud
harness with Rust 1.97.1. The upstream was the local
`plug-test-harness` stdio mock; no network or OAuth vendor was used.

## Production-bar smoke

Command:

```bash
scripts/fleet-truth.sh load
```

Result:

```text
sessions         2
duration         300s
calls            322749 (success=322749 errors=0)
latency          p50=1.83ms p95=2.20ms p99=2.62ms
error rate       0.000%
STAGE load PASS
```

The measured p95, p99, and error rate were below the default gates of 250 ms,
1,000 ms, and 1%, respectively.

## Required short smoke

Command:

```bash
FLEET_LOAD_DURATION_SECS=30 FLEET_LOAD_SESSIONS=2 \
  scripts/fleet-truth.sh load
```

Result:

```text
sessions         2
duration         30s
calls            33073 (success=33073 errors=0)
latency          p50=1.79ms p95=2.10ms p99=2.41ms
error rate       0.000%
STAGE load PASS
```

## Suite policy

The full five-minute load gate is opt-in. The default `all` command remains
fast and prints an explicit load-stage skip. Short smoke runs use
`FLEET_LOAD_DURATION_SECS` and `FLEET_LOAD_SESSIONS`; the production-bar
defaults remain two sessions for five minutes.
