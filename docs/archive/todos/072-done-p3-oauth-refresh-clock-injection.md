---
status: done
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

- [x] Refresh scheduling reads time through an injectable seam, with production defaulting
      to `SystemTime::now` and the existing floors unchanged
- [x] `test_oauth_refresh_under_load_no_auth_errors` exists and observes at least two
      refresh cycles with calls in flight
- [x] The deferral note at `plug-core/tests/integration_tests.rs:2213-2219` is removed

## Resources

- `plug-core/src/oauth.rs`
- `plug-core/tests/integration_tests.rs:2205-2219`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Confirmed the plan 013 step 2 residual is unchanged on `main` and gave it a tracked home.
No code change made — Option A is a real refactor of a security-adjacent module and wants
a deliberate decision rather than a drive-by.

### 2026-08-08 - Resolved

**By:** Claude Fable 5

Took Option A, in the narrowest form that still makes the test honest.

**The seam.** `plug-core/src/oauth.rs` gained a private `unix_now()` that reads
`SystemTime::now()` and adds a process-global `TEST_CLOCK_SKEW_SECS`, plus a
`#[doc(hidden)] pub fn advance_test_clock(secs)` that only ever adds to it. All four
production wall-clock reads in the expiry and refresh-scheduling paths — `token_needs_refresh`,
`time_until_refresh_window`, `update_cache`, and `remaining_token_lifetime_secs` — now go
through it. `MIN_EXPIRES_IN`, `MAX_EXPIRES_IN`, and `SHORT_LIVED_THRESHOLD` are untouched.

The seam is deliberately additive and forward-only rather than a settable clock. The skew can
only make a token look *older* than it is, so the worst a stuck or hostile value can do is
refresh earlier than necessary. There is no value it can hold that makes an expired token look
fresh, which is the property that matters for a security-adjacent module carrying a test hook.
That is also why there is no reset: a `reset` or a settable clock would be the direction that
can move time backwards.

**The test.** `test_oauth_refresh_under_load_no_auth_errors` stands up a mock authorization
server plus a mock MCP upstream sharing one state object, connects the engine, then drives
`call_tool` in a loop until the mock has granted at least two refreshes, asserting that no call
came back `401` and no call failed.

Two things about its timing are worth keeping in mind before editing it:

1. It calls `tokio::time::pause()` *after* `engine.start()`, not via `start_paused = true`. A
   paused Tokio clock jumps to the earliest pending deadline whenever the runtime goes idle,
   and engine startup does real loopback I/O — so under `start_paused` every connect-path timer
   fires during the first socket read and startup fails outright (`health: Failed`, zero tools,
   0.02s). Only the refresh cadence needs virtualizing.
2. Pausing Tokio alone is not enough, which is the whole reason the seam exists: token expiry is
   measured against `SystemTime`, which `tokio::time::pause()` does not affect. A background task
   sleeps 1 virtual second and calls `advance_test_clock(1)` to keep the two clocks in step.

**Non-vacuity checked.** The mock keeps previously issued access tokens valid after a refresh,
which is what a real authorization server does. Patched temporarily to invalidate them eagerly
instead, the test fails with `left: 7, right: 0` unauthorized calls — so the assertion has teeth,
and the measured boundary is worth stating plainly: plug's zero-downtime refresh holds because
providers keep the old access token alive across the refresh window, not because plug never emits
a call with a token that is about to be replaced.

**Residual.** `advance_test_clock` is process-global and the accumulated skew outlives the test
that set it. Because it is forward-only, the only effect on a later OAuth test in the same binary
is that tokens look older and refresh sooner, never fresher. Verified with the full
`integration_tests` binary green.

Runs in ~0.6s, stable across three consecutive runs.
