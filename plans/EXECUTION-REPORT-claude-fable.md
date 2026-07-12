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

**Final gates** (run at `00364c8`, the last code commit — after the three
counter-review repairs; see "Counter-review repairs" below. Every wave
gate passed before the next wave was dispatched — note the wave-1 gate
initially failed four `watcher::tests::*` config-watcher tests and was
closed only after they went green; the program's pre-repair gate at
`c1de241` was also fully green at 812 tests):

- `cargo test --workspace`: 828 tests green (591 plug-core lib + 45
  integration + 192 plug bin), up from 730 at the implementation baseline
  `e341625` (511 + 43 + 176; net +98 — an earlier revision of this report
  wrongly said "802 at program start"; 802 was a mid-program measurement)
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
- `cargo +1.88.0 check --workspace` (MSRV): clean
- `cargo deny check advisories`: ok
- `scripts/check-todo-status.sh`: exit 0

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

## Counter-review repairs (2026-07-12)

After the 24-plan program finished at `48a48b0`, a cross-agent (Codex)
counter-review raised three defect claims. Each was adjudicated against
the code with independent adversarial verification before any repair was
dispatched; all three were confirmed (the 020 claim partially). Each
repair ran as its own executor in an isolated worktree and was reviewed
and merged like every plan (full diff read, per-commit scope audit,
independent re-run of tests — new concurrency tests 3× — with the review
record in the merge body):

- **Repair A (plan 019 follow-up)** — merged `926d7a5`. `cleanup_owner`
  removed task records on HTTP DELETE/idle expiry and IPC disconnect but
  never stopped the tasks: detached futures kept running (holding
  `max_concurrent` permits) and task-native upstreams were never sent
  `CancelTaskRequest`. Teardown now aborts each record's `JoinHandle`
  and forwards best-effort cancellation upstream; tests prove a
  long-running task actually stops for both HTTP and IPC owner styles
  (not merely that the index empties). Pre-existing primitive (since
  `8a0daea`), extended-but-not-fixed by plan 019.
- **Repair B (plan 010 follow-up)** — merged `00364c8`. A third
  refresh-race window distinct from the two residuals recorded in 010's
  merge body: `refresh_tools` publishes the new route snapshot before
  its rebind pass, so `rebind()` could revive an emptied entry as a
  zero-member Active subscription on the new owner, drains could be
  sent to the wrong upstream via route-cache-resolved handles, and a
  subscriber racing a prune became un-unsubscribable ("resource not
  found"). Fix: rebind drains emptied entries against the OLD owner
  instead of reviving; entries record their owning server id and drains
  resolve it at drain time under the transition lock. ToolRouter-level
  tests drive real `refresh_tools` passes against gated duplex
  upstreams. (Codex's proposed epoch/coordinator redesign was judged
  over-scoped; this narrow two-part fix covers all three confirmed
  manifestations.)
- **Repair C (plan 020 follow-up)** — merged `6c44496`. Client-credential
  scope comparison used raw `Vec<String>` equality, so permuted or
  duplicated scope lists defeated token reuse and minted extra live
  rows (RFC 6749 treats scope as a set); now canonicalized
  (sort + dedup) on both parse branches. The six silent `persist_state`
  failure exits and the silent corrupt-state-file fallback now emit
  `tracing::warn!` with error/path fields. Note: canonicalization does
  NOT bound hostile minting — scopes are free-form and secret-gated;
  the eager expiry sweep + 1h TTL remain the actual bound, as 020's
  review already recorded.

The counter-review also produced six truth corrections to this report,
the plans index, plan 020's addendum, the snapshot, and session memory —
all verified against primary evidence (git, a from-scratch baseline test
run, wave-gate transcripts) before being applied at `013d209`.

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
6. **010 residuals** — two acknowledged residual races were recorded in
   its merge body; the counter-review then confirmed a third window
   (publish-before-rebind), fixed on this branch by repair B
   (`00364c8`). One residual remains recorded-not-fixed after repair B
   (pre-existing supersede semantics, cross-owner case): a new
   subscriber superseding any drain before its upstream call can leave
   the old owner's subscription unreleased if the subscriber lands on a
   different owner.
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
