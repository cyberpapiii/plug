---
title: "feat: parallel test suite — remove --test-threads=1"
type: feat
date: 2026-06-10
status: ready
plan_depth: standard
origin: docs/plans/2026-06-10-002-feat-operability-hardening-program-plan.md
program_item: 4
requirements: [R1, R2]
---

# feat: Parallel Test Suite — Remove `--test-threads=1`

## Summary

Deferred **item 4** (U1/U2) of the operability/hardening program. The workspace suite runs single-threaded (`cargo test --workspace -- --test-threads=1`) because the daemon tests share one process-global runtime-paths slot (`test_runtime_paths()` in `plug/src/daemon.rs`). This serializes ~680 tests behind the ~15 that touch that global; the integration suite (43 tests, ~67s) and the `plug` bin suite (150 tests, ~53s) dominate wall-clock and would parallelize cleanly.

This PR makes the suite parallel-safe and drops the flag. It was investigated and reverted during PR #60 (dropping the flag surfaced races across `daemon.rs`, `ipc_proxy.rs`, and `runtime.rs`); this is its own focused home with an empirical safety protocol.

**Scope decision:** the program plan's KTD-1 prefers full `RuntimePaths` injection (delete the global, thread explicit paths through ~31 call sites including client-side socket discovery in `auth.rs`/`doctor.rs`). That is the larger, higher-risk refactor. Per the bounded "test-infra only" scope, this PR instead (a) unifies the three separate test locks into one shared lock so every test that reads or writes the global paths is mutually excluded, and (b) drops the flag **only after** an empirical repeated-parallel-run gate proves no residual race. Full path injection is recorded as a deferred enhancement. The two listing approaches reach the same observable outcome (parallel CI, green suite); the lock-unification carries far less blast radius.

---

## Problem Frame

`runtime_dir()` / `log_dir()` in `plug/src/daemon.rs` consult a `#[cfg(test)]` process-global `Mutex<Option<(PathBuf, PathBuf)>>` (`test_runtime_paths()`). Tests call `set_test_runtime_paths` / `clear_test_runtime_paths` around their body so the daemon they spawn and the client they drive resolve to a per-test temp dir. But three test groups coordinate access to that one global via **different** mechanisms:

- `plug/src/ipc_proxy.rs` — 10 daemon-backed proxy tests hold `daemon_test_lock()`.
- `plug/src/runtime.rs` — 3 tests hold `runtime_path_test_lock()`.
- `plug/src/daemon.rs` — 2 tests (`run_daemon_losing_start_…`, `run_daemon_restores_existing_token_…`) call `set`/`clear` with **no lock at all**.

Because these are distinct locks (and one group has none), under `--test-threads=N` a runtime test and a daemon-proxy test can both `set_test_runtime_paths` concurrently and clobber each other, and any test reading `socket_path()` while another holds the global set sees the wrong path. `--test-threads=1` masks all of it.

Separately, `plug-core/tests/integration_tests.rs` spawns the mock via `cargo run -p plug-test-harness --bin mock-mcp-server` per server — fine serially, but under parallel execution many concurrent `cargo run` processes contend on Cargo's `target/` build lock.

---

## Requirements

- **R1** — `cargo test --workspace` passes with default (parallel) threads; CI and docs no longer pass `--test-threads=1`.
- **R2** — Integration tests exec a pre-built mock binary instead of `cargo run` at test time, so parallel runs don't contend on the Cargo build lock.
- **L1** — Every test that reads or writes the global runtime-paths is mutually excluded by one shared lock; no test group is unlocked.
- **L2** — Parallel-safety is proven empirically (full suite, parallel, repeated) before the CI flag is dropped; any surfaced race is fixed at its source, never re-serialized with the blanket flag.
- **R7** (carried) — `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` (parallel) all pass.

---

## Key Technical Decisions

- **KTD-1: One shared `runtime_paths_test_lock()` in `daemon.rs`.** Define a single `pub(crate)` `tokio::sync::Mutex<()>` (via `OnceLock`) in `daemon.rs` next to the path globals. Replace `ipc_proxy::daemon_test_lock()` and `runtime::runtime_path_test_lock()` with re-exports/uses of it, and add the guard to the two unlocked `daemon.rs` tests. Every `set_test_runtime_paths` call site then holds the same lock for the whole duration the global is set, so no concurrent test can clobber or read a foreign path.
- **KTD-2: Tie the guard to the set/clear lifetime.** Keep the existing pattern (`let _guard = runtime_paths_test_lock().lock().await;` at the top of each test, held until drop) and ensure `set_test_runtime_paths` is only ever called while the guard is held. Audit every call site. The guard outlives the daemon the test spawns, so the daemon reads the global only while its test owns the lock.
- **KTD-3: Pre-build the mock once and exec the binary path.** Move the existing `ensure_mock_server_built()` logic (`ipc_proxy.rs`) into a shared `plug-test-harness` lib helper (e.g. `mock_server_bin()` returning a built-once `PathBuf`). `integration_tests.rs::mock_server_config` uses that path directly as `command` instead of `cargo run …`. The harness already exposes `mock_server_path()` from `current_exe()`; the helper builds once via `cargo build -p plug-test-harness --bin mock-mcp-server` behind a `OnceLock`, parallel-safe.
- **KTD-4: Drop the flag last, gated on a repeated parallel run.** Only after `cargo test --workspace` (no `--test-threads=1`) passes repeatedly (≥10×) with zero flakes do we edit `.github/workflows/ci.yml` and the docs. If a race surfaces, fix it at the source (extend lock coverage / unique port / unique path) and re-run; never reinstate the blanket flag to paper over it.
- **KTD-5: Deviate from program KTD-1 (full path injection) deliberately.** Full injection removes the global entirely and lets even daemon tests run concurrently, but threads `RuntimePaths` through ~31 sites including production client-discovery paths — higher risk than the bounded scope warrants. Lock-unification serializes only the ~15 global-paths tests among themselves while the other ~620 parallelize, achieving R1. Full injection is deferred (Scope Boundaries).

---

## Implementation Units

### U1. Unify the runtime-paths test lock

**Goal:** One lock mutually excludes every test that touches the global runtime-paths.
**Requirements:** L1, R1
**Dependencies:** none
**Files:**
- `plug/src/daemon.rs` (add `pub(crate) fn runtime_paths_test_lock() -> &'static tokio::sync::Mutex<()>`; add the guard to the two currently-unlocked tests ~2772, ~2812)
- `plug/src/ipc_proxy.rs` (replace `daemon_test_lock()` def + 10 usages with the shared lock)
- `plug/src/runtime.rs` (replace `runtime_path_test_lock()` def + 3 usages with the shared lock)
**Approach:** Define the shared lock in `daemon.rs`. In `ipc_proxy.rs` and `runtime.rs`, delete the local lock fns and call `crate::daemon::runtime_paths_test_lock()`. Add `let _guard = crate::daemon::runtime_paths_test_lock().lock().await;` to the two bare `daemon.rs` tests before their `set_test_runtime_paths`. Verify (grep) that every `set_test_runtime_paths` call site is preceded by a guard acquisition in the same test.
**Patterns to follow:** the existing `daemon_test_lock()` / `runtime_path_test_lock()` definitions and guard-acquisition pattern.
**Test scenarios:**
- Existing daemon/ipc/runtime tests still pass with the unified lock (no behavior change serially).
- Edge: under parallel threads, two global-paths tests never observe each other's paths (proven by U3's repeated parallel run).
**Verification:** the two old lock fns are gone (grep); all `set_test_runtime_paths` sites hold the shared lock; suite passes serially.

### U2. Pre-built mock binary for integration tests

**Goal:** Integration tests exec the built mock binary, not `cargo run`, so parallel runs don't contend on Cargo's build lock.
**Requirements:** R2
**Dependencies:** none
**Files:**
- `plug-test-harness/src/lib.rs` (add a `pub fn mock_server_bin() -> PathBuf` that builds once via `OnceLock` + `cargo build -p plug-test-harness --bin mock-mcp-server`, then returns `mock_server_path()`)
- `plug-core/tests/integration_tests.rs` (`mock_server_config` ~1150 → `command: mock_server_bin()`, args start at `--tools`)
- `plug/src/ipc_proxy.rs` (replace the local `ensure_mock_server_built()` with the shared harness helper)
**Approach:** Lift the build-once logic into the harness lib so both `integration_tests.rs` and `ipc_proxy.rs` share it. `mock_server_config` sets `command = Some(mock_server_bin().to_string_lossy().into())` and drops the `cargo run … --bin mock-mcp-server --` prefix from `args`, keeping `--tools <tools>` and any test-appended flags (`--resources`, `--list-fail-flag-file`, …).
**Patterns to follow:** existing `ensure_mock_server_built()` (`ipc_proxy.rs`) and `mock_server_path()` (`plug-test-harness/src/lib.rs`).
**Test scenarios:**
- Integration tests spawn the mock with no `cargo` invocation (observe no compile in `--nocapture` output after a warm build).
- The degraded-vs-absent regression test and all resource/subscription integration tests still pass with the prebuilt mock.
**Verification:** no `cargo run`/`cargo build` in the per-test server command path; integration suite green.

### U3. Prove parallel-safety, then drop the flag

**Goal:** Remove `--test-threads=1` only after empirical proof.
**Requirements:** L2, R1, R7
**Dependencies:** U1, U2
**Files:**
- `.github/workflows/ci.yml` (remove `-- --test-threads=1` from the ubuntu + macos test jobs, ~lines 38, 49)
- `CONTRIBUTING.md` (~line 31), `CLAUDE.md` (~154), `docs/OPERATOR-GUIDE.md` (~200) — update the documented test command
**Approach:** Run `cargo test --workspace` (no flag) repeatedly (≥10×) locally. Any flake is fixed at its source (extend lock coverage, unique port/path) and the run repeated until clean. Only then edit CI + docs. If a residual race proves out of bounded scope, **fall back**: keep `--test-threads=1` in CI, ship U2 (mock prebuild) alone as an honest partial, and record the remaining race.
**Execution note:** This unit is gated on the repeated-run result — do not edit CI until the local parallel run is clean ≥10×.
**Test scenarios:**
- `cargo test --workspace` parallel passes ≥10× with zero flakes.
- CI green without `--test-threads=1` on both ubuntu and macos.
**Verification:** CI green parallel; docs updated; no flake across the repeated local runs.

---

## Scope Boundaries

### In scope
R1, R2, L1, L2: unify the test lock, pre-build the mock, drop the flag after empirical proof, update docs.

### Deferred to Follow-Up Work
- **Full `RuntimePaths` injection (program KTD-1)** — delete the global entirely and thread explicit paths through `run_daemon` and the client-discovery sites, so even the ~15 daemon/runtime tests run concurrently. Larger refactor; deferred.

### Out of scope entirely
Any non-test production behavior change; reworking the mock server's protocol surface.

---

## Risks & Mitigations

- **Latent races beyond the global paths (the prior whack-a-mole).** Mitigation: U3's repeated parallel-run gate is the detector; fixes go at the source. The fallback (keep the flag, ship the mock prebuild) bounds the downside to "no worse than today."
- **Lock-unification leaves a reader unlocked.** Mitigation: U1 audits every `set_test_runtime_paths` AND every `socket_path()`/`runtime_dir()` test reader for guard coverage; the repeated parallel run surfaces any miss.
- **`mock_server_bin()` build race under parallel test processes.** Mitigation: `OnceLock` serializes the build within a process; `cargo build` itself holds the target lock across processes. Built-once-then-exec is strictly less contended than `cargo run` per spawn.

---

## Verification

- `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace` (parallel) green, repeated ≥10× with no flake.
- CI green on ubuntu + macos without `--test-threads=1`.

## Post-Merge Truth Pass

Update `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md`: the suite now runs parallel in CI; record full `RuntimePaths` injection as the remaining deferred test-infra enhancement.
