# Plan 003: Fix four small verified correctness bugs (underflow, busy-spin, cancel window, idle reverse-request starvation)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/session/stateful.rs plug/src/daemon.rs plug-core/src/proxy/tasks.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. Another AI agent (Codex) may be
> working in this repo concurrently — check carefully.

## Status

- **Priority**: P1
- **Effort**: S (×4 independent fixes)
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

Four independently verified bugs from the 2026-07-11 audit, each small and
low-risk, bundled because they share files and a single review:

1. **Underflow in the session-cleanup task** — a concurrent insert during
   `retain` makes `before - sessions.len()` underflow; with default
   `overflow-checks` in dev/test this panics the spawned cleanup task, which
   silently dies (its JoinHandle is never awaited) and idle-session
   reclamation stops forever.
2. **CPU busy-spin after `Deregister`** — the dispatch loop's channel-restore
   resurrects a closed reverse-request channel; the biased `select!` then
   polls `Ready(None)` in a hot loop for the duration of the next request.
3. **Task cancellation window** — cancelling a task-wrapped tool call before
   the upstream request id is recorded sends no upstream `notify_cancelled`;
   the upstream runs the call to completion for a discarded result. The
   foreground call path already guards this exact window
   (`pending_cancel_reason`); the task path doesn't.
4. **Idle connections never service reverse requests** — the daemon's
   registered-idle `select!` has no arm for `reverse_request_rx`, so an
   elicitation/sampling request from a background task hangs until the client
   happens to send another request.

## Current state

### Bug 1 — `plug-core/src/session/stateful.rs:480-482`

Inside `spawn_cleanup_task`'s interval tick (the task starts at `:452`;
`expired_ids` are pre-collected at `:471-475` only when `expiry_tx` is set):

```rust
let before = sessions.len();
sessions.retain(|_, state| state.last_activity.elapsed() <= timeout);
let expired = before - sessions.len();
```

`sessions` is a shared `Arc<DashMap<...>>`; `create_session` inserts
concurrently. `expired` feeds only logging below.

### Bug 2 — `plug/src/daemon.rs`

The dispatch loop takes the channel (`:1140`):

```rust
let mut reverse_rx = ctx.reverse_request_rx.take();
```

selects over it while the dispatch future runs (`:1160-1171`):

```rust
reverse = async {
    if let Some(ref mut rx) = reverse_rx {
        rx.recv().await
    } else {
        std::future::pending().await
    }
} => {
    if let Some((reverse_req, resp_tx)) = reverse {
        handle_reverse_request(reader, writer, reverse_req, resp_tx).await?;
    }
}
```

and restores it afterward (`:1197-1201`):

```rust
// Restore reverse_request_rx (dispatch_request may have replaced it
// during a Register call, in which case ctx already has the new one).
if ctx.reverse_request_rx.is_none() {
    ctx.reverse_request_rx = reverse_rx;
}
```

The `Deregister` handler (`:1704-1705`) sets `ctx.session_id = None;
ctx.reverse_request_rx = None;` after the bridge was unregistered (so the
channel's sender is dropped → closed). The restore then puts the CLOSED
channel back; on the next request the `reverse` arm returns `Ready(None)`
every poll, its body does nothing (`if let Some` falls through), and the
biased loop spins hot until `dispatch_fut` completes.

### Bug 3 — `plug-core/src/proxy/tasks.rs`

Dispatch records the upstream id only AFTER the request is in flight
(`:423-441`):

```rust
let request_handle = match peer.send_cancellable_request(request, options).await { ... };
self.task_store.lock().await.set_upstream_request(
    &task_id,
    TaskUpstreamRef::Request { server_id: server_id.clone(), request_id: request_handle.id.clone() },
);
```

The cancel path (`:261-321`) reads `(task, upstream, handle)` from
`mark_cancelled`; when `upstream` is `None` it skips all upstream
notification and just `handle.abort()`s. The pattern to mirror is the
foreground guard in `plug-core/src/proxy/mod.rs:868-895`
(`attach_upstream_request_id`): it takes `pending_cancel_reason` from the
active-call record and, if a cancel arrived early, spawns
`peer.notify_cancelled(...)` as soon as the id lands.

### Bug 4 — `plug/src/daemon.rs:1043-1061`

The registered-connection idle loop:

```rust
tokio::select! {
    biased;
    _ = ctx.cancel.cancelled() => return Ok(()),
    recv = rx.recv() => { send_ipc_logging_notification(writer, recv).await?; }
    recv = async { if let Some(ref mut crx) = ctrl_rx { crx.recv().await } else { std::future::pending().await } } => {
        send_ipc_control_notification(writer, recv, ctx.session_id.as_deref()).await?;
    }
    result = reader.next() => break 'select result,
}
```

No `reverse_request_rx` arm — contrast the dispatch loop at `:1160-1171`
above, which services it via `handle_reverse_request(reader, writer, ...)`.

Conventions: tests live in `#[cfg(test)] mod tests` at the bottom of each
file (daemon's starts at `plug/src/daemon.rs:2637`); tracing for logs;
clippy must stay clean with `-D warnings`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check | `cargo check --workspace` | exit 0 |
| Full tests | `cargo test --workspace` | all pass |
| Daemon tests | `cargo test -p plug-mcp daemon` | all pass |
| Core tests | `cargo test -p plug-core` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/session/stateful.rs` (bug 1 + test)
- `plug/src/daemon.rs` (bugs 2 & 4 + tests)
- `plug-core/src/proxy/tasks.rs` (bug 3 + test; `set_upstream_request` and the task-store struct it lives on)

**Out of scope** (do NOT touch):
- `plug/src/ipc_proxy.rs` — reconnect behavior is plans 006/007/009.
- `plug-core/src/proxy/mod.rs` — the foreground cancel guard is the *pattern*, not a target.
- The SSE replay logic in `stateful.rs` (`send_replay_events`, `clear_sender_if_matching`) — that's plan 008; only the cleanup-task arithmetic is in scope here.
- Any IPC wire-format change.

## Git workflow

- Branch: `fix/small-correctness-batch`
- One conventional commit per bug, e.g. `fix(session): saturate expired-count arithmetic in cleanup task`, `fix(daemon): do not restore a closed reverse-request channel after Deregister`, `fix(tasks): replay pending cancellation once upstream request id lands`, `fix(daemon): service reverse requests while a registered connection is idle`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Fix the underflow (bug 1)

In `plug-core/src/session/stateful.rs:482`, change:

```rust
let expired = before - sessions.len();
```

to:

```rust
let expired = before.saturating_sub(sessions.len());
```

**Verify**: `cargo test -p plug-core session` → all pass.

### Step 2: Fix the closed-channel busy-spin (bug 2)

Two mutually reinforcing edits in `plug/src/daemon.rs`:

1. **Defensive (sufficient on its own)** — in the `reverse` arm of the
   dispatch loop (`:1167-1171`), handle the `None` case by disabling the
   channel for the rest of the loop:

   ```rust
   if let Some((reverse_req, resp_tx)) = reverse {
       handle_reverse_request(reader, writer, reverse_req, resp_tx).await?;
   } else {
       // Channel closed (e.g. bridge deregistered) — stop polling it.
       reverse_rx = None;
   }
   ```

2. **Root cause** — make the restore honor an intentional clear: capture
   whether the request was a `Deregister` before dispatch (the request enum is
   matched in `dispatch_request`; at the restore site you still have `&request`)
   and skip the restore in that case:

   ```rust
   let request_was_deregister = matches!(request, IpcRequest::Deregister { .. });
   ...
   if ctx.reverse_request_rx.is_none() && !request_was_deregister {
       ctx.reverse_request_rx = reverse_rx;
   }
   ```

   Check the actual `IpcRequest::Deregister` variant shape in
   `plug-core/src/ipc.rs` for the correct pattern (it may have fields —
   use `{ .. }`).

**Verify**: `cargo test -p plug-mcp daemon` → all pass, plus the new test from the Test plan.

### Step 3: Replay pending task cancellation (bug 3)

Mirror the foreground `pending_cancel_reason` pattern inside the task store:

1. In the task-store record (same file, the struct `set_upstream_request` and
   `mark_cancelled` operate on), add a `pending_cancel: bool` (or
   `Option<String>` reason) field, default false.
2. In `mark_cancelled`, when the task has no upstream ref yet, set
   `pending_cancel = true` before returning `(task, None, handle)` — and do
   NOT abort-only silently: the caller keeps its current behavior otherwise.
3. Change `set_upstream_request` to return whether a cancel is pending
   (`-> bool` or return the ref it stored plus the flag).
4. At the call site (`tasks.rs:435-441`), if it returns pending-cancel, send
   `notify_cancelled` to the upstream immediately (same
   `CancelledNotificationParam { request_id, reason }` shape as
   `cancel_task_for_owner` at `:305-313`), then proceed to
   `await_response()` (the upstream will terminate the call; the existing
   response-discard path handles the result).

**Verify**: `cargo test -p plug-core tasks` → all pass, plus the new test from the Test plan.

### Step 4: Add the idle reverse-request arm (bug 4)

In the registered-idle `select!` (`plug/src/daemon.rs:1043-1061`), add an arm
(BEFORE `reader.next()`, matching the biased ordering used in the dispatch
loop) that services `ctx.reverse_request_rx`:

```rust
reverse = async {
    if let Some(ref mut rx) = ctx.reverse_request_rx {
        rx.recv().await
    } else {
        std::future::pending().await
    }
} => {
    if let Some((reverse_req, resp_tx)) = reverse {
        handle_reverse_request(reader, writer, reverse_req, resp_tx).await?;
    } else {
        ctx.reverse_request_rx = None;
    }
}
```

Borrow caution: this select borrows `ctx.reverse_request_rx` mutably while
other arms use `ctx.cancel` and `ctx.session_id` — if the borrow checker
objects, destructure the needed fields before the loop the way the dispatch
loop takes the channel out of `ctx`. Preserve the existing arms unchanged.

**Verify**: `cargo test -p plug-mcp daemon` → all pass, plus the new test from the Test plan.

## Test plan

New tests, one per bug (model after the existing daemon test module at
`plug/src/daemon.rs:2637+` and the proxy task tests in
`plug-core/src/proxy/tests.rs`):

1. **stateful.rs** — unit test: build the session store, insert a session,
   directly exercise the retain-and-count block's logic with a concurrent
   insert (or, simpler and acceptable: a regression test asserting
   `saturating_sub` semantics via the cleanup path with `tokio::time::pause`
   + `advance` past the timeout while inserting a fresh session; assert the
   cleanup task is still alive by advancing again and observing a second
   expiry).
2. **daemon.rs (bug 2)** — after a `Deregister` on a live connection, send
   another request on the same connection and assert it completes with a
   normal response (pre-fix this still passes functionally — so ALSO assert
   the restore behavior directly if the loop structure allows; at minimum the
   test documents the sequence and guards step-2's `matches!` logic via a
   unit test on the restore condition if it's extracted into a helper).
3. **tasks.rs (bug 3)** — unit test on the store: create a task, call
   `mark_cancelled` BEFORE `set_upstream_request`, then call
   `set_upstream_request` and assert it reports a pending cancel.
4. **daemon.rs (bug 4)** — if an IPC e2e harness helper exists in the daemon
   test module for registered connections (look for tests that Register then
   exchange frames, e.g. the `control_notification` tests), add: register,
   go idle, inject a reverse request via the bridge, assert the client side
   receives it without sending a request first. If no such harness exists in
   the unit module, add the test to the daemon IPC e2e tests instead (search
   `plug/tests/` and `plug-core/tests/integration_tests.rs` for `Register`).

Verification: `cargo test --workspace` → all pass, including 3–4 new tests.

## Done criteria

- [ ] `cargo test --workspace` exits 0, with new tests present for bugs 1, 3, and 4 (bug 2 covered per Test plan note)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `grep -n 'before - sessions.len()' plug-core/src/session/stateful.rs` → no matches
- [ ] The idle select in `daemon.rs` has a reverse-request arm (`grep -A2 'reverse = async' plug/src/daemon.rs` shows two sites: idle + dispatch)
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The `select!` borrow structure in step 4 cannot accommodate the new arm without restructuring `ConnectionContext` itself — report the conflict rather than refactoring the context type.
- `set_upstream_request` or `mark_cancelled` signatures are used by callers other than the two sites named here (`grep -rn 'set_upstream_request\|mark_cancelled' plug-core/src plug/src`) and a signature change would ripple — report the caller list.
- Any existing daemon/proxy test fails after a fix — the fix broke an intended behavior; report the failing test name and output.
- The excerpts above don't match the live code (drift; possibly the concurrent Codex run touched these files).

## Maintenance notes

- Bug 2's defensive `else { reverse_rx = None }` also protects against any FUTURE path that closes the bridge channel mid-request; keep it even if the restore logic is later refactored.
- Bug 3's pending-cancel flag must be considered if task retry/re-dispatch is ever added (a retried dispatch must re-check it).
- Reviewer focus: the biased-select arm ordering in step 4 (reverse before reader) and that step 2 doesn't drop a LIVE replacement channel installed by a Register that raced the restore.
