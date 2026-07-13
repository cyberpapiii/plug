# Improve-program execution report (2026-07-11/12)

**Branch**: `improve/integration` (local only, never pushed). Fork point:
local `main` @ `a2d8fd6` (a plans-only docs commit); implementation
baseline: `e341625` (the commit all plans were written against, equal to
`origin/main`'s runtime code). This report was written at the end of the
execution run and updated on 2026-07-12 after three cross-agent (Codex)
counter-review waves; the final code state is the commit this file
lands on.

**Outcome**: 23 of 24 plans completed; 1 partial (013). Every plan was
executed by a dedicated executor agent in an isolated worktree, reviewed by
the orchestrator (diff read in full, done criteria re-run independently,
scope audited hunk-by-hunk), and merged with a review-record merge body.
Per-plan status with review annotations: `plans/README-claude-fable.md`.
Each `merge:` commit body on this branch is the detailed review record for
its plan.

**Final gates** (most recently run at `6f8c2b8`, the last code commit —
after the two wave-3 counter-review repairs; see the three
"Counter-review repairs" sections below. Every wave gate passed before
the next wave was dispatched — note the wave-1 execution gate initially
failed four `watcher::tests::*` config-watcher tests and was closed only
after they went green; the program's pre-repair gate at `c1de241` was
fully green at 812 tests, the wave-1 repair gate at `00364c8` at 828,
and the wave-2 repair gate at `6d3e59a` at 843):

- `cargo test --workspace`: 853 tests green (616 plug-core lib + 45
  integration + 192 plug bin), up from 730 at the implementation baseline
  `e341625` (511 + 43 + 176; net +123 — an earlier revision of this report
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
  the eager expiry sweep + 1h TTL bound retention time, not row
  cardinality (a hostile secret-holder can still mint arbitrarily many
  rows inside a TTL window; the sweep only guarantees they die within
  the hour), as 020's review already recorded.

The counter-review also produced six truth corrections to this report,
the plans index, plan 020's addendum, the snapshot, and session memory —
all verified against primary evidence (git, a from-scratch baseline test
run, wave-gate transcripts) before being applied at `013d209`.

## Counter-review repairs, wave 2 (2026-07-12)

Codex's second counter-review (against `f244a60`) independently
reproduced all six wave-1 gates, then raised three lifecycle defect
claims and one credential-write claim. All four were adjudicated
against the code and confirmed by an eight-verifier adversarial
workflow, with calibrated impact: the 010 skew produces no errors or
leaks — its harm is silently missed `resources/updated` notifications,
previously *permanent*; the unbounded-teardown worst consumer is the
daemon's single serialized idle-expiry loop (a daemon-wide wedge);
the credential-write hazard sits behind stacked preconditions (low).
Its wording corrections (literal `exists off-main` in `docs/PLAN.md`,
TTL as retention-time not cardinality, dropping the "none block the
merge" claim) were applied at `98df2fa`. Each repair then ran under the
same protocol as wave 1 — dedicated executor, isolated worktree, full
diff review, independent re-runs (new concurrency tests 3×), review
record in the merge body:

- **Repair D (plan 010 follow-up)** — merged `89bcb78`. A first
  subscriber landing inside `refresh_tools`' classify→publish window
  bound to the old owner permanently: the running pass classified
  before the entry existed, and every later refresh compared route
  snapshots (new==new) rather than the entry's actual owner. Fix:
  classify now compares each entry's recorded `owner_server_id`
  against the NEW snapshot at every refresh (unconfirmed entries keep
  route-diff behavior), and `subscribe_resource` runs a one-shot
  post-subscribe self-check that rebinds if the route moved
  mid-subscribe. Deterministic first-subscribe-during-refresh tests
  cover both interleavings; the executor proved all six pinning tests
  fail with the fix reverted. (Codex's route-epoch/commit-coordination
  remedy was again judged over-scoped; this is the "equivalent
  authoritative revalidation" its report allowed for.) This report
  originally claimed the remaining windows were "bounded to one refresh
  period" — Codex's wave-3 review falsified that: refreshes are purely
  event-driven, so no next refresh is guaranteed. Repair G (wave 3)
  made the heal same-refresh.
- **Repair E (plan 019 follow-up)** — merged `6d3e59a`. Teardown was
  serial and unbounded (rmcp's plain `send_request` has no timeout, so
  one silent upstream blocked later handles' aborts and could hang
  HTTP DELETE, IPC teardown, or the idle-expiry loop), and task
  creation raced the cleanup boundary (native path registered only
  after the upstream round trip; local path created/spawned/attached
  in three lock scopes, allowing a handle-less drain to detach a
  running future still holding its `max_concurrent` permit). Fix:
  abort-all-local-handles-first, then concurrent upstream
  cancellations each bounded by that server's `call_timeout_secs`
  (`cancel_task_for_owner` got the same bound, sync-back unchanged);
  a per-owner lifecycle ledger (in-flight-create counter + tombstone,
  entries die at count zero) makes teardown refuse late creates —
  the native path cancels the just-created upstream task before
  returning the error — and the local path now creates/spawns/attaches
  in one lock scope. Gated hung-upstream and create-vs-teardown
  regression tests, paused-time bounded.
- **Repair F (plan 020 follow-up)** — merged `ccd38c9`. `persist_state`
  silently ignored a temp-file chmod failure (`let _ =`) and renamed
  the possibly-loose file into place as the plaintext token store; a
  stale crash-left temp also kept its old mode through truncation, and
  the final file's permissions were never enforced post-rename (unlike
  the sibling upstream store). Fix: stale temp removed before open,
  chmod failure warns + removes the temp + returns without renaming,
  and post-rename 0600 enforcement mirrors `oauth.rs`. Unix-gated
  tests pin the stale-loose-temp and fresh-persist cases.

Wave-2 final gates all passed at `6d3e59a`: 843 workspace tests, clippy
`-D warnings`, fmt, MSRV 1.88 check, `cargo deny check advisories`, and
the todo-status guard.

## Counter-review repairs, wave 3 (2026-07-12)

Codex's third counter-review (against `3763ad4`) approved repair F and
the truth pass outright, verified the branch clean/unpushed/
fast-forwardable, and re-ran the full suite — noting one pre-existing
TLS-test startup flake (a single connection refusal on its first run;
3/3 green in isolation; recorded here report-only, no repair). It then
returned exactly plans 010 and 019 for "one bounded revision" with four
claims. All four were adjudicated against the code and confirmed; two
were worse than stated (the silent-success self-check was a regression
introduced by repair D itself, and the unbounded native round trip
could hold an owner-create guard forever). Before dispatch, both remedy
designs were subjected to a four-verifier adversarial refutation
workflow; every verifier found real holes (two blocking), and the
designs were revised before any executor ran. Same per-repair protocol
as waves 1–2:

- **Repair G (plan 010, second follow-up)** — merged `7411fc2`
  (executor branch @ `3fae883`). Claims closed: repair D's heal
  depended on a *next* event-driven refresh that may never fire, and
  its post-subscribe self-check discarded the rebind outcome (silent
  Ok with no live subscription). Fix, five parts: a reconcile-phase
  mutex serializes classify→prune→publish→rebind across overlapping
  refreshes; a DETACHED post-publish sweep re-classifies recorded
  owners against the just-published snapshot inside the triggering
  refresh (immune to the 600s notification-loop backstop dropping the
  refresh future); `rebind` propagates its transition outcome; a
  post-confirm hook fires the heal from the uncancellable transition
  task (a downstream disconnect can no longer kill it); and the
  self-check compares the entry's RECORDED owner (so retries heal) and
  answers from a final membership verify (a superseded transition's
  laundered Ok can no longer mask a failed migration — the client gets
  an explicit retry error). Four deterministic tests: same-refresh
  heal, caller-aborted heal, retry heal, failed-migration propagation.
  Approved deviation: the sweep executes rebinds only — sweep-side
  prunes would break the pre-existing grace window for routeless
  racing subscribers.
- **Repair H (plan 019, second follow-up)** — merged `6f8c2b8`
  (executor branch @ `17e3aaa`). Claims closed: a teardown completing
  entirely before enqueue registered its guard left the create
  invisible to the tombstone (with no hard TTL bound — `prune_expired`
  is opportunistic only), and upstream work sent before its reference
  was published locally (native create, wrapper send-to-record gap)
  had no cancellation path. Fix, three parts: an owner-liveness probe
  re-checked immediately AFTER guard registration (HTTP:
  session-store validate; IPC: client-registry membership; stdio:
  none — no teardown path), sound via the documented happens-before
  argument (guard before probe, session removal before cleanup,
  cleanup tombstones in-flight creates); the native round trip is now
  detached (a spawned task owns the create guard — a dropped POST
  future can neither release it early nor orphan the upstream task),
  bounded by `call_timeout_secs` with an explicit bounded
  request-level cancel on timeout, and backed by a reaper that cancels
  a late-created task by id; the wrapper gap is covered by an RAII
  abort-cancel guard plus a three-state `set_upstream_request` whose
  `Missing` outcome sends its own bounded cancel. Seven deterministic
  tests, including the full POST-vs-DELETE interleaving and
  held-store-lock parking for the send-to-record gap.

Wave-3 final gates all passed at `6f8c2b8` (see "Final gates" above):
853 workspace tests, clippy `-D warnings`, fmt, MSRV 1.88 check,
`cargo deny check advisories`, and the todo-status guard.

## Open findings / follow-up candidates

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
   its merge body; the counter-reviews then confirmed a third window
   (publish-before-rebind), fixed on this branch by repair B
   (`00364c8`), a fourth (first-subscribe-during-refresh permanent
   skew), fixed by repair D (`89bcb78`), and a fifth wave (heal
   depended on a next refresh that event-driven scheduling never
   guarantees, plus a repair-D-introduced silent-success self-check),
   fixed by repair G (`7411fc2` — same-refresh sweep, uncancellable
   post-confirm heal, recorded-owner retry heal, failure propagation).
   What remains is recorded, not fixed: the pre-existing cross-owner
   supersede case (a new subscriber superseding any drain before its
   upstream call can leave the old owner's subscription unreleased if
   the subscriber lands on a different owner); upstream
   subscribe/unsubscribe calls inside transitions are unbounded
   (pre-existing — a wedged upstream can stall that URI's per-URI
   transition queue); superseded transitions still report Ok to their
   own waiter (neutralized at the subscribe caller by repair G's
   membership verify; refresh-path callers log only); and a sweep from
   an older pass can overlap a newer pass's reconcile phase
   (generation supersede plus the newer pass's own sweep converge it).
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
