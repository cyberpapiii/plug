# Plan 013: End-to-end tests for OAuth refresh-under-load and non-stdio supervision recovery

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-test-harness/ plug-core/tests/ plug/tests/`
> On changes, reconcile against the "Current state" facts. Another AI agent
> (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (tests only)
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

Two of plug's highest-value guarantees have no end-to-end test coverage:

1. **Zero-downtime OAuth token refresh** (the PR #42–#50 program): unit tests
   cover the token machinery, and an integration test proves a server with
   valid credentials starts healthy — but no test exercises the full arc
   *token expires → background refresh fires → in-flight and subsequent
   tools/call succeed without an auth error*. This is the feature's entire
   point, and it currently rests on manual verification.
2. **Supervision recovery for non-stdio transports**: there is a good e2e
   test for stdio crash-restart (`test_stdio_crash_restart_recovers_cleanly`)
   but nothing equivalent for HTTP upstreams — kill the upstream HTTP server,
   let health monitoring notice, restart it, verify calls flow again. The
   reconnect path (engine.rs `do_reconnect`) is one of the least-tested
   hot paths (and plans 011/012 are about to modify its surroundings — this
   coverage protects them too).

**This plan adds tests only. Zero production-code changes** (a test-harness
addition is allowed — the harness crate exists for exactly this).

## Current state

Verified at commit `e341625`.

Exemplars to pattern-match (both in the workspace's integration test file —
locate with `grep -rn 'test_stdio_crash_restart_recovers_cleanly\|test_oauth_stateless_http_server' plug-core/tests plug/tests plug-test-harness`; at planning
time they are in the main integration test file `integration_tests.rs`):

- `test_stdio_crash_restart_recovers_cleanly` (`integration_tests.rs:1765`):
  uses a `mock-wrapper.sh` crash script — a stdio server that dies on cue —
  then asserts the supervisor restarts it and tools/call succeeds. This is
  the structural model for the supervision test.
- `test_oauth_stateless_http_server_with_valid_credentials_starts_healthy`
  (`integration_tests.rs:3267`): uses `MockStatelessOauthProvider`
  (`integration_tests.rs:192`) — an in-process mock OAuth token endpoint —
  plus a mock HTTP MCP upstream requiring bearer auth. This is the structural
  model for the refresh test. Read the provider: it can mint tokens with
  chosen expiries.
- OAuth tests serialize behind `oauth_integration_test_lock` (a shared
  mutex/static in the test file) because credential storage is
  process-global — every new OAuth test MUST take it.
- The harness crate `plug-test-harness/` provides mock upstream servers —
  read its `src/` top-level to see what HTTP mock capabilities exist
  (bearer-auth checking, kill/restart support) before building anything new.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Run just the new tests | `cargo test --workspace oauth_refresh_under_load` / `cargo test --workspace http_upstream_supervision` | pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- The integration test file containing the two exemplars (new tests appended
  near their models).
- `plug-test-harness/` — additions ONLY if the existing mocks lack a needed
  capability (e.g. programmatic kill/restart of the mock HTTP server, or
  token-expiry knobs).

**Out of scope** (do NOT touch):
- ALL production code (`plug-core/src`, `plug/src`) — if a scenario can't be
  driven, that's a STOP condition, not a hook-adding license.
- The stdio crash-restart test and `MockStatelessOauthProvider` — extend by
  composition, don't modify, unless a strictly additive knob (new optional
  constructor parameter) is required; never change existing behavior.
- Timing/retry constants in production supervision.

## Git workflow

- Branch: `test/oauth-supervision-e2e`
- Commits: `test(oauth): cover expiry→refresh→call under load`,
  `test(engine): cover http upstream crash-restart supervision`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Map the harness capabilities

Read `MockStatelessOauthProvider` (`integration_tests.rs:192` region) and the
`plug-test-harness` mock HTTP upstream. Answer: (a) can the provider mint a
token that expires in ~1–2s? (b) does the mock HTTP upstream validate the
bearer token per-request against the provider's current token? (c) can the
mock HTTP upstream be stopped and restarted on the SAME port (needed for
supervision — port reuse via holding the listener config)? Write the answers
in the test module comment.

**Verify**: answers (a)–(c) determined; missing capabilities listed.

### Step 2: OAuth refresh-under-load e2e

> **Reviewer ruling (2026-07-12, at execution): DEFERRED behind plan 014.**
> Execution STOPPED here, correctly: `MIN_EXPIRES_IN = 60`
> (`plug-core/src/oauth.rs:35`) clamps any provider-supplied `expires_in` up to
> 60s, and the short-lived 50% rule puts the earliest background refresh ~30s
> after token receipt — two observed refresh windows need ~60s+ of wall clock,
> past this plan's own STOP threshold. This is the production constraint plan
> 014's paused/mocked-time work removes; re-attempt this step only after 014
> lands. Step 3 was executed and merged separately (see the in-code deferral
> note atop the new test section in `plug-core/tests/integration_tests.rs`).
>
> **Final adjudication (2026-07-12, after plan 014 landed): step 2 is NOT
> achievable as a tests-only change — do not re-attempt under this plan.**
> Plan 014's landed scope (paused time for in-memory-only test sleeps) does
> not unblock it, for two verified reasons: (1) refresh scheduling is
> SystemTime-based — `token_needs_refresh` / `time_until_refresh_window`
> take unix-second timestamps (`plug-core/src/oauth.rs:85,105`, consumed by
> the engine refresh loop at `plug-core/src/engine.rs:907`) — so paused
> tokio time cannot accelerate it, and the flow crosses real HTTP sockets,
> where paused time causes spurious timeouts (the exact reason plan 014
> classified real-I/O sites as non-convertible); (2) `MIN_EXPIRES_IN = 60`
> (`oauth.rs:35`) plus the 50% short-lived rule puts two observed refresh
> windows at ≥60s wall clock, past this plan's own ~15s STOP threshold.
> Prerequisite for any future attempt: a separate, small production-side
> plan making the refresh scheduler's clock injectable (or the expiry floor
> test-configurable); only then does a refresh-under-load e2e become
> writable within budget.

New test `test_oauth_refresh_under_load_no_auth_errors` (take
`oauth_integration_test_lock`):

1. Provider mints an access token with ~2s expiry + a refresh token.
2. Start the engine with the mock HTTP upstream requiring the CURRENT bearer.
3. Fire a loop of tools/call round-trips spanning ≥2 refresh windows (e.g.
   calls every 100ms for 5s — real time; refresh timers likely don't run
   under paused time because the token endpoint is real HTTP — confirm; keep
   total under ~8s).
4. Assert: zero auth-failure responses across all calls; the provider's
   token-endpoint hit count ≥2 (refresh actually happened — not one long
   token); every call after the first refresh used the NEW token (provider
   tracks last-seen bearer if the mock supports it — otherwise assert hit
   count only and note it).

**Verify**: `cargo test --workspace test_oauth_refresh_under_load` → passes,
runtime < ~10s.

### Step 3: HTTP upstream supervision e2e

New test `test_http_upstream_crash_restart_recovers_cleanly`, modeled on the
stdio exemplar at `:1765`:

1. Start a mock HTTP MCP upstream (no OAuth — isolate supervision from auth);
   engine with health monitoring enabled at the fastest configurable cadence
   the test config allows (read how the stdio test configures monitor
   intervals).
2. Verify a tools/call succeeds.
3. Kill the mock server (drop its task/listener). Verify a call now fails or
   the server is marked unhealthy (poll engine health state the same way the
   stdio test does).
4. Restart the mock on the same port. Poll until healthy (bounded ~15s,
   sleep-poll pattern copied from the stdio test).
5. Assert tools/call succeeds again and (if the health API exposes it) the
   status reflects a recorded recovery.

**Verify**: `cargo test --workspace test_http_upstream_crash_restart` →
passes; 3 consecutive runs green (`for i in 1 2 3; do cargo test --workspace test_http_upstream_crash_restart || break; done`).

### Step 4: Full-suite check

**Verify**: `cargo test --workspace` → all pass; `cargo clippy --workspace --all-targets -- -D warnings` → exit 0; `cargo fmt --check` → exit 0.

## Test plan

This plan IS the test plan (two e2e tests + any additive harness knobs).

## Done criteria

- [ ] Both new tests pass; supervision test green 3× consecutively
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] No production-code diffs (`git diff --stat` shows only test files / harness)
- [ ] Harness changes (if any) are strictly additive (no existing mock behavior changed)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Step 1 reveals the mock HTTP upstream CANNOT be restarted on the same port
  (e.g. port is chosen by the OS at bind and the harness has no fixed-port
  mode) — report the harness gap; adding a fixed-port mode is allowed if
  additive, but if it requires restructuring the harness, report first.
- The refresh timer cannot fire within test-scale time (e.g. minimum refresh
  margin is minutes and not configurable from tests) — report the constant's
  location (`grep -rn 'refresh' plug-core/src/oauth.rs | grep -i 'margin\|before\|expiry'`); do NOT change production constants.
- The refresh test needs > ~15s wall clock to be reliable — report the
  timing budget conflict instead of merging a slow test.
- Either test flakes in the 3× loop — report the failure mode (this is
  exactly the signal plan 014's paused-time work wants).

## Maintenance notes

- Plans 011/012 modify `do_reconnect`/`replace_server`/`shutdown_all` — the
  supervision test from this plan is their regression guard; run it in their
  review.
- If PAGE/monitor cadences change in config defaults, the poll bounds here
  may need matching updates — the bounds are named consts at the top of each
  test for that reason (write them that way).
- The refresh test intentionally uses real time; do not convert it to paused
  time without confirming the refresh timer and axum mock both honor it.
