# Plan 024: End-to-end tests for the config-file watcher (disk change → debounce → reload applied)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md` — unless a reviewer dispatched you and
> told you they maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat e341625..HEAD -- plug-core/src/watcher.rs plug-core/Cargo.toml`
> If watcher.rs changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW for the codebase (production code is NOT modified), MED for
  the tests themselves (real filesystem events + real time; flake risk
  managed by design rules below)
- **Depends on**: none (test-only). Follows plan 014's de-flake philosophy
  from day one: poll-with-timeout, never fixed sleeps as success criteria.
- **Category**: tests
- **Planned at**: commit `e341625`, 2026-07-12

## Why this matters

The config watcher is the daemon's only AUTOMATIC reconfiguration path
(`plug reload` via IPC is the manual one). The chain — file modified on
disk → notify/debouncer event → filename filter → `load_config` →
`Engine::reload_config` → diff applied — has ZERO test coverage:
`watcher.rs` has no test module and is never constructed in any test;
reload tests cover the pure `diff_configs` and direct in-memory
`reload_config` calls only; even the daemon-level runtime tests bypass
`cmd_daemon`, the one place the watcher is spawned. A regression here —
a filename-filter typo, a `notify`/`notify-debouncer-mini` behavior change
on a version bump (currently 8.2.0 / 0.7.0), a broken error branch — would
ship silently and surface as "I edited config.toml and nothing happened."
The watcher also has three deliberate keep-current-config error branches
(parse failure, reload failure, missing directory) that nothing exercises.
This plan adds an in-file test module covering the happy path, the
error-recovery path, the sibling-file filter, and the editor
atomic-rename-save pattern — with **no production code changes**.

## Current state

All excerpts verified at the planned-at commit.

- `plug-core/src/watcher.rs` (132 lines, no `#[cfg(test)]` module):
  - `:18` — `const DEBOUNCE_MS: u64 = 500;` (private, NOT injectable — a
    deliberate constraint on the tests, see hazards).
  - `:24-35` — the only public entry:

    ```rust
    pub fn spawn_config_watcher(
        engine: Arc<Engine>,
        config_path: PathBuf,
        cancel: CancellationToken,
        tracker: &tokio_util::task::TaskTracker,
    ) {
        tracker.spawn(async move {
            if let Err(e) = run_watcher(engine, config_path, cancel).await {
    ```

  - `:42-55` — watches the config file's PARENT DIRECTORY (NonRecursive);
    if the directory doesn't exist it logs, awaits `cancel`, and never
    arms.
  - `:57` — `let (tx, mut rx) = mpsc::channel(16);` — debouncer callback
    does `tx.try_send(event.path)` (`:66`, drops on full).
  - `:96-102` — filename filter: events whose `file_name()` differs from
    the config filename are skipped.
  - `:105-124` — on match: `config::load_config(Some(&config_path))`, then
    `engine.reload_config(new_config)`; parse failure (`:121-123`) and
    reload failure (`:116-118`) both log and KEEP the current config, and
    the loop continues (the watcher must survive errors).
- Spawn wiring (context): exactly one production call site —
  `plug/src/runtime.rs:757-762` inside `cmd_daemon`, passing
  `engine.cancel_token().clone()` and `engine.tracker()`.
- Engine accessors a test needs (all public, `plug-core/src/engine.rs`):
  `server_statuses()` `:264`, `tool_list()` `:269`, `cancel_token()`
  `:274`, `event_sender()` `:289` (subscribe to `EngineEvent` via
  `.subscribe()` — reload emits a `ConfigReloaded` event), `config()`
  `:294`, `tracker()` `:299`.
- Exemplar test for the assertion style:
  `reload_preserves_failed_server_visibility_for_added_server`
  (`plug-core/src/engine.rs:1236`) — builds `Engine::new(config)`, calls
  `reload_config`, asserts on `server_statuses()`. The new tests are that,
  plus a real file and the real watcher in front.
- Mock upstream for observable server add/remove:
  `plug_test_harness::mock_server_bin()`
  (`plug-test-harness/src/lib.rs:29`) builds `mock-mcp-server` once and
  returns its path; `plug-core/tests/integration_tests.rs` ~`:1676-1730`
  shows the pattern of pointing a `ServerConfig.command` at it. There is
  NO temp-config helper in the harness — tests write config files with
  `std::fs::write` inline (existing precedent: `plug/src/runtime.rs:1452`).
- Existing coverage inventory (so nobody double-writes): `reload.rs`
  `:350-596` — pure `diff_configs` tests + one concurrency-pattern test;
  `engine.rs:1236`, `:1553` — direct in-memory `reload_config` tests;
  `plug-core/tests/integration_tests.rs` — zero watcher/reload references.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Build check | `cargo check` | exit 0 |
| New tests | `cargo test -p plug-core watcher` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Flake probe | run the watcher tests 5× in a loop | 5/5 pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (see done-criteria caveat) |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `plug-core/src/watcher.rs` — a new `#[cfg(test)] mod tests` ONLY. Zero
  changes above it.
- `plug-core/Cargo.toml` — dev-dependencies ONLY, and only if `tempfile`
  (or equivalent) isn't already there (check first; `plug-test-harness`
  should already be a dev-dependency since integration tests use it).

**Out of scope** (do NOT touch, even though they look related):
- Any production line of watcher.rs — including exposing `DEBOUNCE_MS`,
  adding test hooks, or changing the channel capacity. If the tests prove
  impossible without a production change, that's a STOP, not a license.
- `plug/src/runtime.rs` (`cmd_daemon` wiring) — the watcher's spawn site
  is exercised implicitly; wiring tests are not this plan.
- `reload.rs` / `engine.rs` — their direct-call tests already exist.
- The IPC `plug reload` path (daemon.rs) — separate trigger, separately
  covered.

## Git workflow

- Branch: `advisor/024-config-watcher-e2e-tests` off `main`.
- Conventional commits; suggested:
  `test(watcher): cover disk-change → debounce → reload end to end`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Confirm dev-deps and read the file

Read watcher.rs fully (132 lines). Check `plug-core/Cargo.toml`
`[dev-dependencies]` for `tempfile` and `plug-test-harness`; add `tempfile`
if absent.

**Verify**: `cargo check` → exit 0.

### Step 2: Build the shared test scaffold

In the new test module, write one helper used by every test:

- Create a `tempfile::TempDir` (EACH test gets its OWN — the watcher
  watches the whole directory, so a shared dir would cross-trigger between
  parallel tests).
- Write an initial valid config into `<dir>/config.toml` with
  `std::fs::write` (start with zero servers for the cheap tests; the
  happy-path test adds a mock server later).
- `let engine = Arc::new(Engine::new(config));` + `engine.start().await`.
- Subscribe to events: `let mut events = engine.event_sender().subscribe();`
- `spawn_config_watcher(engine.clone(), config_path, engine.cancel_token().clone(), engine.tracker());`
- **Arm-wait**: the watcher arms asynchronously; before mutating the file,
  poll until it's live. There is no readiness signal, so use a bounded
  retry loop around the FIRST mutation instead: write the change, then
  poll for the expected effect up to 5s; if silent, rewrite the file
  (touch again) up to 2 more times. Encapsulate this in the helper
  (`mutate_and_await_reload`), so individual tests stay clean.
- Poll pattern (plan 014 philosophy — this is the ONLY acceptable wait
  shape): loop `tokio::time::sleep(Duration::from_millis(100))` +
  check-condition, deadline 5s, with the condition being a received
  `ConfigReloaded` event (drain `events.try_recv()`) or an observable
  state change (`engine.server_statuses()`).
- Teardown: `engine.cancel_token().cancel()` (stops the watcher loop and
  drops the debouncer's OS thread), then engine shutdown if the exemplar
  tests do so.
- Tests MUST be `#[tokio::test(flavor = "multi_thread")]` — the debouncer
  delivers from its own OS thread and the 500ms debounce is real
  wall-clock; paused tokio time cannot work here.

**Verify**: scaffold compiles; a trivial smoke test (spawn + cancel, no
mutation) passes.

### Step 3: The four tests

1. `watcher_applies_config_change_from_disk` (happy path): start with zero
   servers; rewrite config.toml adding one server whose command is
   `mock_server_bin()` (model the ServerConfig on the integration-test
   pattern); await reload; assert `server_statuses()` now contains the
   server AND a `ConfigReloaded` event arrived.
2. `watcher_keeps_config_and_survives_parse_error`: write syntactically
   invalid TOML; wait out the debounce with a bounded poll asserting NO
   `ConfigReloaded` arrives within ~2s and `server_statuses()` is
   unchanged; then write a VALID config (with a change) and assert the
   reload applies — proving the loop survived the error (this second half
   is the load-bearing assertion; the watcher dying on bad TOML mid-edit
   is the realistic regression).
3. `watcher_ignores_sibling_file_changes`: create/modify `other.toml` in
   the same directory; assert no `ConfigReloaded` within ~2s; then modify
   config.toml and assert reload fires (positive control in the same test
   makes the negative half meaningful).
4. `watcher_survives_atomic_rename_save`: write the new config to
   `<dir>/config.toml.tmp`, then `std::fs::rename` over `config.toml`
   (the editor atomic-save pattern); assert the reload applies. This works
   because the watcher watches the DIRECTORY, not the file inode — this
   test pins that load-bearing design choice.

**Verify**: `cargo test -p plug-core watcher` → 4 (+smoke) pass.

### Step 4: Flake probe

Run the watcher tests five times consecutively (e.g.
`for i in 1 2 3 4 5; do cargo test -p plug-core watcher || break; done`).
All five runs must pass. These tests are wall-clock bound (~1-3s each —
500ms debounce + poll overhead); that's the accepted cost of covering the
real notify backend.

**Verify**: 5/5 green; then `cargo test --workspace` → all pass.

## Test plan

The four tests in step 3 ARE the deliverable, plus the scaffold smoke test.
Coverage claimed after this plan: happy path, parse-error recovery,
filename filter, rename-replace. Explicitly NOT covered (stated so the
index doesn't over-claim): the missing-directory branch (`:48-55` — trivial
and requires racing spawn against dir creation), the full-channel drop
(`:66` — needs >16 debounced events in flight; not realistically
reachable), and reload-failure-keeps-config (`:116-118` — requires
constructing a config that passes parse but fails reload; add it
opportunistically if `reload_config` has an easy failure input, otherwise
note it as accepted residual).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `git diff e341625..HEAD -- plug-core/src/watcher.rs` shows ONLY an
      appended `#[cfg(test)]` module (production lines 1-131 unchanged)
- [ ] `cargo test -p plug-core watcher` exits 0 with ≥4 new tests
- [ ] Flake probe: 5 consecutive runs pass (recorded in completion notes)
- [ ] `cargo test --workspace` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
      **Known pre-existing failure caveat**: at the planned-at commit this
      gate is RED for two findings unrelated to this plan (`question_mark`
      at `plug-core/src/artifacts.rs:482`, `for_kv_map` at
      `plug-core/src/server/mod.rs:774` — plan 001 step 0 fixes them). If
      clippy fails with EXACTLY those two, record it and treat this
      criterion as met.
- [ ] `git status` shows only `plug-core/src/watcher.rs` (and possibly
      `plug-core/Cargo.toml` dev-deps) modified
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The tests cannot be made reliable (a failure in the 5× probe, twice,
  after applying the retry-on-first-mutation scaffold) — report the
  platform (macOS FSEvents vs Linux inotify), the failing test, and the
  observed timing. The likely follow-up is a `#[cfg(test)]`-visible
  debounce constant, but that is a PRODUCTION change requiring a decision,
  not something to do unilaterally.
- Passing tests requires ANY production-code change in watcher.rs.
- `Engine::new` + `start()` with a zero-server config doesn't come up
  cleanly in a unit-test context (would indicate the engine needs runtime
  scaffolding this plan didn't anticipate) — report what it needs.
- Watcher tests interfere with the rest of the suite under parallel
  execution (the suite runs parallel in CI per PR #62) — report rather
  than serializing the whole suite.

## Maintenance notes

- If `notify` or `notify-debouncer-mini` is ever bumped, these tests are
  the regression net — run them on the bump PR and pay attention to the
  rename-replace test (backend event semantics differ most there).
- If the debounce interval ever becomes configurable, drop the wall-clock
  waits to the configured minimum and delete the retry-on-first-mutation
  scaffold.
- Reviewer should scrutinize: per-test TempDir isolation (no shared
  watched directory), and that no assertion depends on a bare
  `sleep`-then-check without a polling deadline.
- Residual gaps accepted (listed in Test plan): missing-directory branch,
  full-channel drop, reload-failure branch.
