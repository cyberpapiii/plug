# Plan 006: Characterization tests for the IPC stdio proxy (`ipc_proxy.rs`) before touching its reconnect internals

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug/src/ipc_proxy.rs plug/src/runtime.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P1 (prerequisite for plans 007 and 009)
- **Effort**: M
- **Risk**: LOW (additive tests only)
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

`plug/src/ipc_proxy.rs` is the adapter every local `plug connect` stdio
client (the Claude Code path — this project's primary consumer) runs through:
2,521 lines with only 10 tests, the thinnest-tested large module on the
hottest path. Its incident history is real: todos/001 (tool-call correlation
bug) and todos/064 (task ownership lost across daemon reconnect) were both
found live, not by tests. Plans 007 (reconnect state replay) and 009 (read
watchdog) will rewire this file's reconnect internals; this plan pins current
behavior first so those changes can be reviewed against a green
characterization baseline.

**This plan adds tests only. Zero production-code changes.**

## Current state

- `plug/src/ipc_proxy.rs` — test module starts at `:1324`; the 10 existing
  `#[tokio::test]`s are at `:1498, :1644, :1791, :1900, :2053, :2147, :2221,
  :2298, :2410` (+1 more). The test at `:1498` covers a daemon-restart
  reconnect but only asserts tool listing afterward. Read the whole test
  module first — it contains the harness helpers you will reuse (in-process
  daemon spawning / socket fixtures).
- Key production seams to characterize (verified excerpts):
  - `session_round_trip` (`:88-127`): takes the single global
    `shared.conn` mutex, writes a frame, and on a reconnectable failure calls
    `reconnect_locked` then either retries (RetryPolicy::SafeToRetry, with the
    request REBUILT against the new session id) or returns
    `REQUEST_RETRY_UNSAFE` (UnsafeToRetry).
  - `try_round_trip_locked` (`:128-155+`): reads frames in a loop; the daemon
    may interleave push notifications and reverse requests with the response;
    notifications are forwarded to the downstream peer, reverse requests
    handled inline; chunked responses are reassembled (`chunked_response`,
    `expected_chunks`).
  - `refresh_session` / `refresh_session_locked` (`:375-410`): re-establish
    via `establish_daemon_proxy_session(config_path, client_id, client_info)`
    — which (per `plug/src/runtime.rs:541-586`) sends ONLY `Register` +
    `Capabilities`. Known consequence (bug, to be fixed by plan 007): client
    capabilities (`UpdateCapabilities`, sent once at `:560-572` during
    initialize), resource subscriptions, and log level are NOT replayed after
    reconnect.
  - Heartbeat drives reconnects (~1s cadence; see the heartbeat task around
    `:357-368`).
- The daemon side lives in `plug/src/daemon.rs` (its own tests from `:2637`
  show how to run an in-process daemon against a temp socket — reuse that
  pattern if ipc_proxy's module lacks one).
- Conventions: `#[tokio::test]` in `#[cfg(test)] mod tests`; tests that need
  the process-global runtime-paths slot serialize behind a shared lock (the
  parallel-test work in PR #62 introduced this — look for the lock in the
  daemon/runtime test helpers, e.g. `grep -rn 'runtime_paths_lock\|paths_lock' plug/src plug-core/src`,
  and take it in any test that touches runtime paths).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-mcp ipc_proxy` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug/src/ipc_proxy.rs` — the `#[cfg(test)] mod tests` block ONLY (new tests + new test helpers).
- If a shared helper genuinely must live elsewhere, `plug/src/daemon.rs` test module only.

**Out of scope** (do NOT touch):
- ALL production code in `ipc_proxy.rs`, `runtime.rs`, `daemon.rs` — if a
  behavior can't be tested without a production hook, document it in the
  completion report instead of adding the hook.
- `plug-core` — no changes.

## Git workflow

- Branch: `test/ipc-proxy-characterization`
- Commits: `test(ipc-proxy): characterize reconnect, retry-policy, and frame handling` (split by scenario if large).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read and map the existing harness

Read `plug/src/ipc_proxy.rs:1324-1500` (helpers) and the test at `:1498`
(daemon-restart reconnect). Identify: how a test daemon is started, how the
proxy is pointed at it, how frames are observed. Write a one-paragraph
summary comment at the top of your new test section describing the harness
(future executors of plans 007/009 will read it).

**Verify**: `cargo test -p plug-mcp ipc_proxy` → the existing 10 tests pass (baseline green).

### Step 2: Characterize reconnect state (the plan-007 baseline)

Add tests:

1. `reconnect_reregisters_with_register_and_capabilities_only` — restart the
   test daemon mid-session, trigger a round-trip so the proxy reconnects, and
   assert (by inspecting the daemon side's received request sequence, or the
   registry state) that the new session was established via
   Register+Capabilities and that the daemon-side session's client
   capabilities are the DEFAULT (empty) — i.e. pin the current
   capability-loss behavior. Mark with a comment:
   `// CHARACTERIZATION: current behavior loses client caps on reconnect — plan 007 will change this assertion.`
2. `retry_policy_safe_rebuilds_against_new_session` — force a reconnectable
   failure (close the daemon-side socket) under `RetryPolicy::SafeToRetry`
   and assert the retried frame carries the NEW session id.
3. `retry_policy_unsafe_surfaces_retry_error` — same failure under
   `UnsafeToRetry`; assert the error message contains `REQUEST_RETRY_UNSAFE`.

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass, 3 new tests.

### Step 3: Characterize interleaved-frame handling

Add tests driving `try_round_trip_locked`'s read loop:

4. `notifications_interleaved_before_response_are_forwarded` — daemon sends a
   logging notification frame, then the response; assert the response is
   returned AND the notification reached the downstream peer hook.
5. `chunked_response_reassembly` — daemon sends a chunked response (use the
   daemon's real chunking helper via a large payload, or hand-build frames if
   the harness allows); assert reassembly returns the full logical response.
6. `malformed_frame_is_reconnectable_failure` — daemon sends garbage bytes /
   an oversized frame header; assert the proxy surfaces a transport failure
   and recovers via reconnect on the next call (not a hang, not a panic).

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass, 6 new tests total.

### Step 4: Characterize the wedge (the plan-009 baseline)

7. `silent_daemon_stall_currently_hangs` — daemon accepts a request and never
   responds (add a stall mode to the TEST daemon helper, not production).
   Current behavior: the round-trip never completes. Assert it via
   `tokio::time::timeout(Duration::from_secs(2), round_trip)` returning
   `Err(Elapsed)`, with the comment
   `// CHARACTERIZATION: no read watchdog today — plan 009 will turn this into a reconnectable failure.`
   Keep the test fast: it must not sleep real 2s if the harness supports
   paused time; if the socket I/O prevents paused time, keep the timeout ≤2s.

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass, 7 new tests total. Then `cargo test --workspace` → all pass.

## Test plan

This plan IS the test plan. Structural pattern: the existing restart test at
`ipc_proxy.rs:1498`. All new tests live in the same module, named as in the
steps, each with a one-line comment stating what behavior it pins and which
follow-up plan (007/009) may legitimately change it.

## Done criteria

- [ ] `cargo test -p plug-mcp ipc_proxy` exits 0 with ≥7 new tests (17+ total in the module)
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `git diff e341625..HEAD --stat -- plug/src/ipc_proxy.rs` shows changes ONLY inside the test module (verify: `git diff e341625..HEAD -- plug/src/ipc_proxy.rs | grep '^-' | grep -v '^---'` contains no removed production lines)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- A scenario can't be driven without modifying production code (no test
  hooks exist for it) — list the blocked scenarios and what hook each would
  need; plans 007/009 can absorb that.
- The existing harness spawns a REAL daemon binary (not in-process) and a
  scenario (e.g. stall mode) can't be induced — same reporting rule.
- Any EXISTING test fails at step 1 — the baseline is already broken; report
  before writing anything.
- Tests are flaky across 3 consecutive runs (`for i in 1 2 3; do cargo test -p plug-mcp ipc_proxy || break; done`) — report the flaky test instead of adding sleeps.

## Maintenance notes

- Plans 007 and 009 MUST update the two `CHARACTERIZATION:` tests they
  invalidate (capability loss; stall hang) — that's by design, the comments
  say so.
- The daemon-stall test helper (step 4) is deliberately test-only; if plan
  009 wants a production stall-injection point, that's its decision.
- Reviewer focus: tests must assert on observable wire/registry behavior,
  not on private internals that plans 007/009 will restructure.
