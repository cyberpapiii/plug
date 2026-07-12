# Plan 014: Convert wall-clock sleeps in tests to tokio paused time where safe

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug/src/daemon.rs plug-core/src`
> On changes to files you're editing, re-run the step-1 inventory rather than
> trusting the counts below. Another AI agent (Codex) may be working in this
> repo concurrently.

## Status

- **Priority**: P3
- **Effort**: M (mechanical but requires judgment per site)
- **Risk**: LOW/MEDIUM (a wrong conversion produces a hanging or lying test)
- **Depends on**: none (coordinate: plans 006/007/009 add ipc_proxy tests, 013 adds e2e tests — do NOT convert tests those plans just added without checking their notes; execution after them is fine, this plan targets the EXISTING suite)
- **Category**: tests / DX
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

The suite (~730 tests) leans on real `tokio::time::sleep` waits for
timing-dependent behavior — the daemon test module alone has four 1500ms
sleeps, and sleep-poll loops appear across the integration tests. Costs:
minutes of pure wall-clock waiting per full run (multiplied across every CI
job and every local `cargo test --workspace` in the verification gates all
other plans use), plus flake risk on slow CI runners where a fixed sleep
races the thing it waits for. `tokio::time::pause()`/`advance()` makes
timer-driven tests instant AND deterministic — but ONLY for tests whose
awaited events are timer-driven; converting a test that waits on real I/O
(sockets, child processes, filesystem watchers) produces a hang, because
paused time auto-advances only when the runtime is otherwise idle on timers.

## Current state

Verified at commit `e341625`:

- `plug/src/daemon.rs` test module (from `:2637`): four `sleep(Duration::from_millis(1500))`
  calls waiting for daemon-side async effects after IPC round-trips.
- Inventory command (run at execution time — counts will have drifted):

  ```sh
  grep -rn 'sleep(Duration::from_millis\|sleep(Duration::from_secs\|std::thread::sleep' \
    plug/src plug-core/src plug-core/tests plug/tests plug-test-harness/src \
    | grep -v '^.*//' | wc -l
  ```

- Existing paused-time usage to pattern-match: `grep -rn 'time::pause\|start_paused' plug-core/src plug/src plug-core/tests` — if hits exist, follow their idiom (`#[tokio::test(start_paused = true)]` is the preferred form); if none, this plan introduces it.

## Classification rule (the core judgment)

For each sleep site, determine what the test is actually waiting FOR:

| Waiting for | Convertible? | Action |
|---|---|---|
| A tokio timer in production code (retry backoff, grace period, debounce, refresh margin, watchdog) | YES | `start_paused = true`; replace `sleep(X)` with `tokio::time::advance(X).await` (or just await the future — auto-advance fires idle timers) |
| An async effect with no I/O (task spawned on same runtime updating shared state) | USUALLY | replace fixed sleep with a bounded poll on the actual condition (`tokio::time::timeout(1s, async { loop { if cond() {break} tokio::task::yield_now().await } })`), which is deterministic without pause |
| Real I/O: Unix socket, child process, HTTP server, `notify` file watcher, keychain | **NO** | leave the sleep; optionally tighten to a condition-poll with real time; do NOT pause |
| Test-harness sequencing (giving a spawned server time to bind) | NO (I/O) | prefer readiness signal if the harness exposes one; else leave |

`std::thread::sleep` inside async tests is a bug regardless (blocks the
runtime thread) — convert to `tokio::time::sleep` at minimum wherever found.

Mixed tests (timer + I/O in one test) are NOT convertible to paused time;
only the poll-tightening applies.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Baseline timing | `cargo test --workspace 2>&1 | tail -5` (note total time) | — |
| Per-crate timing | `cargo test -p plug-mcp 2>&1 | tail -3` | — |
| Full tests | `cargo test --workspace` | all pass |
| Flake check on converted modules | `for i in 1 2 3; do cargo test -p <crate> <module> || break; done` | 3× green |
| Lint / format | `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- Test code ONLY (`#[cfg(test)]` modules and `tests/` dirs) across the workspace.
- Priority order: (1) the four 1500ms daemon.rs sleeps, (2) any
  `std::thread::sleep` in async tests, (3) multi-second sleeps in integration
  tests, (4) sub-second sleeps only if trivially convertible.

**Out of scope** (do NOT touch):
- ALL production code — including production timer constants; if a test is
  unconvertible because a production constant is untestable, note it, don't
  change it.
- Tests added by plans 006–013 in this program (their plans own their timing).
- The harness's mock servers' internal timing.
- Sleeps that are load-bearing for REAL I/O (classification row 3) — leaving
  them is the correct outcome, not a failure.

## Git workflow

- Branch: `test/paused-time-deflake`
- Commits: one per module converted, `test(<module>): use paused time / condition polls instead of wall-clock sleeps`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Inventory and classify

Run the inventory command; produce a table (file:line → wait target →
classification row → action). The daemon.rs 1500ms×4 sites are your first
entries: read each surrounding test to see what effect it awaits (they follow
IPC round-trips — the effect is likely "daemon task processed the frame",
which is row 2 (condition-pollable) or row 3 (real socket) — decide per test
by reading what asserts after the sleep).

Record the baseline: full-workspace test wall time.

**Verify**: classification table complete; baseline time recorded.

### Step 2: Convert row-1 and row-2 sites, module by module

For each module: convert, then immediately
`cargo test -p <crate> <module>` + the 3× flake loop. A conversion that
hangs = misclassified I/O — revert THAT site to row 3, annotate it in the
table, move on. Never leave a converted site un-flake-checked.

**Verify** (per module): tests pass 3× consecutively, and the module's
runtime dropped (or you can articulate why not).

### Step 3: Tighten row-3 sites that have an observable condition

Fixed sleep → `timeout`-bounded condition poll with REAL time (no pause).
Only where the condition is already observable from the test (a health
status, a counter, a registry read). Where nothing is observable, leave the
sleep untouched and record it.

**Verify**: same per-module gate as step 2.

### Step 4: Full-suite verification and the numbers

**Verify**:
- `cargo test --workspace` → all pass.
- 3× full-workspace flake loop → green.
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0; `cargo fmt --check` → exit 0.
- Report before/after wall time in the completion report, plus the final
  classification table (converted / tightened / left-as-is with reason).

## Test plan

Not applicable in the usual sense — the deliverable IS test changes; the
guard is the 3× flake loops and the unchanged pass/fail semantics of every
test (no assertion may be weakened to make a conversion work — weakening is a
STOP condition).

## Done criteria

- [ ] `cargo test --workspace` exits 0, 3× consecutively
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] No production code modified (`git diff --stat` — only test modules / `tests/` files)
- [ ] No `std::thread::sleep` remains in async test bodies (`grep -rn 'std::thread::sleep' plug/src plug-core/src --include='*.rs'` hits, if any, are in sync-context tests only — justify each in the report)
- [ ] Completion report contains the classification table and before/after suite wall time
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- A conversion only passes if an ASSERTION is weakened — the sleep was hiding
  a real ordering bug; report the test and the ordering issue (that's a
  finding, potentially a new plan).
- More than ~3 sites in one module resist classification (you can't tell what
  the sleep waits for) — the module needs a harness redesign, not in-place
  edits; report it.
- The daemon.rs conversions require the runtime-paths global lock story to
  change (paused-time tests interleaving badly with the PR #62 parallel-test
  locks) — report the interaction.
- Suite wall time does NOT improve measurably after the daemon.rs + top
  integration sites are done — the effort/payoff assumption was wrong; report
  with numbers and await direction before grinding through sub-second sites.

## Maintenance notes

- New timing-dependent tests should default to `#[tokio::test(start_paused = true)]`
  when timer-driven, condition-polls when effect-driven, and fixed sleeps
  only for real-I/O readiness with a comment saying what they wait for.
- The classification table in the completion report is reusable — keep it
  (paste into the PR description) so reviewers of future test changes know
  which sleeps are load-bearing.
