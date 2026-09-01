# Plan 012: Track grace-period retirement tasks so engine shutdown retires old upstreams immediately

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/server/mod.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Plan 011 also edits `replace_server` — if it
> landed, expect its `ReplaceOutcome` shape and build on it. Another AI agent
> (Codex) may be working in this repo concurrently.

## Status

- **Priority**: P3
- **Effort**: S/M
- **Risk**: LOW/MEDIUM (shutdown path only)
- **Depends on**: soft dependency on plan 011 (same function vicinity; land
  011 first to avoid conflicts — this plan does not require 011's logic).
- **Category**: correctness / lifecycle
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

When zero-downtime replacement swaps in a new upstream (`replace_server`),
the OLD upstream is retired by a **fire-and-forget `tokio::spawn`** that
sleeps ~30s (grace for in-flight requests holding the old Arc) and then shuts
it down. Nothing tracks these tasks. Consequences:

- `shutdown_all` (engine shutdown, `server/mod.rs:1569-1582`) tears down the
  server MAP, but orphaned grace tasks keep OLD upstream connections (child
  processes for stdio servers, HTTP connections, OAuth refresh timers held
  via the upstream) alive up to 30s AFTER the engine reports shutdown
  complete. For `plug serve` under launchd this can look like a hung/dirty
  exit; in tests it leaks processes across test boundaries.
- Rapid successive replacements (flappy server + health monitor) stack
  multiple grace tasks, each pinning an old upstream generation.

**Important negative decision (do not "improve" on it)**: the grace period
itself is CORRECT — the old upstream's Arc is intentionally kept alive while
in-flight requests finish (`strong_count > 1` logic). During planning, an
auditor's sketch proposed "cancel the old upstream's token immediately before
the sleep" — that would kill in-flight requests and defeat zero-downtime
replacement. The fix is TRACKING, not shortening: normal operation is
unchanged; only engine shutdown accelerates retirement.

## Current state

Verified at commit `e341625` in `plug-core/src/server/mod.rs`.

The fire-and-forget spawn inside `replace_server` (`:1743-1768`, structural):

```rust
if let Some(old) = old {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        // wait for strong_count to drop / force-retire the old upstream
        ...old.shutdown()... // read the real retirement body
    });
}
```

`shutdown_all` (`:1569-1582`, structural): iterates the map, shuts each
upstream down, clears the map. No knowledge of in-flight grace tasks.

Facts to confirm on read:
- The exact retirement body (what "shutdown" means for an upstream here —
  method name, whether it awaits).
- What struct `replace_server` and `shutdown_all` live on (the server
  manager) and where to hang a new field.
- Whether the manager is `Clone`/`Arc`-shared (affects field type choice).
- Existing test patterns for this file
  (`grep -n '#\[tokio::test\]' plug-core/src/server/mod.rs`).

## Fix design (already decided)

Add a tracked-task registry to the server manager:

```rust
/// Grace-period retirement tasks for replaced upstreams. Aborted (and the
/// upstreams force-retired) on shutdown_all; otherwise they self-clean.
retire_tasks: Mutex<tokio::task::JoinSet<()>>,
```

- `replace_server`: spawn the SAME grace body via
  `retire_tasks.lock().await.spawn(...)` instead of `tokio::spawn`.
  `JoinSet::spawn` returns an `AbortHandle` and the set reaps completed tasks
  on subsequent calls (call `try_join_next()` in a small loop after each
  spawn to drain finished entries so the set doesn't grow unboundedly).
- Force-retire on shutdown: the grace body's shutdown call must ALSO run when
  aborted — abort alone would leak the old upstream without shutdown. So
  restructure: keep a shared handle to the old upstream in a side list, OR
  simpler and preferred: give the grace body a shutdown-signal receiver
  (`tokio::sync::watch<bool>` on the manager, set true by `shutdown_all`):

  ```rust
  let mut shutdown_rx = self.shutdown_signal.subscribe(); // watch::Receiver<bool>
  self.retire_tasks.lock().await.spawn(async move {
      tokio::select! {
          _ = tokio::time::sleep(GRACE) => {}
          _ = shutdown_rx.wait_for(|v| *v) => {} // engine shutting down: skip the grace
      }
      /* existing retirement body, unchanged */
  });
  ```

- `shutdown_all`: first set the watch to true, then `while let Some(_) =
  retire_tasks.lock().await.join_next().await {}` (await all retirements —
  each now completes promptly), THEN proceed with the existing map teardown.
  Bound the wait with a `tokio::time::timeout` of ~5s and `warn!` on expiry
  (a wedged retirement must not hang shutdown forever).

This preserves the grace semantics in normal operation exactly (the sleep
arm), and on shutdown converts every pending grace to immediate retirement.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-core server` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/server/mod.rs` — manager fields, `replace_server` spawn
  site, `shutdown_all`, tests.

**Out of scope** (do NOT touch):
- The grace DURATION and the strong-count/retirement logic inside the body —
  byte-for-byte unchanged apart from the added select.
- Plan 011's staleness check (if present) — untouched.
- `stop_server` — its direct shutdown path is already synchronous with the
  caller.
- Any engine.rs shutdown ordering.

## Git workflow

- Branch: `fix/grace-retire-tracking`
- Commit: `fix(server): track replacement grace tasks and retire them on shutdown`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read the real retirement body and manager shape

Complete the "facts to confirm". Decide where the two new fields
(`retire_tasks`, `shutdown_signal` watch sender) go on the manager struct and
how they're initialized in its constructor(s).

**Verify**: `grep -n 'fn new' plug-core/src/server/mod.rs` — you know every
constructor to update.

### Step 2: Implement per the design

Fields + constructor init; spawn-site swap with `try_join_next` drain; select
in the grace body; `shutdown_all` signal + bounded join-all before existing
teardown.

**Verify**: `cargo check --workspace` → exit 0.

### Step 3: Tests

Pattern-match existing server/mod.rs tests. Use short grace via
`tokio::time::pause()` where possible (the sleep arm is pausable; if the
retirement body awaits real I/O on mocks, keep durations tiny):

1. `replace_grace_still_waits_in_normal_operation` — replace a server; assert
   the old upstream is NOT retired before the grace elapses and IS after
   (paused-time advance).
2. `shutdown_all_retires_pending_grace_immediately` — replace a server
   (grace pending), call `shutdown_all`; assert the old upstream's shutdown
   ran WITHOUT advancing time past the grace, and `shutdown_all` returned.
3. `stacked_replacements_all_retire_on_shutdown` — replace the same server
   twice quickly (two pending graces); `shutdown_all`; assert both old
   generations retired.
4. `shutdown_all_bounded_when_retirement_wedges` — mock retirement that never
   completes; assert `shutdown_all` returns after the 5s bound (paused time)
   with a warning, not a hang. (Only if the mock seam allows a wedgeable
   retirement; otherwise skip with a note.)

**Verify**: `cargo test -p plug-core server` → all pass; `cargo test --workspace` → all pass.

## Test plan

Covered by step 3. Test 2 is the headline behavior; test 1 proves the
negative decision (grace preserved) held.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] Retirement body diff shows only the added `select!` wrapper (grace logic unchanged)
- [ ] Only `plug-core/src/server/mod.rs` modified (`git status`)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The retirement body cannot run under `JoinSet` because it captures
  non-Send state — report the capture.
- `shutdown_all` is called from a context that must not await long (e.g. a
  Drop impl or sync context) — the bounded join changes its latency profile;
  report where it's called from (`grep -rn 'shutdown_all' plug-core/src plug/src`) if any caller looks latency-sensitive.
- The manager has multiple constructors/builders in other files (the fields
  must be initialized everywhere) — if constructors exist outside
  server/mod.rs, the scope grows; report.
- Paused time doesn't work because the retirement body does real I/O even
  under mocks — fall back to millisecond-scale real durations; if tests then
  flake, report rather than padding sleeps.

## Maintenance notes

- Future code that spawns anything owning an upstream generation must go
  through `retire_tasks` (or a sibling tracked set) — fire-and-forget spawns
  in the server manager are now a review smell.
- The 5s shutdown bound is a backstop; if it ever fires in real logs, the
  retirement body has a hidden await on a dead resource — investigate there,
  don't raise the bound.
- Interacts with plan 011: its `StaleDiscarded` path shuts the NEW upstream
  inline (no grace task) — nothing to track there; confirm in review that no
  new fire-and-forget spawn appeared in that path.
