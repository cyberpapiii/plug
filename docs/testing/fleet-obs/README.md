# Fleet observability stage

Run the Phase 0 observability floor with:

```bash
scripts/fleet-truth.sh obs
```

The stage reuses the mock-upstream workload from `scripts/fleet/load.py` and
requires every run to emit:

- a cumulative latency histogram;
- an error taxonomy;
- maximum, final, and sampled in-flight counts;
- aggregate RSS and file-descriptor samples for the Plug daemon and clients;
- a zero-byte stderr assertion.

The stage fails when any signal is absent, stderr is non-empty, or the workload
records an error. Linux `/proc` is intentionally required for RSS and FD
sampling; an environment that cannot expose those measurements fails closed.

Defaults are two sessions for five seconds. Override them with
`FLEET_OBS_SESSIONS` and `FLEET_OBS_DURATION_SECS`.

## Negative proof

`FLEET_OBS_OMIT_SIGNAL` removes a named signal after collection so the
fail-closed path can be exercised without changing production behavior:

```bash
FLEET_OBS_DURATION_SECS=1 \
FLEET_OBS_OMIT_SIGNAL=fd_samples \
scripts/fleet-truth.sh obs
```

This command must exit non-zero and report `MISSING required signal:
fd_samples`.

## Default `all` policy

`scripts/fleet-truth.sh all` keeps `obs` opt-in. The stage builds binaries,
runs a concurrent workload, and depends on Linux process telemetry, so adding
it to the fast default gate would change both runtime and platform assumptions.
The `all` report prints an explicit `STAGE obs SKIP` line.
