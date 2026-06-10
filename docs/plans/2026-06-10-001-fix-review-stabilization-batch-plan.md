---
title: "fix: Stabilization batch from full-repo code review"
type: fix
date: 2026-06-10
status: ready
origin: code review run 20260610-005116-4bd6c4e6
plan_depth: standard
---

# fix: Stabilization Batch From Full-Repo Code Review

## Summary

Implement the confirmed, code-level defects surfaced by the full-repository review of `plug` (run `20260610-005116-4bd6c4e6`). Every item below was independently re-verified against current `main` code by a per-finding validator. This is a stabilization batch — bug fixes, performance corrections, dead-code removal, and documentation-truth repair. No new features, no protocol changes, no wire-format changes.

The work splits into five tracks: **(1)** correctness/reliability bugs in the daemon IPC and proxy paths, **(2)** artifact I/O moved off the async worker, **(3)** dead-code removal, **(4)** an agent-native correctness fix plus the `doctor` exit code, and **(5)** a documentation-truth sweep mandated by the repo's own `docs/TRUTH-RULES.md`.

Five higher-stakes findings that require owner decisions (OAuth resource-owner auth, reload-resurrection locking, reload silently-ignored config fields, the stringly-typed IPC error-code wire change, and IPC cancellation forwarding) are **explicitly out of scope** and listed under Scope Boundaries for a follow-up.

---

## Problem Frame

The codebase is healthy (review verdict: B+, no P0s), but the review confirmed a set of real defects:

- The **default local-client path** (`plug connect` → daemon IPC) silently drops the progress token on `tools/call`, so progress notifications never reach Claude Code et al. — contradicting the project snapshot's claim of progress routing "for stdio, HTTP, and daemon IPC."
- Reading a **TTL-expired artifact** that is still in the in-memory map **deadlocks** the daemon connection (DashMap same-shard read-guard held across `remove()`).
- `refresh_tools` lists upstream resources/prompts with **no timeout**, so one stalled-but-connected upstream freezes catalog refreshes for all clients; and the **notification-refresh in-progress flag** is never cleared on hang or panic, permanently silencing all `list_changed` delivery thereafter.
- `plug server edit --output json` **silently no-ops** — prints the unedited config and returns success without writing.
- Artifact writes (≥16MB) and per-chunk reads (full-file-per-128KB-chunk) run **synchronously on Tokio workers**.
- Two dead functions linger behind suppressed warnings.
- `plug doctor` computes an exit code but the process always **exits 0**, giving agents false passes.
- Four docs (and the canonical state snapshot) cite **`rmcp 1.1.0` while the lockfile pins `1.7.0`**, plus other stale stack/doc-map claims — a direct violation of the repo's truth rules.

---

## Requirements

- **R1** — Daemon IPC `tools/call` forwards the progress token end-to-end; a `progressToken` on an IPC call yields a `ProgressNotification` to the client. (review #3)
- **R2** — Reading an expired artifact URI returns an error without deadlocking. (review #2)
- **R3** — `refresh_tools` resource/prompt listing is bounded by each server's `call_timeout_secs`; a stalled upstream is skipped with a warning, not allowed to block the refresh. (review #5)
- **R4** — The notification-refresh in-progress flag is always cleared, even on hang or panic, and the refresh is itself time-bounded. (review #6)
- **R5** — `plug server edit --output json` performs the edit and emits a structured result. (review #8)
- **R6** — Artifact write and chunk-read I/O do not block Tokio worker threads. (review #7, #14, #15)
- **R7** — Dead functions `sighup_reload` and `resource_subscription_count` are removed. (review #12, #13)
- **R8** — `plug doctor` exits with its computed exit code on both JSON and text paths. (agent-native finding)
- **R9** — All version/stack/doc-map claims in the standards and state docs match `Cargo.lock` and the filesystem. (review project-standards set; TRUTH-RULES)
- **R10** — All CI gates pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace -- --test-threads=1`. Each behavior change carries a test.

---

## Key Technical Decisions

- **KTD-1: Fix the progress token at both IPC sites, mirroring the existing `enqueue_task` pattern.** `enqueue_task` (`plug/src/ipc_proxy.rs:695`) already does `serde_json::to_value(&request)` which preserves `_meta`/`progressToken`; `call_tool` (`:732`) hand-builds JSON with only `name`/`arguments`. The proven fix is to make `call_tool` serialize the full request identically, and to change the daemon non-task branch (`plug/src/daemon.rs:2227`) from a hardcoded `None` to `call_params.progress_token()` — matching what the stdio path at `plug-core/src/proxy/mod.rs:4322` already does. No new types; we reuse existing serialization and accessor patterns.
- **KTD-2: Clear the in-progress flag with an RAII Drop guard, not inline stores.** The flag is currently cleared only on the inline success path inside the spawned loop. A small guard struct whose `Drop` resets `notification_refresh_in_progress` guarantees the flag clears on early return, hang-cancellation, or panic. Pair it with `tokio::time::timeout` around `refresh_tools().await` so the task always makes forward progress. This is the same defensive pattern the existing `ActiveCallGuard` uses elsewhere in `proxy/mod.rs`.
- **KTD-3: Bound listing calls with the server's own `call_timeout_secs`, mirroring `health_check_server`.** `health_check_server` (`plug-core/src/server/mod.rs`) already wraps its probe in a timeout; the listing calls (`get_resources`/`get_prompts`) do not. Reuse the same `tokio::time::timeout` shape and the per-server `call_timeout_secs` config value already in scope. On timeout, log a warning and treat that server's contribution as empty for this refresh rather than failing the whole refresh.
- **KTD-4: Move artifact I/O into `spawn_blocking`; make prune a background task.** Wrap the write pipeline (`create_dir_all` + payload/attachment/metadata writes) and the single-chunk read in `tokio::task::spawn_blocking`. For chunk reads, replace whole-file `std::fs::read` with `seek` + `read_exact` of exactly one `ARTIFACT_CHUNK_BYTES` window. Remove the inline `self.prune()` from the spill path and schedule prune as a periodic background task at `ToolRouter`/engine construction (the engine already spawns a periodic artifact-maintenance task — fold it in there rather than per-spill).
- **KTD-5: `server edit --json` performs the edit then serializes the result.** Delete the early `return Ok(())` at `plug/src/commands/servers.rs:574-577`; run the existing non-interactive edit path (already gated on `non_interactive`), then in JSON mode emit `{"updated": true, "server": <config-after-edit>}`. If `--name` is absent in JSON mode, emit a structured error rather than no-oping.
- **KTD-6: `doctor` sets the process exit code.** After `cmd_doctor` computes `report.exit_code`, propagate it — `std::process::exit(report.exit_code)` after output is flushed, or return a typed error the `main` wrapper maps to that code. Both JSON and text branches must agree with the process exit status.
- **KTD-7: Doc-truth edits are mechanical and verified against source-of-truth files.** Each version/name claim is corrected to match `Cargo.toml`/`Cargo.lock`; the two non-existent plan-file references are removed from the CLAUDE.md doc map; the clippy command is aligned to `.github/workflows/ci.yml`. No prose invention.

---

## Implementation Units

Units are dependency-ordered and independently landable. U1–U6 are behavior changes with tests; U7–U9 are cleanup/doc with lighter verification.

### U1. Forward progress token on daemon IPC `tools/call`

**Goal:** Restore progress notifications on the default local-client path.
**Requirements:** R1, R10
**Dependencies:** none
**Files:**
- `plug/src/ipc_proxy.rs` (modify `call_tool` ~`:730` to serialize the full `CallToolRequestParams` via `serde_json::to_value(&request)`, matching `enqueue_task` at `:695`)
- `plug/src/daemon.rs` (modify the non-task `tools/call` branch ~`:2223` to pass `call_params.progress_token()` instead of `None`)
- `plug-core/src/proxy/mod.rs:4322` (reference only — the correct stdio behavior; do not change)
- test: extend the daemon-backed proxy integration tests in `plug/src/ipc_proxy.rs` (inline `#[cfg(test)]`) or `plug-core/tests/integration_tests.rs`
**Approach:** Both sites must change together — fixing only one leaves the token dropped. Verify the `_meta.progressToken` survives serialization in `call_tool`, and that the daemon dispatch extracts it the same way the task branch and stdio path do.
**Patterns to follow:** `enqueue_task` serialization (`ipc_proxy.rs:695`); stdio progress extraction (`proxy/mod.rs:4322`).
**Test scenarios:**
- Happy path: an IPC `tools/call` carrying `_meta.progressToken` against the mock server's progress-emitting tool produces a `ProgressNotification` delivered to the proxy client, asserting the token correlates.
- Edge: a `tools/call` with no `progressToken` still succeeds and emits no progress (no panic on `None`).
- Integration: assert parity — the same call over stdio and over daemon IPC both deliver progress (guards against re-divergence).
**Verification:** New test passes; manual `plug connect` against a progress-emitting upstream shows progress where it previously showed none.

### U2. Fix DashMap deadlock on expired-artifact read

**Goal:** Reading an expired-but-in-map artifact returns an error instead of deadlocking.
**Requirements:** R2, R10
**Dependencies:** none
**Files:**
- `plug-core/src/artifacts.rs` (the expired-record branch ~`:155`: drop the `records.get()` `Ref` guard before calling `records.remove()` on the same key)
- test: inline `#[cfg(test)]` in `plug-core/src/artifacts.rs`
**Approach:** Capture `expires_at` (and anything else needed) into a local, then `drop()` the `Ref` (or structure the code so the guard's scope ends) before `remove()`. The guard's destructor — not NLL borrow-end — is what holds the shard read-lock, so the `drop` must be explicit and precede `remove`.
**Patterns to follow:** existing clone-then-drop patterns elsewhere in `artifacts.rs`.
**Test scenarios:**
- Happy path / regression: insert a record with `expires_at` in the past, call the read path for its URI, assert it returns the "artifact expired" error and completes (the test would hang before the fix). Use a bounded `tokio::time::timeout` around the call so a regression fails loudly instead of hanging CI.
- Edge: a non-expired record still reads normally after the refactor.
**Verification:** New test passes within its timeout; no behavior change for live artifacts.

### U3. Bound resource/prompt listing in `refresh_tools` with per-server timeout

**Goal:** A stalled upstream cannot freeze catalog refreshes for all clients.
**Requirements:** R3, R10
**Dependencies:** none
**Files:**
- `plug-core/src/proxy/mod.rs` (`refresh_tools` ~`:1203` — wrap each per-server `get_resources`/`get_prompts` call in a timeout)
- `plug-core/src/server/mod.rs` (`get_resources` ~`:1072`, `get_prompts` ~`:1140` — confirm where `call_timeout_secs` is reachable; apply the timeout at whichever layer keeps per-server config in scope)
- test: inline `#[cfg(test)]` in `plug-core/src/proxy/mod.rs` or an integration test using the mock server's hang mode
**Approach:** Mirror `health_check_server`'s `tokio::time::timeout` guard, keyed to the server's `call_timeout_secs`. On elapse, log a warning naming the server and treat its resource/prompt contribution as empty for this refresh; do not abort the whole refresh or poison the catalog.
**Patterns to follow:** `health_check_server` timeout guard (`server/mod.rs`); the mock server's `--fail-mode timeout` (`plug-test-harness/src/bin/mock-server.rs`) — currently unused, ideal here.
**Test scenarios:**
- Failure path: a mock upstream that accepts the connection but never responds to `list_all_resources`; trigger a refresh and assert it completes within `call_timeout_secs` + margin, with the stalled server's tools/resources absent and a warning logged.
- Happy path: a responsive upstream's resources/prompts still appear in the refreshed catalog.
**Verification:** Refresh returns bounded-time under the hang; healthy servers unaffected.

### U4. Always-clear notification-refresh flag + bounded refresh

**Goal:** A refresh hang or panic never permanently disables `list_changed` delivery.
**Requirements:** R4, R10
**Dependencies:** U3 (the timeout in U3 reduces, but does not replace, the need for the guard)
**Files:**
- `plug-core/src/proxy/mod.rs` (`schedule_*_refresh` spawned task ~`:702`)
- test: inline `#[cfg(test)]` in `plug-core/src/proxy/mod.rs`
**Approach:** Introduce a small RAII guard struct holding a handle to `notification_refresh_in_progress`; its `Drop` stores `false`. Construct it at the top of the spawned task so the flag clears on every exit (normal, early-return, cancellation, panic). Wrap `refresh_tools().await` in `tokio::time::timeout` so the task cannot wedge even if a future U3 gap reappears. Preserve the existing pending-flag re-loop semantics.
**Execution note:** Add the failing test first — stub/inject a hanging refresh, assert a later schedule still delivers.
**Patterns to follow:** `ActiveCallGuard` Drop pattern in `proxy/mod.rs`.
**Test scenarios:**
- Failure path: force `refresh_tools` to exceed the timeout once; assert `notification_refresh_in_progress` is observed cleared afterward and a subsequent schedule runs and publishes a notification.
- Edge: normal back-to-back schedules still coalesce correctly (pending flag re-loops without dropping a refresh).
**Verification:** Flag-stuck regression test passes; coalescing behavior unchanged.

### U5. `server edit --output json` performs the mutation

**Goal:** Agents can edit servers via JSON and get a structured, truthful result.
**Requirements:** R5, R10
**Dependencies:** none
**Files:**
- `plug/src/commands/servers.rs` (`cmd_server_edit` ~`:573` — remove the early JSON return; emit a structured result after `save_config`)
- test: inline `#[cfg(test)]` in `plug/src/commands/servers.rs`
**Approach:** Delete the `if matches!(output, OutputFormat::Json) { …; return Ok(()) }` block at `:574-577`. Let the existing non-interactive edit path run, then in JSON mode print `{"updated": true, "server": <config-after-edit>}`. When `--name` is missing in JSON mode (non-interactive), emit a structured error object and a non-zero exit rather than silently doing nothing.
**Patterns to follow:** the JSON-result shapes already emitted by read commands in `plug/src/views/`.
**Test scenarios:**
- Happy path: `cmd_server_edit` in JSON mode with a changed field writes the change (assert the persisted config reflects it) and the emitted JSON reports `updated: true` with the new value.
- Error path: JSON mode with an unknown `--name` returns a structured error, not `Ok(())`.
- Regression: the pre-fix behavior (config unchanged after a JSON edit) is explicitly asserted against.
**Verification:** New tests pass; an agent `plug server edit --name X --command Y --output json` actually persists.

### U6. Artifact I/O off the async worker

**Goal:** Large artifact writes and chunk reads stop blocking Tokio worker threads.
**Requirements:** R6, R10
**Dependencies:** none
**Files:**
- `plug-core/src/artifacts.rs` (`maybe_spill_tool_result*` write pipeline ~`:110`; `read_chunk_text` ~`:491`; remove inline `prune()` from the spill path ~`:134`)
- `plug-core/src/proxy/mod.rs` (call site ~`:2936`; confirm the async signature accommodates `spawn_blocking` awaits)
- `plug-core/src/engine.rs` (periodic maintenance task — host the relocated prune)
- test: inline `#[cfg(test)]` in `plug-core/src/artifacts.rs`
**Approach:** (a) Wrap `create_dir_all` + payload/attachment/metadata writes in `tokio::task::spawn_blocking`, returning the assembled `ArtifactRecord` to insert into the `DashMap` after the blocking task resolves. (b) Replace `std::fs::read(whole-file)` in `read_chunk_text` with `File::open` + `seek(index * ARTIFACT_CHUNK_BYTES)` + `read_exact` of one chunk, inside `spawn_blocking`; preserve the existing overflow/out-of-range guards. (c) Delete the inline `self.prune()` from `maybe_spill_*` and ensure the existing periodic artifact-maintenance task in `engine.rs` performs prune (it already runs hourly per the snapshot — verify and fold in if not).
**Patterns to follow:** the engine's existing periodic artifact-maintenance spawn; existing chunk-size constants (`ARTIFACT_CHUNK_BYTES`).
**Test scenarios:**
- Happy path: a multi-chunk artifact reads each chunk correctly (byte-equality vs. the source across all chunk indices), proving the seek/read_exact path returns the same bytes the whole-file read did.
- Edge: out-of-range and overflow chunk indices still return the existing errors.
- Edge: spill of an attachment-style result still produces a readable artifact and the record lands in the map after the blocking write.
**Verification:** Tests pass; chunk reads are byte-identical to pre-fix; no `std::fs` write/read remains on the inline async spill/read path (grep `spawn_blocking` shows the new wrappers).

### U7. Remove dead code

**Goal:** Delete two unreachable functions and their suppressed warnings.
**Requirements:** R7, R10
**Dependencies:** none
**Files:**
- `plug/src/daemon.rs` (delete both `sighup_reload` variants ~`:2502` and ~`:2532`)
- `plug-core/src/reload.rs` (update the module comment that references SIGHUP)
- `plug-core/src/proxy/mod.rs` (delete `resource_subscription_count` ~`:780` and its `#[allow(dead_code)]`)
**Approach:** Confirm zero callers (already verified: `sighup_reload` has no caller; `resource_subscription_count` has a single definition and no call sites). Delete; adjust the `reload.rs` comment so it no longer claims a SIGHUP path that doesn't exist.
**Test scenarios:** Test expectation: none — pure dead-code removal. Coverage is the compiler: the build must stay green with no new `dead_code` allows.
**Verification:** `cargo build` and `cargo clippy -D warnings` pass; grep confirms both symbols are gone.

### U8. `doctor` exits with its computed code

**Goal:** `plug doctor` exit status reflects check outcomes for scripting/agents.
**Requirements:** R8, R10
**Dependencies:** none
**Files:**
- `plug/src/commands/misc.rs` (`cmd_doctor` ~`:162`)
- `plug/src/main.rs` (only if a typed-error mapping is chosen over `std::process::exit`)
- test: inline `#[cfg(test)]` where the report's `exit_code` computation can be asserted
**Approach:** After both output branches emit, exit with `report.exit_code` (Fail→1, Warn→2, else 0). Prefer flushing output then `std::process::exit(report.exit_code)`; if that complicates testing, return a typed error from `cmd_doctor` that `main` maps to the code. Ensure JSON and text paths agree with the process status.
**Test scenarios:**
- Unit: given a synthesized report with a Fail check, the computed `exit_code` is 1; with only a Warn, it is 2; with all pass, 0. (Assert the code-selection logic; process-exit itself is validated by an e2e/manual check.)
**Verification:** `plug doctor` returns non-zero on a failing check (manual: break a config, run `plug doctor; echo $?`).

### U9. Documentation-truth sweep

**Goal:** Bring standards/state docs back into compliance with `docs/TRUTH-RULES.md`.
**Requirements:** R9
**Dependencies:** none
**Files:**
- `CLAUDE.md` (`:140` rmcp 1.1.0 → 1.7.0; `:156` clippy command → `cargo clippy --workspace --all-targets -- -D warnings`; `:74-75` remove the two non-existent `docs/plans/2026-03-06-strategic-assessment.md` and `…-v0-1-stabilization-execution-plan.md` references)
- `docs/CRATE-STACK.md` (`:10` rmcp 1.1.x → 1.7.0; `:17` `serde_yml` → `serde_norway`)
- `docs/PROJECT-STATE-SNAPSHOT.md` (`:31` rmcp 1.1.0 → 1.7.0; baseline header "after PR #56" → "after PR #57", add upstream MCP icon passthrough + client-visible icon assets to "What Is True On main")
- `docs/PLAN.md` (`:28` rmcp 1.1.0 → 1.7.0)
**Approach:** Mechanical, each correction verified against `Cargo.toml`/`Cargo.lock` (rmcp 1.7.0, serde_norway) and the filesystem (which plan files exist; PR #57 is HEAD). Replace the two dead doc-map entries with valid current plan references or remove them.
**Test scenarios:** Test expectation: none — documentation. Verification is grep-based.
**Verification:** `grep -rn "1\.1\.0\|1\.1\.x\|serde_yml" CLAUDE.md docs/` returns nothing in the corrected files; the two referenced plan files either exist or are no longer referenced; CLAUDE.md clippy line matches `.github/workflows/ci.yml`.

---

## Scope Boundaries

### In scope
R1–R10 as enumerated above (review findings #2, #3, #5, #6, #7, #8, #12, #13, #14, #15, the doctor exit code, and the documentation-truth set).

### Deferred to Follow-Up Work (owner decision required — do NOT implement here)
- **#1 OAuth downstream resource-owner auth** (`plug-core/src/downstream_oauth/mod.rs:138`) — a threat-model decision about public-client OAuth on non-loopback binds. Needs the owner to choose the auth model.
- **#4 Config-reload server resurrection** (`plug-core/src/engine.rs:494`) — fixing the reconnect/reload race involves a locking-strategy choice with its own deadlock risk; needs deliberate design.
- **#10 Reload silently ignores config fields** (`plug-core/src/reload.rs:205`) — deciding restart-vs-refresh-vs-warn semantics per field is a behavior decision.
- **#11 Stringly-typed IPC error codes** (`plug-core/src/ipc.rs:448`) — improving this touches the daemon↔proxy wire contract; needs a compatibility decision (mixed-version operation).
- **#19 IPC cancellation forwarding** (`plug/src/ipc_proxy.rs:726`) — requires an IPC protocol addition (out-of-band frame); a protocol change, not a fix.

### Out of scope entirely
Any new feature, transport, or protocol surface. No refactor of the `ToolRouter` god-file or the cross-transport dispatch duplication (architectural work tracked separately in the audit).

---

## Risks & Mitigations

- **U6 changes async ordering** (record now lands in the map after a `spawn_blocking` boundary). Risk: a read racing a just-spilled artifact. Mitigation: insert into the `DashMap` synchronously after the blocking task resolves but before returning the result to the caller, so the artifact URI is never advertised before the record exists. Cover with the "spill then immediately read" test scenario in U6.
- **U3/U4 interact** — U3 bounds the listing calls; U4 bounds the whole refresh and guarantees flag-clear. Land U3 before U4 so U4's test exercises the realistic (bounded) refresh, but both are required: U4's Drop guard also protects against panics U3 doesn't address.
- **U8 `std::process::exit` bypasses destructors** — ensure any buffered stdout is flushed before exiting; the text path uses `println!` (line-buffered to a tty, fully buffered to a pipe) so flush explicitly, or use the typed-error-to-`main` route to avoid the concern.
- **Test serialization** — the suite runs `--test-threads=1` (global daemon path mutex). New daemon/IPC tests must use isolated temp paths and not assume parallelism; follow the existing `set_test_runtime_paths` convention.

---

## Verification (whole batch)

- `cargo fmt --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean (U7 removes the only `dead_code` allows touched here).
- `cargo test --workspace -- --test-threads=1` green, including the new tests in U1–U6 and U8.
- Manual smoke: `plug connect` against a progress-emitting upstream shows progress (U1); `plug doctor; echo $?` is non-zero on a broken config (U8); `plug server edit --name X --command Y --output json` persists (U5).
- Doc grep checks from U9 pass.

## Post-Merge Truth Pass (required by CLAUDE.md)

After merge: confirm merged code is on `main`; re-verify `docs/PROJECT-STATE-SNAPSHOT.md` matches `main` (it will have changed in U9); confirm `docs/PLAN.md` still matches; revalidate the snapshot's progress-routing and artifact claims against the U1/U6 changes.
