# Plan 019: Clean up a departing HTTP session's tasks on both teardown paths

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- plug-core/src/http/server.rs plug-core/src/proxy/tasks.rs plug/src/runtime.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW-MED (adds cleanup calls on the session-teardown paths; the
  cleanup primitive already exists and is exercised by IPC)
- **Depends on**: none (plan 003 also edits `plug/src/runtime.rs`-adjacent
  daemon code and plan 008 edits `session/stateful.rs` — different
  functions; coordinate merges, don't run simultaneously on the same file)
- **Category**: bug
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

plug implements the MCP tasks feature: HTTP and IPC clients can make
task-augmented `tools/call` requests, and the records live in a single
process-wide `TaskStore` keyed by owner (`http:{session_id}` /
`ipc:{client_id}`). When an IPC client departs, the daemon calls
`cleanup_tasks_for_owner` on both its teardown paths. When an HTTP session
ends — via `DELETE /mcp` or idle expiry — plug cleans up EIGHT other kinds
of per-session state but never the session's tasks. `plug serve` runs one
long-lived engine, so dead sessions' task records accumulate in the shared
store. The damage is bounded but real: the store's TTL pruning (terminal
tasks after 1h, in-flight after 24h, 100-completed cap per owner) is LAZY —
it runs only when some task operation happens later — and is keyed on
wall-clock, not session liveness. IPC reclaims immediately; HTTP leaks
until an unrelated task op happens to trigger a prune. This is the single
asymmetric map: every other per-session structure is cleaned on both HTTP
paths and both IPC paths (enumeration in "Current state"). The fix mirrors
the existing IPC behavior — two call sites plus a shared owner-key helper —
and makes the expiry path testable.

## Current state

All excerpts verified at the planned-at commit.

**The store and its primitives:**

- `plug-core/src/proxy/mod.rs:182` — the process-wide store on the shared
  router: `task_store: Mutex<TaskStore>,`
- `plug-core/src/tasks.rs:240-242` — the per-owner teardown primitive:

  ```rust
  pub fn cleanup_owner(&mut self, owner: &TaskOwner) {
      self.tasks.retain(|_, record| &record.owner != owner);
  }
  ```

- `plug-core/src/proxy/tasks.rs:257-259` — the router-level wrapper:

  ```rust
  pub async fn cleanup_tasks_for_owner(&self, owner: &TaskOwner) {
      self.task_store.lock().await.cleanup_owner(owner);
  }
  ```

- `plug-core/src/proxy/tasks.rs:4-6` — the IPC owner-key helper (there is
  no HTTP equivalent; HTTP builds its key inline, see below):

  ```rust
  pub fn task_owner_for_ipc_client(client_id: &str) -> TaskOwner {
      TaskOwner::new(Arc::<str>::from(format!("ipc:{client_id}")))
  }
  ```

**The insert side (how HTTP task records are created):**

- `plug-core/src/dispatch/mod.rs:85-97` — a task-augmented `tools/call`
  with `supports_tasks()` calls `ctx.task_owner()` and
  `enqueue_tool_task(...)`. Stdio overrides `supports_tasks()` to `false`
  (`plug-core/src/proxy/handler.rs:21`); HTTP and IPC use the default
  `true` (dispatch/mod.rs:58-60).
- `plug-core/src/http/server.rs:86-90` — the HTTP owner key, built inline:

  ```rust
  fn task_owner(&self) -> Result<crate::tasks::TaskOwner, McpError> {
      Ok(crate::tasks::TaskOwner::new(Arc::<str>::from(
          format!("http:{}", self.session_id).as_str(),
      )))
  }
  ```

- The four HTTP task handlers (`tasks/get|list|cancel|result`, around
  `http/server.rs:1336`, `:1360`, `:1384`, `:1408`) also build
  `format!("http:{session_id}")` inline — find them all with
  `grep -n '"http:{' plug-core/src/http/server.rs`.

**The two HTTP teardown paths (the gap):**

- `plug-core/src/http/server.rs:932-965` — `delete_mcp` (DELETE /mcp):
  after `state.sessions.remove(&session_id)` succeeds it cleans
  subscriptions (`:943`), `roots_capable_sessions` (`:944`),
  `client_capabilities` (`:945`), `pending_client_requests` (`:947-949`),
  the downstream bridge (`:950`), roots (`:951-953`), the client log level
  (`:956`), and the lazy session (`:957-961`) — and never touches tasks.
- `plug/src/runtime.rs:257-283` — the idle-expiry consumer
  (`while let Some(session_id) = expiry_rx.recv().await` inside a
  `tokio::spawn`): the same eight cleanups, also without task cleanup. The
  body is inline in the spawn — there is currently no named function to
  test.

**The IPC contrast (the behavior to mirror):**

- `plug/src/daemon.rs:907-915` (disconnect path; the explicit
  `EndSession` path ~`:1695-1702` is the same shape):

  ```rust
  if let Some(client_id) = removed_client_id {
      if !ctx.client_registry.client_sessions.contains_key(&client_id) {
          let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
          ctx.engine
              .tool_router()
              .cleanup_tasks_for_owner(&owner)
              .await;
      }
  }
  ```

  (IPC gates on the client's LAST session departing because IPC owners are
  per-client; HTTP owners are per-session, so no gate is needed.)

**Why stdio is not affected**: stdio rejects task-augmented calls
(`supports_tasks() = false`), and the standalone stdio server runs a fresh
engine per client that dies with the process.

**Asymmetry enumeration** (context, verified): `resource_subscriptions`,
`client_roots`, `downstream_bridges`, `lazy_working_sets`,
`client_log_levels` (router-side) and `roots_capable_sessions`,
`client_capabilities`, `pending_client_requests` (HttpState-side) are all
cleaned on delete_mcp AND expiry AND the IPC paths. `task_store` is cleaned
only on the IPC paths.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build check | `cargo check` | exit 0 |
| Tests | `cargo test --workspace` | all pass |
| Targeted | `cargo test -p plug-core task_` and `cargo test -p plug-mcp expired_http_session` | new tests pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (see done-criteria caveat) |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `plug-core/src/proxy/tasks.rs` — add `task_owner_for_http_session`
  helper next to the IPC one.
- `plug-core/src/http/server.rs` — `delete_mcp` gains the task cleanup;
  the inline `format!("http:{…}")` sites switch to the new helper.
- `plug/src/runtime.rs` — extract the expiry-consumer body into a named
  function and add the task cleanup to it.
- Test additions in those files' existing test modules (or
  `plug-core/src/proxy/tests.rs` if http/server.rs has no test module —
  check first).

**Out of scope** (do NOT touch, even though they look related):
- `plug/src/daemon.rs` — IPC teardown already correct; leave both sites
  alone.
- `TaskStore` internals (`plug-core/src/tasks.rs`) — TTL prune and
  retention behavior stay exactly as they are; this plan adds CALLERS of
  the existing cleanup, not new store semantics.
- Consolidating the delete_mcp/expiry teardown lists into one shared
  function across the two crates — a worthwhile follow-up (see Maintenance
  notes), but a cross-crate refactor of the session teardown path is more
  risk than this fix warrants.
- `plug-core/src/session/stateful.rs` (how sessions expire) — unchanged.

## Git workflow

- Branch: `advisor/019-http-session-task-teardown` off `main`.
- Conventional commits (repo style); suggested:
  `fix(http): clean up a departing session's tasks on DELETE and idle expiry`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Inventory the inline owner-key sites and existing task tests

Run `grep -n '"http:{' plug-core/src/http/server.rs` (expect ~5 hits: the
`task_owner` impl + four task handlers) and
`grep -rn 'enqueue_tool_task\|cleanup_tasks_for_owner' plug-core plug --include='*.rs'`
to find every producer/consumer and the existing task-lifecycle tests to
model on. Record both lists in your notes.

**Verify**: lists recorded; the only `cleanup_tasks_for_owner` callers are
the two daemon.rs sites.

### Step 2: Add the HTTP owner-key helper

In `plug-core/src/proxy/tasks.rs`, next to `task_owner_for_ipc_client`
(:4-6), add:

```rust
pub fn task_owner_for_http_session(session_id: &str) -> TaskOwner {
    TaskOwner::new(Arc::<str>::from(format!("http:{session_id}")))
}
```

Switch every inline `format!("http:{…}")` owner construction found in step
1 to this helper (the `task_owner()` impl at http/server.rs:86-90 and the
four task handlers). The key format now lives in exactly one place per
transport.

**Verify**: `cargo check` → exit 0;
`grep -c '"http:{' plug-core/src/http/server.rs` → 0.

### Step 3: Clean tasks in `delete_mcp`

Inside the `if state.sessions.remove(&session_id)` block of `delete_mcp`
(http/server.rs:938-962), alongside the existing cleanups, add:

```rust
let owner = crate::proxy::ToolRouter::task_owner_for_http_session(&session_id);
state.router.cleanup_tasks_for_owner(&owner).await;
```

**Verify**: `cargo check` → exit 0.

### Step 4: Extract and fix the expiry consumer

In `plug/src/runtime.rs`, extract the body of the expiry loop (:258-282)
into a named function in the same file, e.g.
`async fn cleanup_expired_http_session(http_state: &Arc<HttpState>, tool_router: &Arc<ToolRouter>, session_id: &str)`
(match the actual types in scope — read the surrounding code), moving the
existing eight cleanups verbatim, and add the task cleanup:

```rust
let owner = plug_core::proxy::ToolRouter::task_owner_for_http_session(session_id);
tool_router.cleanup_tasks_for_owner(&owner).await;
```

The spawn becomes
`while let Some(session_id) = expiry_rx.recv().await { cleanup_expired_http_session(&http_state_for_expiry, &tool_router, &session_id).await; }`.
Extraction is what makes this path testable; keep it same-file, no
signature exports beyond `pub(crate)` if the test module needs it.

**Verify**: `cargo check` → exit 0; `cargo test --workspace` → all existing
tests pass.

### Step 5: Add an observability accessor for tests

In `plug-core/src/proxy/tasks.rs`, add a `pub` count method (tasks are
owner-scoped, so no external client can observe another owner's records —
tests need a direct probe; `pub` because the plug crate's runtime test in
step 6 also uses it):

```rust
pub async fn task_count_for_owner(&self, owner: &TaskOwner) -> usize
```

implemented via the store's existing per-owner listing/retain machinery
(read `TaskStore`'s API in plug-core/src/tasks.rs and use the least
invasive query — do not add TaskStore methods if an existing one serves).

**Verify**: `cargo check` → exit 0.

## Test plan

Model on the existing task-lifecycle tests found in step 1 (they show how
to build a router with a mock upstream and enqueue a task-augmented call).

1. `delete_mcp_cleans_up_session_tasks` (plug-core, in http/server.rs's
   test module if one exists, else the established in-crate location for
   HTTP tests): create a session, enqueue a task for owner
   `http:{session_id}` (via the same path existing task tests use), assert
   `task_count_for_owner == 1`, invoke the DELETE path, assert
   `task_count_for_owner == 0`.
2. `expired_http_session_cleans_up_tasks` (plug crate, runtime.rs test
   module — daemon/runtime tests already exist there ~:1452+): seed a task
   record for a session owner, call `cleanup_expired_http_session`
   directly, assert count drops to 0 and (spot-check) one of the other
   cleanups also ran (e.g. log level removed) — guarding the extraction's
   behavior preservation.
3. Negative check (proves the tests detect the bug): temporarily comment
   out the two new `cleanup_tasks_for_owner` calls, run the two tests —
   both must FAIL; restore. Do NOT use `git stash` (shared worktree).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -c 'cleanup_tasks_for_owner' plug-core/src/http/server.rs` → ≥1
      and `grep -c 'cleanup_tasks_for_owner' plug/src/runtime.rs` → ≥1
- [ ] `grep -c '"http:{' plug-core/src/http/server.rs` → 0 (helper used
      everywhere)
- [ ] `cargo test --workspace` exits 0, including the two new tests
- [ ] Negative check demonstrated (recorded in completion notes)
- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
      **Known pre-existing failure caveat**: at the planned-at commit this
      gate is RED for two findings unrelated to this plan (`question_mark`
      at `plug-core/src/artifacts.rs:482`, `for_kv_map` at
      `plug-core/src/server/mod.rs:774` — plan 001 step 0 fixes them). If
      clippy fails with EXACTLY those two, record it and treat this
      criterion as met.
- [ ] `git status` shows only the three in-scope source files (+ their test
      modules) modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The delete_mcp or expiry-consumer code no longer matches the excerpts
  (drift — someone may have already fixed or restructured teardown).
- Enqueueing a task in a test requires substantially more machinery than
  the existing task-lifecycle tests use (report what's missing rather than
  building a parallel harness).
- The step-4 extraction cannot preserve the existing cleanup order without
  visible behavior change (e.g. a test starts failing on cleanup ordering)
  — report; do not reorder cleanups to make a test pass.
- You find a THIRD HTTP teardown path (anything else consuming session
  removal) during step 1 — report it; this plan covers exactly two.

## Maintenance notes

- Root cause worth fixing later: the eight-item teardown list is duplicated
  between `delete_mcp` (plug-core) and the expiry consumer (plug), which is
  exactly how tasks got added to one transport's teardown and not the
  other's. A follow-up could consolidate both onto one shared
  `HttpState`-level teardown function; that refactor was deliberately kept
  out of this fix.
- If a new per-session structure is ever added, it must be added to BOTH
  HTTP teardown sites and BOTH IPC teardown sites (daemon.rs:907-915,
  ~:1695-1702) — reviewers should ask "where is this cleaned up?" for every
  new session-keyed map.
- Reviewer should scrutinize: the expiry extraction is move-only apart from
  the added task cleanup; the IPC paths are untouched.
