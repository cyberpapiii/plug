# Fleet concurrent load smoke

This harness opens one local `plug-test-harness` mock MCP server per session,
synchronizes the sessions after discovery, and continuously calls the mock
`echo` tool. It reports aggregate tool-call latency and errors. Mock startup
and discovery are outside the measured interval.

Run the `dev-smoke-2x5m` gate with its defaults:

```sh
python3 scripts/fleet/load.py
```

Run the short verification mode:

```sh
python3 scripts/fleet/load.py --sessions 2 --duration 10s
```

Durations accept `s`, `m`, or `h`. Session count and thresholds are
configurable:

```sh
python3 scripts/fleet/load.py \
  --sessions 4 \
  --duration 1m \
  --max-p99-ms 300 \
  --max-error-rate 0.01
```

The default limits are p99 latency at or below 250 ms and an error rate at or
below 1%. `--max-error-rate` is a fraction from 0 through 1. A threshold
breach, session failure, or run with no completed calls exits non-zero.
Percentiles use the nearest-rank method over all measured tool-call attempts.

Force a deterministic threshold failure:

```sh
python3 scripts/fleet/load.py --sessions 2 --duration 10s --max-p99-ms 0
```

Run the focused self-check:

```sh
python3 scripts/fleet/test_load.py
```

Wiring this stage into `scripts/fleet-truth.sh` is deferred to P0-07.
