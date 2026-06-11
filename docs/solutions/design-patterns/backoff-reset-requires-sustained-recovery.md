---
title: "Backoff is defeated if you reset it on transient health — gate the reset on sustained recovery"
date: "2026-06-11"
category: design-patterns
module: plug-core/upstream-supervision
problem_type: design_pattern
component: background_job
severity: high
applies_when:
  - "Implementing exponential backoff for a restart, retry, or reconnect governor"
  - "A remediation action transiently restores the same signal used to detect health"
  - "Resetting a backoff or attempt counter based on current state rather than a sustained window"
  - "Supervising a flapping dependency that a restart only temporarily fixes"
tags:
  - backoff
  - exponential-backoff
  - restart-supervision
  - circuit-breaker
  - health-check
  - flapping
---

# Backoff is defeated if you reset it on transient health — gate the reset on sustained recovery

## Context

The `plug` daemon (an MCP multiplexer) added active upstream supervision (item 2b): if an upstream MCP server stays degraded past a threshold, the supervisor restarts it. To stop a perpetually-failing upstream from storming with restarts, each supervised restart is bounded by an **exponential inter-episode backoff** — a per-server attempt counter that grows on every restart, producing `required_wait = (min_restart_interval * 2^attempts).min(max_restart_interval)`. The counter is reset to zero when the upstream returns to `Healthy`, so a server that genuinely recovers starts fresh next time.

The intended target is the **flapping / partial-recovery** upstream: a process that a restart *helps briefly* but that then re-degrades — e.g. the iMessage server's Swift continuation leak (auto memory [claude]), which works for a few minutes after a bounce and then rots again. The backoff is meant to space that server's restarts out over time instead of bouncing it every threshold-interval forever.

This learning was caught by an adversarial code review (rated P1) **before merge**, not in production. It generalizes well beyond MCP supervision — the trap applies to any backoff/retry/restart/circuit-breaker governor.

## Guidance

**Gate the backoff *reset* on a sustained-success window, not a single success sample.** The trap: the recovery action itself transiently restores the very signal the reset is gated on. In `plug`, a supervised restart goes through `replace_server`, which resets the upstream's health to `Healthy` and its circuit to `Closed`. So the very next health-check tick observes `Healthy` and resets the backoff counter to zero — *before the upstream has demonstrated it actually recovered*. For a flapping upstream: restart → brief `Healthy` blip → counter reset → re-degrade → restart at the **floor** interval again. The exponential backoff never compounds; it only ever bites upstreams whose *reconnect keeps failing* (which never go `Healthy`, so never reset) — the opposite of the flapping case the feature was built for.

The root error is **conflating "currently healthy" with "recovered."** A single success sample taken right after the recovery action is not evidence of recovery; it is an artifact of the action.

Introduce a `settled` predicate and gate the reset on it (directional Rust, not a literal spec):

```rust
// BEFORE — resets on a single healthy sample (the restart produces that sample)
if health == Healthy && !circuit_open {
    reset_supervision(name);
}

// AFTER — reset only once recovery is *stable*
if health == Healthy && !circuit_open && supervision_settled(name) {
    reset_supervision(name);
}

fn supervision_settled(name) -> bool {
    match last_restart_epoch(name) {
        None => true,                                   // never restarted; nothing to settle
        Some(t) => now() - t >= max_restart_interval_secs,
    }
}
```

Choosing `max_restart_interval_secs` as the settle window is deliberate: a server is "settled" only once it has stayed healthy for at least as long as the worst-case backoff it would otherwise have been waiting. A flapping upstream re-degrades well inside that window, so its attempt counter keeps doubling toward the cap; only a genuinely sustained recovery clears it.

Keep the decision **pure and unit-testable** — factor the policy out of the IO so the flapping case can be tested directly:

```rust
fn should_restart(
    health: Health,
    consecutive_failures: u32,
    circuit_open: bool,
    secs_since_last_restart: Option<u64>,
    attempts: u32,
) -> bool {
    let required_wait = min_restart.saturating_mul(2u64.saturating_pow(attempts.min(16))).min(max_restart);
    let eligible = (matches!(health, Degraded | Failed) && consecutive_failures >= threshold) || circuit_open;
    eligible && secs_since_last_restart.map_or(true, |s| s >= required_wait)
}
```

Two supporting fixes hardened the same mechanism:

1. **Unify restart accounting.** Every recovery episode — crash/`Failed` recovery *and* supervised restart — stamps the same restart clock. Otherwise a crash-recovery doesn't suppress an immediate supervised restart, and the restart metric undercounts real restarts. One clock, stamped by every path that actually restarts the server.
2. **Reject a zero floor in config validation.** `min_restart_interval_secs == 0` silently disables the backoff entirely, because `0 * 2^n == 0` — `required_wait` is always zero no matter how high `attempts` climbs. Validate `min_restart_interval_secs > 0` at config load.

## Why This Matters

Backoff is the only thing standing between a sick dependency and a restart storm. When the reset is mis-gated, the backoff silently degrades into a no-op for *precisely the failure mode it was designed to contain* — and it does so invisibly, because a flapping server *looks* healthy at every sample the governor takes. The system appears to be supervising correctly (restarts happen, health recovers) while actually hammering a leaky process at the floor interval indefinitely, masking the underlying rot and burning resources on every bounce.

The bug is also **self-camouflaging**: the persistent-failure case (server never recovers) works correctly, so naive tests pass and the feature looks done. The defect lives entirely in the recover-then-fail path.

## When to Apply

Apply this whenever a governor measures the success signal that its own recovery action produces:

- **Restart supervisors** — the restart resets health/liveness, then the next health check reads that reset value.
- **Retry-with-backoff loops** — a single success after a backed-off attempt resets the delay; a flaky dependency then never accumulates backoff.
- **Circuit breakers** — a half-open probe succeeding once closes the circuit; one lucky probe re-arms full traffic against a still-failing dependency.
- **Adaptive rate limiters / load shedders** — a brief drop in error rate (caused by the shedding itself) lifts the limit prematurely.

The shared smell: *the recovery mechanism perturbs the metric, and the reset condition reads the metric immediately after the perturbation.*

Reset rules to prefer:
- **Time-based settle window** — require N seconds of sustained success since the last action (used here).
- **Consecutive-success count** — require N independent successes, not one.
- Never reset on a single post-action success sample.

And always **test the flapping case explicitly** — `recover → fail → recover → fail`. The persistent-failure case (`fail → fail → fail`) and the clean-recovery case (`fail → recover → stay healthy`) both pass even with the bug present; only the flapping case exposes it. A test suite that omits flapping will green-light a backoff that doesn't back off.

## Examples

**Restart supervisor (this codebase).** Flapping upstream with a continuation leak:

- *Before:* restart → `Healthy` blip → `reset_supervision` → counter back to 0 → restarts at the floor interval every cycle, forever.
- *After:* restart → `Healthy` blip < `max_restart_interval_secs` → `supervision_settled` is false → counter is **not** reset → next restart waits `min * 2^attempts` → backoff compounds toward the cap, as intended.

**Retry loop, generalized.**

```text
Before: success_after_backoff  =>  delay = base        // resets on one success
After:  success_after_backoff  =>  delay unchanged until
        N consecutive successes observed, then delay = base
```

**Circuit breaker, generalized.**

```text
Before: half_open_probe_ok (1 probe)  =>  state = Closed
After:  half_open requires K consecutive probe successes  =>  state = Closed
        (a single lucky probe stays half-open)
```

In every case the fix is the same shape: replace "reset on the success the action just produced" with "reset on a sustained-success window the action cannot fake."

## Related

- `integration-issues/2026-03-18-reload-health-refresh-coalescing.md` — adjacent: a per-server recovery-task claim flag so the health loop launches only one recovery task at a time. Same domain (health-loop ticks over-triggering recovery during flapping), different primitive (debounce + claim flag, not a backoff-counter reset gate). Moderate overlap — consolidation review candidate if a "health-loop recovery discipline" doc emerges.
- `integration-issues/phase3-resilience-token-efficiency.md` — prior art on circuit-breaker `Open → HalfOpen` cycling and "counter reset on recovery" hazards (already labeled historical / rmcp-1.0.0-era).
- Landed on `main` via PR #67 (active upstream supervision); built on the first-class `degraded` state (PR #61) and per-upstream metrics (PR #60).
