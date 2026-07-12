# Improve-program execution report (2026-07-11/12)

**Branch**: `improve/integration` (local only, never pushed). Fork point:
local `main` @ `a2d8fd6` (a plans-only docs commit); implementation
baseline: `e341625` (the commit all plans were written against, equal to
`origin/main`'s runtime code). This report was written at the end of the
execution run and corrected on 2026-07-12 after a cross-agent (Codex)
counter-review; the final code state is the commit this file lands on.

**Outcome**: 23 of 24 plans completed; 1 partial (013). Every plan was
executed by a dedicated executor agent in an isolated worktree, reviewed by
the orchestrator (diff read in full, done criteria re-run independently,
scope audited hunk-by-hunk), and merged with a review-record merge body.
Per-plan status with review annotations: `plans/README-claude-fable.md`.
Each `merge:` commit body on this branch is the detailed review record for
its plan.

**Final gates** (run at the wave-5 gate, `c1de241`; every wave gate passed
before the next wave was dispatched — note the wave-1 gate initially failed
four `watcher::tests::*` config-watcher tests and was closed only after
they went green):

- `cargo test --workspace`: 812 tests green (575 plug-core lib + 45
  integration + 192 plug bin), up from 730 at the implementation baseline
  `e341625` (511 + 43 + 176; net +82 — an earlier revision of this report
  wrongly said "802 at program start"; 802 was a mid-program measurement)
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
- `cargo +1.88.0 check --workspace` (MSRV): clean

## What merged (by category)

- **Correctness fixes**: 003 (four-bug batch), 008 (SSE replay tail loss +
  raced-sender enqueue gap), 009 (IPC read watchdog — 120s stall forces
  reconnect instead of a wedged mutex), 010 (per-URI atomic
  subscribe/unsubscribe state machine), 011 (reconnect/restart committed
  under reload_lock), 012 (shutdown_all retires replacement-grace tasks via
  a latched watch signal), 019 (HTTP session task teardown on DELETE/idle
  expiry), 020 (downstream-OAuth store hardening).
- **Tests**: 006 (IPC-proxy characterization baseline), 013 step 3 (HTTP
  upstream crash-restart supervision e2e), 024 (config-watcher e2e), 014
  (paused-time de-flake: 8 sleeps / 5 tests).
- **Perf**: 004 (catalog hot-path batch), 005 (artifact `spawn_blocking`
  oversized writes; re-scoped after a confirmed STOP), 023 (concurrent
  catalog family fetch: refresh under upstream latency 1.81s → ~604ms).
- **Toolchain/docs**: 001 (CI quick wins + MSRV reality-bump to 1.88), 002
  (todo/README truth reconciliation + guard script), 021 (rmcp ~1.7 tilde
  pin + upgrade policy), 022 (dispatcher plan-doc truth fix), 018
  (downstream-OAuth conformance spike doc), 017 (dispatch-unification
  design doc), 015 (shared notification fan-out classify/resolve), 016
  (move-only daemon.rs → `plug/src/daemon/` split; zero content drift
  proven by an independent line-multiset audit; test-count parity 78==78).

## Open findings / follow-up candidates (none block the merge)

1. **IPC `ping` gap (correctness, new)** — MCP `ping` over the daemon IPC
   proxy returns `UNSUPPORTED_METHOD` (catch-all arm, now in
   `plug/src/daemon/mcp_dispatch.rs`) where stdio and HTTP succeed; no
   parity-test coverage. Surfaced by plan 017's design; the doc sequences
   the fix as its own small PR. Verified in code.
2. **HTTP tier-1 parse-failure asymmetry** — a POST body failing JSON-RPC
   parsing gets a plain-text 400 (`plug-core/src/http/server.rs`,
   `post_mcp`), not a JSON-RPC error envelope. Pre-existing; flagged by
   plan 017 as a follow-up needing an operator decision.
3. **009 replay-reader-reuse** — watchdog expiry mid-frame during session
   replay leaves a torn-but-open stream (read_frame not cancel-safe;
   replay warns-and-continues across items). Strictly better than the
   prior infinite hang; recommended follow-up: abort replay + reconnect on
   first item failure.
4. **013 step 2 prerequisite** — the OAuth refresh-under-load e2e is not
   achievable tests-only: refresh scheduling is SystemTime-based
   (`oauth.rs` `token_needs_refresh`/`time_until_refresh_window`,
   `engine.rs` refresh loop) so paused tokio time cannot accelerate it,
   and `MIN_EXPIRES_IN = 60` forces ≥60s wall for two refresh windows.
   Follow-up: a small production knob (injectable clock or
   test-configurable expiry floor), then the e2e.
5. **reload.rs test gap (found by exec-014)** —
   `run_reload_start_actions_is_bounded_and_concurrent` never calls the
   real `run_reload_start_actions`; it reimplements the pattern inline and
   guards nothing. Candidate for a test-coverage pass.
6. **010 residuals (report-only, recorded in its merge body)** — two
   acknowledged residual races outside the plan's fix surface.
7. **003 bug-3 residual race (report-only, recorded in its merge body)**.
8. **015 drift observations** — daemon pushes `AuthStateChanged` as a
   native unfiltered `IpcResponse` where stdio/HTTP flatten to a logging
   broadcast; daemon no-ops on a closed control channel where stdio/HTTP
   break their fan-out loop. Both verified; inputs for a future
   notification-focused plan (also catalogued in plan 017's §5).

## Handoff

- `improve/integration` → `main` is the operator's decision; nothing has
  been pushed anywhere.
- On merge, run the repo post-merge checklist: promote the snapshot's
  "What Exists Off-Main" entry into a dated Release Status paragraph,
  retire `docs/PLAN.md`'s artifact-`spawn_blocking` remaining-work bullet
  (annotated on this branch), and re-verify the snapshot against `main`.
- Executor worktrees under `.claude/worktrees/` and the merged plan
  branches (`fix/...`, `test/...`, `perf/...`, `docs/...`, `refactor/...`)
  can be pruned after merge.
