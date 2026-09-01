# Plan 007: Replay client session state after IPC daemon reconnect

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug/src/ipc_proxy.rs plug/src/runtime.rs plug/src/daemon.rs`
> If any file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MEDIUM (touches the reconnect path every local client depends on)
- **Depends on**: **Plan 006 (ipc_proxy characterization tests) MUST be merged first** — it pins current behavior and contains a `CHARACTERIZATION:` test this plan flips.
- **Category**: correctness
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

When the daemon restarts (binary upgrade, crash, operator `plug start`), each
`plug connect` proxy transparently reconnects — but the fresh daemon session
is created with ONLY `Register` + `Capabilities`. Three pieces of
client-session state the client negotiated on the OLD session are silently
lost:

1. **Client capabilities** (sent via `UpdateCapabilities` exactly once during
   initialize) — after reconnect the daemon thinks the client supports
   nothing: no sampling, no elicitation, no roots. Reverse-request features
   silently stop working for that client until it fully restarts.
2. **Resource subscriptions** — the daemon-side session no longer knows the
   client subscribed to any URIs; `resources/updated` notifications stop.
3. **Log level** (`logging/setLevel`) — resets to default.

This is the same failure class as todos/064 (task ownership lost across
daemon reconnect, found live and fixed) — the reconnect path re-establishes
identity but not negotiated state. The client has no way to know it happened:
the proxy hides the reconnect by design.

## Current state

All excerpts verified at commit `e341625`.

`plug/src/ipc_proxy.rs:375-410` — the reconnect re-establishment. It sends
only Register + Capabilities (inside `establish_daemon_proxy_session`) and
returns; nothing is replayed:

```rust
async fn refresh_session(&self) -> Result<(), McpError> {
    let mut conn = self.shared.conn.lock().await;
    self.refresh_session_locked(&mut conn).await
}

async fn refresh_session_locked(
    &self,
    conn: &mut MutexGuard<'_, SharedConnection>,
) -> Result<(), McpError> {
    let (session, stream) = crate::runtime::establish_daemon_proxy_session(
        &self.config_path,
        &self.client_id,
        self.client_info.clone(),
    )
    .await
    .map_err(|e| McpError::internal_error(format!("daemon reconnect failed: {e}"), None))?;
    ...
}
```

`plug/src/runtime.rs:541-586` — `establish_daemon_proxy_session` sends
`IpcRequest::Register { ... }` then reads `IpcResponse::Capabilities`; no
other state is transferred.

`plug/src/ipc_proxy.rs:560-572` — where client capabilities are sent, ONCE,
during downstream initialize (this is the state that must be captured and
replayed):

```rust
// inside the initialize handling
let update = IpcRequest::UpdateCapabilities {
    session_id: ...,
    capabilities: client_capabilities.clone(),
};
// ... session_round_trip(update) ...
```

Subscription requests (`resources/subscribe` / `resources/unsubscribe`) and
`logging/setLevel` flow through the generic MCP-forwarding path
(`session_round_trip` of an `IpcRequest::Mcp{...}` frame) — grep for how the
proxy forwards them: `grep -n 'subscribe\|setLevel\|set_level' plug/src/ipc_proxy.rs`.
The daemon-side handling is in `plug/src/daemon.rs` (`dispatch_mcp_request`
starts at `:2172`).

`SharedConnection` is the struct guarded by `shared.conn` (find its
definition near the top of `ipc_proxy.rs`; it holds the session + stream).

## Design decision (already made — do not re-litigate)

Fix on the **proxy side** (ipc_proxy.rs), not the daemon side. Rationale: the
daemon cannot restore state it lost in a restart (it has no persistence for
per-session negotiated state, and adding daemon persistence is a much bigger
change); the proxy is the surviving process that already knows the state.

Track three items on the proxy and replay them after every successful
re-establishment, in this order:

1. Last-sent client capabilities (`Option<ClientCapabilities>` or whatever
   type `UpdateCapabilities` carries).
2. Active resource subscriptions (`HashSet<String>` of URIs — add on
   successful `resources/subscribe`, remove on successful
   `resources/unsubscribe`).
3. Last-set log level (`Option<...>` from `logging/setLevel`).

Replay failures must NOT fail the reconnect: log a warning per failed item
and continue. A degraded session beats no session (and beats today's silently
degraded session, because it's now logged).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-mcp ipc_proxy` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug/src/ipc_proxy.rs` — state tracking + replay in `refresh_session_locked`; updating the plan-006 characterization test.

**Out of scope** (do NOT touch):
- `plug/src/daemon.rs` — no daemon-side persistence.
- `plug/src/runtime.rs::establish_daemon_proxy_session` — keep its contract
  (Register+Capabilities); replay happens AFTER it returns, in
  `refresh_session_locked`.
- Task-ownership reattachment (todos/064 territory) — already handled; don't
  restructure it.
- Upstream (plug-core) subscription logic — plan 010's territory.

## Git workflow

- Branch: `fix/ipc-reconnect-state-replay`
- Commit: `fix(ipc-proxy): replay capabilities, subscriptions, and log level after daemon reconnect`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the state-tracking fields

Add to the proxy's shared state (alongside `shared.conn` — check whether
state lives on the proxy struct or inside `SharedConnection`; put it where
the initialize path and the MCP-forwarding path can both reach it, e.g.
`Mutex<ReplayState>` or fields inside the existing `SharedConnection` since
all writers already hold that lock):

```rust
struct ReplayState {
    client_capabilities: Option<ClientCapabilities>, // use the actual type from UpdateCapabilities
    subscriptions: HashSet<String>,
    log_level: Option<LoggingLevel>, // use the actual rmcp type seen in setLevel handling
}
```

Prefer storing inside `SharedConnection`: every mutation site below already
holds `shared.conn`, so no new lock ordering is introduced. If a mutation
site does NOT hold the lock, note it and use a separate `Mutex<ReplayState>`
acquired only after any `conn` guard is dropped (never while holding it, to
avoid ordering hazards — except in `refresh_session_locked` where you already
hold `conn`; in that case a separate mutex must be acquired strictly after
`conn`, and everywhere else too — document the ordering in a comment).

**Verify**: `cargo check --workspace` → exit 0.

### Step 2: Record state at the three mutation sites

1. Where `UpdateCapabilities` is sent (`:560-572` region): after a successful
   round-trip, store the capabilities.
2. Where `resources/subscribe` / `resources/unsubscribe` results return
   success: insert/remove the URI. Only mutate on SUCCESS — a failed
   subscribe must not be replayed.
3. Where `logging/setLevel` succeeds: store the level.

If subscribe/setLevel are not specially recognized by the proxy (pure
pass-through of MCP frames), add minimal method-name matching on the request
before forwarding + state mutation after a successful response. Keep it to
exact method-name string matches (`resources/subscribe`,
`resources/unsubscribe`, `logging/setLevel`) with URI/level extracted from
the params — do not build a general interception framework.

**Verify**: `cargo check --workspace` → exit 0.

### Step 3: Replay in `refresh_session_locked`

After the new session/stream is installed in the guard (end of
`refresh_session_locked`, `:381-410` region), replay in order:

```rust
// Best-effort replay of negotiated session state lost in the daemon restart.
if let Some(caps) = replay.client_capabilities.clone() {
    if let Err(e) = /* send UpdateCapabilities on the NEW session via the locked conn */ {
        tracing::warn!(error = %e, "reconnect: failed to replay client capabilities");
    }
}
for uri in replay.subscriptions.iter() {
    if let Err(e) = /* forward resources/subscribe {uri} on the NEW session */ {
        tracing::warn!(%uri, error = %e, "reconnect: failed to replay subscription");
    }
}
if let Some(level) = replay.log_level.clone() {
    if let Err(e) = /* forward logging/setLevel on the NEW session */ {
        tracing::warn!(error = %e, "reconnect: failed to replay log level");
    }
}
```

CRITICAL: you are already holding the `conn` lock. The replay round-trips
must use the LOCKED round-trip helper (`try_round_trip_locked` or equivalent
that takes the guard), NOT `session_round_trip` (which would deadlock trying
to re-acquire `shared.conn`). Also: a replay failure must not trigger
recursive reconnect — call the raw locked write/read path and treat errors as
warn-and-continue.

**Verify**: `cargo check --workspace` → exit 0; `cargo test -p plug-mcp ipc_proxy` →
the plan-006 test `reconnect_reregisters_with_register_and_capabilities_only`
now FAILS (expected — proceed to step 4).

### Step 4: Flip the characterization test and add replay tests

1. Update plan-006's `reconnect_reregisters_with_register_and_capabilities_only`:
   rename to `reconnect_replays_client_capabilities` and assert the
   daemon-side session's capabilities MATCH the originally-negotiated ones
   after reconnect. Remove the `CHARACTERIZATION:` comment.
2. Add `reconnect_replays_subscriptions`: subscribe to a URI, restart the
   test daemon, reconnect, assert the daemon-side registry shows the
   subscription (or that a `resources/updated` for that URI reaches the
   client, whichever the harness can observe).
3. Add `reconnect_replay_failure_does_not_fail_session`: make the test daemon
   reject the replayed subscribe (error response); assert the reconnect still
   succeeds and a subsequent tools/list round-trip works.
4. Add `unsubscribe_removes_from_replay_set`: subscribe, unsubscribe, restart
   daemon, assert the URI is NOT re-subscribed.

**Verify**: `cargo test -p plug-mcp ipc_proxy` → all pass; then `cargo test --workspace` → all pass.

## Test plan

Covered by step 4 (one flipped characterization test + three new tests),
using the harness mapped in plan 006 step 1. Pattern exemplar: the
daemon-restart test at `ipc_proxy.rs:1498`.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] The flipped test asserts capabilities survive reconnect; three new replay tests pass
- [ ] No files outside `plug/src/ipc_proxy.rs` modified (`git status`)
- [ ] Replay failures produce `tracing::warn!` and do not error the reconnect (asserted by the failure-tolerance test)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 006 is not merged (no characterization tests exist) — do not proceed without the baseline.
- The proxy has NO visibility into subscribe/unsubscribe success (e.g. responses are opaque bytes never parsed) — report what parsing hook would be needed instead of building a parser inline.
- Replaying inside the held `conn` lock is impossible with the existing locked helpers (signature mismatch) — report the refactor needed rather than dropping/re-taking the lock (which would let another request interleave mid-replay).
- You find the daemon ALREADY persists any of these three items somewhere (search first: `grep -n 'capabilities' plug/src/daemon.rs | head -40`) — the design assumption is wrong; report.

## Maintenance notes

- Any FUTURE negotiated-state message added to the IPC protocol must be added
  to `ReplayState` — leave a comment on the struct saying exactly that, and
  reference this plan file.
- Watch in review: lock ordering (no new mutex acquired while holding `conn`
  unless ordering is documented), and that replay uses locked round-trip
  helpers (deadlock risk is the #1 review item).
- Plan 009 (read watchdog) also touches the locked read loop — if executed
  concurrently, coordinate; sequential execution (006 → 007 → 009) is the
  recommended order.
