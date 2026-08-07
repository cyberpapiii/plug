# Fleet truth baseline

Recorded at `2026-08-07T22:51:52Z` from commit `f08a6ac`.

Run the Phase 0 fast predicate from the repository root:

```bash
scripts/fleet-truth.sh all
```

Overall result: **PASS**.

| Stage | Policy | Status | Metric | Elapsed |
| --- | --- | --- | --- | --- |
| conformance | required | PASS | 2/2 checks | 0.01s |
| golden | required | PASS | 1 fixture | 0.17s |
| contract | required | PASS | 1 snapshot | 0.18s |
| load | opt-in | SKIP | default 2 × 5m | — |
| fault | opt-in | SKIP | deterministic faults | — |
| obs | opt-in | SKIP | default 2 × 5s | — |

The required predicate is conformance + golden + contract. The load, fault,
and observability stages remain explicit opt-ins because they are slower or
disruptive; their durable stage-specific baselines remain under `docs/testing/`.
