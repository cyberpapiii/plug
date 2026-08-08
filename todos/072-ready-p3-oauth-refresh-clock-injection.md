---
status: ready
priority: p3
issue_id: "072"
tags: [oauth, testing, time, refresh, test-coverage]
dependencies: []
---

# OAuth refresh scheduling has no clock seam, so refresh-under-load cannot be tested

## Problem Statement

Plan 013 step 2 of the 2026-07 improve program was adjudicated **not achievable
tests-only** and left partial. It is the one item from that program that was never
completed. Nothing has changed on `main` since, and it is not tracked anywhere.

The deferred test is `test_oauth_refresh_under_load_no_auth_errors`: drive an upstream
through two background token-refresh cycles while calls are in flight and assert no auth
errors surface. It cannot be written today because refresh timing is bound to real wall
time.

## Findings

Verified on `main` @ `e3b562e` on 2026-08-08.

- `plug-core/src/oauth.rs:41` — `const MIN_EXPIRES_IN: u64 = 60;` clamps every
  provider-supplied `expires_in` upward at `:178`
  (`v.clamp(MIN_EXPIRES_IN, MAX_EXPIRES_IN)`). A test cannot ask for a 2-second token.
- The short-lived rule (`SHORT_LIVED_THRESHOLD = 600`, `:47`) floors the first background
  refresh at roughly 30s of real time per cycle, so two observed refresh windows exceed
  any reasonable integration-test budget.
- There is **no clock abstraction anywhere in the workspace** — a grep for
  `trait Clock`, `MockClock`, `TestClock`, `clock_source` across all `*.rs` returns
  nothing. `oauth.rs` calls `SystemTime::now()` directly at `:197`, `:217`, `:1022`,
  `:1094` and in tests at `:1795`, `:1805`, `:1817`, `:1831`, `:1843`, `:2090`.
- The deferral is recorded in the test file itself at
  `plug-core/tests/integration_tests.rs:2213-2219`, which points at "time-control work
  (plan 014)". Plan 014 shipped (`0feb776`, paused-time de-flake for in-memory sleep
  tests) but covers tokio-sleep tests, not `SystemTime`-based OAuth expiry — so the
  dependency it named is satisfied without unblocking this.

## Proposed Solutions

### Option A: inject a clock into the OAuth store (recommended)

Introduce a narrow time source (a `Clock` trait or an injected `fn() -> SystemTime`) used
by the expiry and refresh-scheduling paths in `oauth.rs`, defaulting to `SystemTime::now`.
Tests substitute a controllable clock.

**Pros:** unblocks the test without weakening any production floor
**Cons:** touches a security-adjacent module; every `SystemTime::now()` in the refresh
path has to go through the seam or the test lies
**Effort:** medium **Risk:** medium

### Option B: make the floors configurable behind a test-only knob

Expose `MIN_EXPIRES_IN` / `SHORT_LIVED_THRESHOLD` as overridable in `#[cfg(test)]` or via
a hidden config field.

**Pros:** much smaller change
**Cons:** a production constant becomes reachable from config, and a real deployment could
end up with a 2-second floor; the test then exercises timings no real provider produces
**Effort:** small **Risk:** medium-high

### Option C: leave it

Accept that refresh-under-load is covered only by the unit-level refresh tests and the
mock-OAuth integration coverage already on `main` (metadata discovery, auth-code exchange,
refresh persistence, reconnect with refreshed credentials).

**Pros:** zero risk **Cons:** the concurrency property the test was meant to prove stays
unproven

## Acceptance Criteria

- [ ] Refresh scheduling reads time through an injectable seam, with production defaulting
      to `SystemTime::now` and the existing floors unchanged
- [ ] `test_oauth_refresh_under_load_no_auth_errors` exists and observes at least two
      refresh cycles with calls in flight
- [ ] The deferral note at `plug-core/tests/integration_tests.rs:2213-2219` is removed

## Resources

- `plug-core/src/oauth.rs`
- `plug-core/tests/integration_tests.rs:2205-2219`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Confirmed the plan 013 step 2 residual is unchanged on `main` and gave it a tracked home.
No code change made — Option A is a real refactor of a security-adjacent module and wants
a deliberate decision rather than a drive-by.
