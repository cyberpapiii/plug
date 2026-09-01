# Plan 009: Add a read watchdog to the IPC proxy so a silent daemon stall cannot wedge a client forever

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug/src/ipc_proxy.rs`
> If the file changed since this plan was written (it WILL have, if plans 006
> and 007 landed — that's expected; their changes are the baseline you build
> on), compare the "Current state" excerpts against the live code before
> proceeding; on an unexplained mismatch, treat it as a STOP condition.
> Another AI agent (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: S/M
- **Risk**: MEDIUM (a wrong timeout aborts healthy long calls)
- **Depends on**: **Plan 006 MUST be merged** (it contains the
  `silent_daemon_stall_currently_hangs` characterization test this plan
  flips). Plan 007 should land first too (both edit the same functions;
  sequential order 006 → 007 → 009 avoids conflicts).
- **Category**: correctness
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

`session_round_trip` in `plug/src/ipc_proxy.rs` writes a frame and then reads
frames in a loop (`try_round_trip_locked`) until the matching response
arrives. Failure detection is connection-based: if the socket ERRORS or
CLOSES, reconnect logic kicks in. But if the daemon process is alive and the
socket healthy while the daemon never responds — wedged on the macOS Keychain
prompt (a documented real failure mode in this project's memory: a daemon
started outside a login session hangs forever on the Keychain OAuth read),
deadlocked internally, or stuck on a hung upstream — the read loop waits
forever **while holding the single global `shared.conn` mutex**. One wedged
call therefore wedges EVERY subsequent request from that client: the proxy is
fully hung, and because `plug connect` clients look like unresponsive MCP
servers, the host app (Claude Code) just spins.

There is no timeout anywhere on this read path today (verified: no
`tokio::time::timeout` in the round-trip/read functions at `e341625`).

## Current state

Verified at commit `e341625` (plans 006/007 may have shifted line numbers —
find the functions by name).

- `session_round_trip` (`ipc_proxy.rs:88-127`): locks `shared.conn`, calls
  `try_round_trip_locked`, and on a RECONNECTABLE error runs
  `reconnect_locked` + retry-policy handling. The reconnectable-error
  classification is the hook this plan uses: a watchdog expiry just needs to
  be classified reconnectable.
- `try_round_trip_locked` (`:128-155+`): loop { read frame; if notification →
  forward; if reverse request → handle; if chunk → accumulate; if response →
  return }. The read call inside this loop is the await to wrap.
- Reconnect on error path already exists and (post-plan-007) replays session
  state — so "abort + reconnect" is a safe recovery, EXCEPT the in-flight
  request itself: after a watchdog expiry the daemon may still execute the
  original request (it was delivered). The retry-policy machinery already
  distinguishes `SafeToRetry` vs `UnsafeToRetry` (`REQUEST_RETRY_UNSAFE`) for
  exactly this ambiguity — reuse it unchanged.
- Heartbeat task (~1s cadence, `:357-368` region) also round-trips through
  the same mutex — it is a VICTIM of the wedge today (queued behind the held
  mutex), not a detector.

## Design (already decided — key parameters)

- Wrap the **frame-read await** inside `try_round_trip_locked`'s loop with
  `tokio::time::timeout(READ_WATCHDOG, read_fut)`.
- The watchdog is **per-frame-read inactivity**, not per-request-total: each
  received frame (notification, chunk, reverse request) RESETS it, because
  arriving frames prove the daemon is alive. Long-running tool calls that
  stream progress notifications stay healthy indefinitely; only true silence
  trips it.
- `READ_WATCHDOG = 120s` as a named `const` with a comment. Rationale: the
  daemon forwards to upstreams that legitimately take tens of seconds
  (tool calls with no progress); 120s of TOTAL SILENCE (no heartbeat
  response either — heartbeats can't run while the mutex is held, so silence
  means the daemon isn't answering THIS request with anything) is decisive.
  Do not make it configurable in this plan.
- On expiry: return the same error TYPE the socket-closed path returns
  (classified reconnectable) with a distinct message
  (`"daemon read watchdog expired after {}s"`), so `session_round_trip`'s
  existing reconnect + retry-policy handling takes over. `tracing::warn!`
  at the expiry site.
- The heartbeat path uses the same locked read helper — the watchdog
  automatically covers it; no separate heartbeat timeout needed.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-mcp ipc_proxy` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug/src/ipc_proxy.rs` — `try_round_trip_locked` (timeout wrap), the new
  const, and the flipped/added tests.

**Out of scope** (do NOT touch):
- Daemon-side request timeouts (`plug/src/daemon.rs`) — separate concern.
- The retry-policy classification (`SafeToRetry`/`UnsafeToRetry`) — reuse as-is.
- Reconnect/replay logic from plan 007 — build on it, don't restructure it.
- Making the watchdog configurable (config/env) — explicitly deferred.

## Git workflow

- Branch: `fix/ipc-read-watchdog`
- Commit: `fix(ipc-proxy): reconnect on 120s read silence instead of wedging the session mutex`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the const and wrap the read

At the top of `ipc_proxy.rs` (near other consts):

```rust
/// Max silence (no frames of ANY kind) on a locked read before the daemon is
/// declared wedged and the connection is torn down for reconnect. Frames
/// (notifications, chunks, reverse requests) reset the clock, so slow tool
/// calls that emit progress are unaffected. See plans/009.
const READ_WATCHDOG: Duration = Duration::from_secs(120);
```

In `try_round_trip_locked`'s loop, wrap the frame-read await:

```rust
let frame = match tokio::time::timeout(READ_WATCHDOG, read_next_frame(...)).await {
    Ok(result) => result?, // existing error handling unchanged
    Err(_elapsed) => {
        tracing::warn!(secs = READ_WATCHDOG.as_secs(), "daemon read watchdog expired; forcing reconnect");
        return Err(/* same reconnectable error type/constructor the socket-closed path uses,
                      message: format!("daemon read watchdog expired after {}s", READ_WATCHDOG.as_secs()) */);
    }
};
```

Find the exact error constructor by reading how a socket EOF/read error is
turned into the reconnectable error in this same function — mirror it
precisely so `session_round_trip`'s classification treats expiry as
reconnectable. Because the timeout wraps each read call inside the loop, the
per-frame reset behavior falls out automatically.

**Verify**: `cargo check --workspace` → exit 0.

### Step 2: Flip the plan-006 characterization test

Rename `silent_daemon_stall_currently_hangs` →
`silent_daemon_stall_triggers_reconnect`. New shape: use the test daemon's
stall mode; call round-trip; assert it returns an ERROR (or, if retry policy
is SafeToRetry and the harness restores the daemon, a successful retried
response) rather than hanging. To keep the test fast, DO NOT wait real 120s:
use `tokio::time::pause()` + `advance` if the harness's socket I/O tolerates
it. If real socket reads prevent paused time (likely — real UnixStream reads
don't complete under paused time), instead make `READ_WATCHDOG` overridable
for tests via a `#[cfg(test)]` constructor/field on the proxy (a test-only
knob, NOT config) and set it to ~200ms in the test.

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass, including the
flipped test, in reasonable time (no 120s waits in the suite).

### Step 3: Add the frame-reset test

`watchdog_resets_on_interleaved_frames` — test daemon sends a notification
frame every ~50ms (below the test watchdog) but delays the response beyond
2× the test watchdog; assert the round-trip still SUCCEEDS (frames reset the
clock; total elapsed exceeds the watchdog but no single silent gap does).

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass; `cargo test --workspace` → all pass.

## Test plan

Covered by steps 2–3 (one flipped characterization test, one reset-behavior
test), on the plan-006 harness.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `silent_daemon_stall_triggers_reconnect` passes without real multi-second waits
- [ ] `watchdog_resets_on_interleaved_frames` passes
- [ ] Only `plug/src/ipc_proxy.rs` modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 006 is not merged (no stall characterization test / no stall mode in
  the test daemon) — the baseline is missing.
- The read future is NOT cancel-safe (dropping it mid-read could tear a
  partially-read frame and corrupt the stream) — check what `read_next_frame`
  does on partial reads; since expiry immediately abandons the connection for
  reconnect, partial-read state is normally moot — but if the same reader is
  REUSED after a timeout anywhere, report instead of proceeding.
- Watchdog expiry cannot be routed through the existing reconnectable-error
  classification without touching `session_round_trip`'s match arms — a small
  addition there is acceptable, but if it cascades into the retry-policy
  types, report first.
- The flipped test can't be made fast (no paused time AND no test-only
  override injectable) — report; do not merge a suite with a 120s test.

## Maintenance notes

- If a legitimate use case ever exceeds 120s of TOTAL silence (no progress
  frames), the fix is for the daemon/upstream to emit progress notifications
  — not to raise the const. Note this in review if anyone proposes raising it.
- The `#[cfg(test)]` override (if step 2 needed it) must never grow into a
  runtime config knob without an operator decision.
- Interacts with plan 007: expiry → reconnect → state replay. The replay
  round-trips also go through the (now watchdog-guarded) locked reads — a
  wedged daemon during replay times out too, which is correct; be aware the
  reconnect can therefore fail wholesale and surface to the caller.
