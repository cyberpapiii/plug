# Improvement Plans — Claude Fable run, 2026-07-11

Plans 001-018 were produced by the Claude Fable `/improve` run on
2026-07-11; plans 019-024 by the agreed follow-up pass on 2026-07-12 (the
prioritized "019+" batch from the cross-agent review). All are planned
against commit `e341625` (clean `main`). Files are
suffixed `-claude-fable.md` because **another agent (Codex) was running the
same workflow in this repo concurrently** — if unsuffixed or
`-codex`-suffixed plan files appear, they are the other run's output;
reconcile numbering/overlap before executing either set (the two runs may
have planned the same finding twice).

Prior-run history: an improve-audit hardening batch merged 2026-07-03 (see
`docs/PROJECT-STATE-SNAPSHOT.md`); its plans were never committed to
`plans/`, so numbering here starts at 001.

Every plan is self-contained (an executor needs no context beyond the plan
file), stamped with the planned-at commit, and begins with a drift-check
command. Executors: update your plan's Status cell here when you start
(`IN PROGRESS`), finish (`DONE <date>`), or hit a STOP condition
(`BLOCKED: <one line>`).

## Execution order and status

Recommended order below is grouped by track; tracks are independent of each
other except where the Depends column says otherwise. Within a track, order
matters.

| # | Plan | Category | Effort | Depends on | Status |
|---|------|----------|--------|------------|--------|
| 001 | [Toolchain/CI quick wins](001-toolchain-ci-quick-wins-claude-fable.md) — fix 2 clippy-1.97 failures (gate is currently RED), advisories gate, MSRV job, oauth2 default-features, fs2→fs4, shared 0700-dir helper | tooling | S | — (landed FIRST: clippy gate green) | DONE 2026-07-12 (merged to improve/integration @ 49426d4; 8 commits — incl. MSRV 1.86→1.88 amendment, 1.88-lint fix-up across 19 files, third 0700 site; see plan amendments) |
| 002 | [Todo/README hygiene](002-todo-readme-hygiene-claude-fable.md) — reconcile 6 contradictory todo statuses, guard script, README staleness | docs | S | — | DONE 2026-07-12 (merged to improve/integration @ fa2471d; 2 amendments — 045 residual, 062 had no frontmatter) |
| 003 | [Correctness small-fix batch](003-correctness-small-fix-batch-claude-fable.md) — eviction metric, daemon busy-spin, pending-cancel-before-attach, idle-select reverse arm | correctness | S/M | — | TODO |
| 004 | [Catalog perf batch](004-catalog-perf-batch-claude-fable.md) — pagination clones, refresh-loop hoists, gated filtered-catalog builds | perf | M | — | DONE 2026-07-12 (merged to improve/integration @ f4b489e; 1 criterion amendment) |
| 005 | [Async artifact write](005-artifact-async-write-claude-fable.md) — spawn_blocking for ≥16MB payload writes (PR #58 residual) | perf | S | after 004+019 merge | DONE 2026-07-12 (merged to improve/integration @ 25c7e88 under the re-scope amendment; first attempt STOPPED correctly on the sync-fn mismatch; +1 attachment round-trip test) |
| 006 | [ipc_proxy characterization tests](006-ipc-proxy-characterization-tests-claude-fable.md) — pin reconnect/retry/framing/stall behavior | tests | M | — | DONE 2026-07-12 (merged to improve/integration @ d8bf512; 7 tests via fake-daemon harness; 1 amendment — malformed-frame reconnectability facts for 009) |
| 007 | [IPC reconnect state replay](007-ipc-reconnect-state-replay-claude-fable.md) — replay caps/subscriptions/log level after daemon restart | correctness | M | 006 | TODO |
| 008 | [SSE replay integrity](008-sse-replay-integrity-claude-fable.md) — replay-queue tail loss + raced-sender enqueue gap | correctness | M | — (003 same file, trivial) | TODO |
| 009 | [IPC read watchdog](009-ipc-read-watchdog-claude-fable.md) — 120s silence → reconnect instead of wedged mutex | correctness | S/M | 006 (007 first preferred) | TODO |
| 010 | [Subscription registry atomicity](010-subscription-registry-atomicity-claude-fable.md) — per-URI state machine + transition locks; downstream, cleanup, and catalog prune/rebind all route through one coordinator | correctness | M/L | — (NOT parallel with 004 or 023: same refresh_tools) | TODO |
| 011 | [Reconnect/reload interlock](011-reconnect-reload-interlock-claude-fable.md) — removed/reconfigured server can't be resurrected by a late reconnect or manual restart (v2: commit under `Engine::reload_lock`) | correctness | M | — | TODO |
| 012 | [Grace-retire task tracking](012-grace-retire-task-tracking-claude-fable.md) — shutdown_all retires replacement-grace tasks immediately | correctness | S/M | 011 (soft, same file) | TODO |
| 013 | [OAuth + supervision e2e tests](013-oauth-supervision-e2e-tests-claude-fable.md) — expiry→refresh→call under load; HTTP upstream crash-restart | tests | M | — | PARTIAL 2026-07-12 — step 3 (HTTP crash-restart e2e) merged to improve/integration @ e511382; step 2 (refresh-under-load) DEFERRED behind plan 014 (MIN_EXPIRES_IN=60 floor; see plan amendment) |
| 014 | [Test time de-flake](014-test-time-deflake-claude-fable.md) — paused time / condition polls for wall-clock sleeps | tests | M | after 006/007/009/013 preferred | TODO |
| 015 | [Notification fanout dedup](015-notification-fanout-dedup-claude-fable.md) — shared classify/resolve; per-transport delivery only | debt | M | — | TODO |
| 016 | [daemon.rs module split](016-daemon-module-split-claude-fable.md) — move-only decomposition into daemon/ submodules | debt | M/L | 015 (and 003, 014 landed) | TODO |
| 017 | [Dispatch unification decision](017-dispatch-unification-design-claude-fable.md) — design doc: per-family migrate-vs-keep verdicts for dispatch/ (docs only) | direction | M | reads best after 015 | TODO |
| 018 | [Downstream OAuth conformance spike (todo 057)](018-downstream-oauth-conformance-spike-claude-fable.md) — RFC 8414/9728 gap matrix + live probes (docs only; also owns the scope-semantics question handed over by 020) | direction | M | — | DONE 2026-07-12 (merged to improve/integration @ 87d8fe2; discovery surface fully conformant; 3 gaps F1–F3/F4 triaged spec-gap-no-known-impact → next planning round; reconciled vs merged 020) |
| 019 | [HTTP session task teardown](019-http-session-task-teardown-claude-fable.md) — clean a departing HTTP session's tasks on DELETE and idle expiry (mirrors existing IPC teardown; the one asymmetric per-session map) | bug | M | — (015 also touches http/server.rs: sequence) | DONE 2026-07-12 (merged to improve/integration @ 834d42f) |
| 020 | [Downstream OAuth store hardening](020-downstream-oauth-store-hardening-claude-fable.md) — eager expiry sweep + client_credentials token reuse; pins unenforced-scope behavior with a characterization test | security | M | — | DONE 2026-07-12 (merged to improve/integration @ 0bfa607; 1 criterion amendment — evict_expired grep-count collision) |
| 021 | [rmcp version pin policy](021-rmcp-version-pin-policy-claude-fable.md) — `~1.7` tilde pin + CRATE-STACK policy note (protects unlocked installs and broad `cargo update`) | deps | S | 001 (soft; same Cargo.toml) | DONE 2026-07-12 (merged to improve/integration @ e94d330) |
| 022 | [PLAN.md dispatcher truth fix](022-plan-doc-dispatcher-truth-fix-claude-fable.md) — remove the "only remaining dispatcher item"/"decomposition also remains" claims contradicted by the ✅ entries two lines below (docs only; the memory-contamination root cause) | docs | S | — | DONE 2026-07-12 (merged to improve/integration @ ed0fc62) |
| 023 | [Catalog family concurrent fetch](023-catalog-family-concurrent-fetch-claude-fable.md) — `tokio::join!` the three live family getters in `refresh_tools` (sum→max of family latencies; servers were already concurrent within each family) | perf | S/M | — (NOT parallel with 004 or 010: same refresh_tools) | TODO |
| 024 | [Config-watcher e2e tests](024-config-watcher-e2e-tests-claude-fable.md) — disk change → debounce → reload chain, parse-error recovery, sibling filter, rename-replace; zero production changes | tests | M | — | DONE 2026-07-12 (merged to improve/integration @ c6307ad) |

## Dependency notes (the ones that bite)

- **006 → 007 → 009**, strictly. 006 pins current ipc_proxy behavior with two
  `CHARACTERIZATION:` tests that 007 and 009 deliberately flip. All three
  edit the same functions — execute sequentially, never in parallel.
- **015 → 016.** 016 moves the daemon fanout block; 015 thins it first. 016
  also wants 003 and 014's daemon.rs edits already landed (it's the
  merge-conflict hotspot — 016 goes last among daemon.rs plans).
- **011 → 012** (soft): both edit `replace_server`'s vicinity; 011 first
  avoids conflicts. 012 must NOT shorten the grace period (documented
  negative decision in the plan).
- **003 and 008** touch the same file (`stateful.rs`) at disjoint functions —
  either order, 003 first is trivial.
- **013's supervision e2e** is the regression guard for 011/012 — landing 013
  before them is worth it.
- **004, 010, and 023 all edit `proxy/mod.rs`'s `refresh_tools`** (004:
  perf hoists; 010: prune/rebind block onto the coordinator; 023: the
  four-await head onto `tokio::join!`) — mutually sequence, any order,
  never in parallel; whichever lands later re-anchors by function name.
- **019 and 015 both edit `plug-core/src/http/server.rs`** — sequence them
  (either order; 019 is the smaller diff).
- **021 after 001** (both edit the workspace `Cargo.toml`; 001 lands first
  anyway).
- **Parallel-safe starting set** (disjoint files): 001, 002, 004, 005, 006,
  013, 018, 019, 020, 022, 024 (010 and 023 conflict with 004; 021 waits
  for 001).

## Suggested overall sequence

Quick wins first (001–005), then the IPC track (006, 007, 009), then
remaining correctness (008, 010, 011, 012) with 013 landed before 011/012,
then 014, then debt (015, 016), with the two docs-only plans (017, 018)
anytime.

Second batch (019-024), in the cross-review's agreed priority order:
019 + 020 first (session-teardown correctness and auth hardening), then
021 + 022 (dependency and truth-doc reliability, both small), then 023
(performance — slot it where 004/010 aren't running) and 024 (coverage)
anytime.

## Corrections log (2026-07-11, post cross-agent review)

After comparing against the concurrent Codex audit of the same commit and
re-verifying against the tree, the following corrections were applied to the
plans above:

- **Plans 015/017 + this index**: the "IPC identity split is deferred" claim
  came from stale project memory — the split has LANDED on `main`
  (`DownstreamTransport::Ipc`, `NotificationTarget::Ipc`). Both plans and the
  out-of-scope list were rewritten accordingly.
- **Wrong paths fixed**: dispatch module is `plug-core/src/dispatch/` (not
  `plug/src/dispatch/`); stdio handler is `plug-core/src/proxy/handler.rs`
  (not `plug/src/proxy/…`); subscription registry is
  `plug-core/src/proxy/subscriptions.rs` (plans 010/015/017).
- **Plan 016**: daemon.rs is 4,987 lines total (≈2,640 production + ≈2,350
  test), not "~2,650 lines" — anchors were and remain correct.
- **Plan 001 gained step 0** (found by the Codex run, verified live here):
  clippy is currently RED on stable 1.97 — `question_mark` at
  `artifacts.rs:482`, `for_kv_map` at `server/mod.rs:774`. Every plan's
  clippy done-criterion is blocked until plan 001 step 0 lands.

**Round 5 (2026-07-12, execution correction to plan 010 — loop frozen after
this):**

- **Plan 010 scope was structurally incomplete**: the registry is declared
  on the router struct (`proxy/mod.rs:174`), not in subscriptions.rs, and
  the catalog refresh (`refresh_tools`'s prune/rebind region,
  mod.rs:1439-1620; the function itself starts at :1081) is a SECOND
  mutation source — it retains-away pruned entries before awaiting their
  upstream unsubscribes and issues rebind unsubscribe/subscribe pairs with
  no coordination against downstream transitions (race 4, now in the plan).
  Amended: state extracted to `Arc<SubscriptionRegistry>` so detached
  transition tasks can own a handle; prune/rebind route through the same
  per-URI coordinator (decision logic and failure policy verbatim);
  transition tasks spawn on the Engine's existing `TaskTracker`
  (engine.rs:299) instead of raw `tokio::spawn` (avoiding a new plan-012
  instance — the round-4 "no tracking machinery" wording was wrong, the
  tracker already exists); new `rebind_serializes_against_downstream_transitions`
  test; effort M → M/L; 010 removed from the parallel-safe set (conflicts
  with 004 in `refresh_tools`).

**Round 4 (2026-07-12, final surgical pass — adversarial loop frozen after
this):**

- **Plan 010**: transition ownership moved from request futures to detached
  tasks. An RAII lock guard owned by a cancellable request future releases
  the URI transition lock on drop while the already-sent upstream
  unsubscribe can still land remotely (cancellation is advisory and races
  completion) — reopening the ordering race through the cancellation path.
  The cancelled-subscribe test was reworked (the cohort now sees the real
  outcome, not a synthetic Err) and
  `last_unsubscriber_cancelled_mid_flight_then_new_subscriber` added as the
  regression.
- **Plan 011 (v2.2)**: the retry-loop early-exit now also uses
  `server_config_changed` — it still said "no longer equals the snapshot",
  which was the same non-material-reload strand-the-server-down bug one
  layer before commit. New regression test:
  `retry_loop_survives_non_material_reload`.
- **Plan 010**: the `git stash`-based negative check was removed (a stash in
  this shared, concurrently-edited worktree can capture the other agent's
  changes) — tests are now written before the fix, with a
  disposable-worktree fallback pinned to the planned-at commit.

**Round 3 (same day, Codex's second counter-review — two design fixes):**

- **Plan 010: upstream ordering promoted from STOP condition to design
  requirement.** The v2 design let a subscriber arriving during `Draining`
  issue its upstream subscribe unordered against the in-flight unsubscribe —
  if the subscribe completed first, the upstream ended unsubscribed under an
  `Active` registry entry (subscribe/unsubscribe don't commute; idempotence
  only protects sub-after-sub). Now: a persistent per-URI async transition
  lock serializes ALL upstream subscribe/unsubscribe calls; drain-racing
  subscribers wait. Tests upgraded to assert completion order + final
  modeled upstream state (they previously passed even under the broken
  ordering), plus a new cancellation test for the `watch::Sender` owner.
- **Plan 011 (v2.1): comparison contract corrected.** v2 compared whole
  `ServerConfig` structs; reload's materiality predicate
  (`server_config_changed`, reload.rs:203) is narrower (omits
  `max_concurrent`, `circuit_breaker_enabled`, `enrichment`,
  `tool_renames`, `tool_groups`, `sandbox`) — so a non-material reload
  would have made commit discard a good reconnection and strand the server
  down while returning Ok. Commit now shares reload's predicate (one
  definition of "materially changed"); the `PartialEq` derive and
  serde-value fallback are gone (`SecretString` has no `PartialEq` and its
  transparent `Serialize` emits plaintext — the fallback was unsafe as well
  as wrong). New regression test: non-material change → `Committed`.

**Round 2 (same day, after Codex's counter-review of these plans):**

- **Plan 011 redesigned (v2).** v1's design validated inside
  `replace_server` "under its existing write lock" — no such lock exists
  (the body is a bare DashMap insert); `replace_server` has a second
  production caller v1 missed (`Engine::restart_server`, engine.rs:481,
  reached from two daemon dispatch sites); and reload stores the new config
  only AFTER its stop/start work, so comparing against the ArcSwap alone
  cannot detect an in-flight reload. v2 commits under `Engine::reload_lock`
  (connect outside the lock, revalidate + install-or-discard under it) and
  covers both callers.
- **Plan 010 mechanism text corrected.** The subscribe-failure rollback
  removes only the initiating target — raced piggy-backers stay registered
  against no upstream subscription and the surviving entry poisons future
  subscribers; unsubscribe removes the registry entry BEFORE awaiting
  upstream (result discarded), racing fresh subscribers on both the registry
  and the upstream side; `cleanup_subscriptions_for_target` (same shape at
  fan-out scale) added to scope. The fix design (watch + generation state
  machine) was already compatible and stands.
- **Plan 017 reframed to verdict-first.** The dispatch routing core is
  already shared (per `dispatch/mod.rs`'s own doc) and `docs/PLAN.md`
  records full-surface parity coverage since PR #64 — so the design must
  decide migrate-vs-keep per method family instead of presupposing
  migration; a leftover done-criterion about the (landed) identity split was
  removed.
- **Plan 001**: noted that steps after step 0 are independent and may ship
  as separate PRs (per-step commits already made that mechanical).

**Second planning pass (2026-07-12) — the 019+ batch.** The Codex-unique
findings plus the PLAN.md contradiction were investigated against the tree
and planned as 019-024. Verification outcomes worth recording:

- **Task teardown (→ 019)**: CONFIRMED, and sharpened — `task_store` is the
  single per-session map cleaned on both IPC teardown paths
  (daemon.rs:912/:1700) but on NEITHER HTTP path (`delete_mcp`
  http/server.rs:938-962 and the idle-expiry consumer runtime.rs:257-283
  each clean eight other structures). Bounded by lazy wall-clock TTL
  pruning, so a slow leak, not unbounded.
- **Downstream OAuth (→ 020)**: MIXED — the persistence claim is a FALSE
  alarm (rejected below); lazy-only store eviction and per-call
  client_credentials minting are real bounds gaps; scope enforcement is
  genuinely absent (`validate_access_token` returns a bool and discards
  scopes). Scope SEMANTICS were deliberately routed to plan 018's spike
  rather than invented in an executor plan.
- **rmcp caret pin (→ 021)**: CONFIRMED with mitigations already present
  (committed Cargo.lock, `--locked` in every documented install) — the
  residual float paths (unlocked `cargo install`, broad `cargo update`,
  published-crate requirement) justify a tilde pin only.
- **PLAN.md contradiction (→ 022)**: CONFIRMED and slightly worse — line 94
  carries TWO stale claims (KTD3 "only remaining" AND decomposition "also
  remains"), each contradicted by the ✅ bullets at lines 95-96. The
  snapshot needs no edits (its "still deferred" lines are dated history).
- **Serial catalog fetch (→ 023)**: HALF-CONFIRMED — the four families are
  serial at the head of `refresh_tools` (mod.rs:1082-1085), but the
  "servers iterated serially" half is FALSE (rejected below).
- **Config-watcher coverage (→ 024)**: CONFIRMED in full — watcher.rs has
  zero tests; even the daemon runtime tests bypass `cmd_daemon`, the only
  spawn site.

## Findings considered and rejected (do not re-audit)

Recorded so the next run doesn't re-litigate:

- **health/availability JSON casing inconsistency** — pinned wire contract;
  consumers exist; churn > value.
- **`try_register` TOCTOU in the client registry** — window is theoretical
  under the daemon's single accept loop; not reachable as described.
- **half-registered client window during connect** — bounded, self-healing,
  no observed effect; revisit only if a real symptom appears.
- **register-vs-subscribe ordering at session start** — behavior is correct
  per protocol; reordering adds risk for no gain.
- **`reconnecting` flag clear timing** — reviewed; current lifecycle is
  correct; the auditor's proposed change would introduce a gap.
- **rand 0.8/0.9/0.10 triplication** — not fixable from this repo (oauth2
  core pins rand 0.8); upstream problem.
- **Auditor fix-sketches corrected during vetting** (the finding was real,
  the sketch wrong): DEPS-01 "pin reqwest 0.12" → correct fix is
  `oauth2 default-features = false` (plan 001); CORRECTNESS-10 "cancel old
  upstream token before grace sleep" → would kill in-flight requests; correct
  fix is task tracking (plan 012).
- **"Downstream-OAuth tokens don't persist across restarts"** — FALSE
  (verified 2026-07-12): access + refresh tokens survive restart via an
  atomic temp+rename 0600 JSON state file (downstream_oauth/mod.rs:501-554,
  loaded in `new`), pinned by the module's own
  `issued_tokens_survive_manager_recreation` test. Auth codes are
  intentionally ephemeral. There is no dynamic client registration, so
  there is no per-client registration state to persist.
- **"Catalog refresh iterates servers serially"** — FALSE for the three
  live families (verified 2026-07-12): `get_resources` /
  `get_resource_templates` / `get_prompts` each `join_all` across servers
  with per-server timeouts (server/mod.rs:1313 etc.); `get_tools` loops
  serially but reads an in-memory cache (no I/O). Only the FAMILY-level
  serialization was real → plan 023.
- **Renaming `oauth_authorize_not_implemented`/`oauth_token_not_implemented`
  handlers** (http/server.rs:1074/:1118) — the names are stale (bodies are
  fully implemented) but renaming is cosmetic churn in a file three plans
  touch; noted inside plans 018/020 instead so no one mistakes them for
  stubs.

## Out of scope this round (deliberate)

- Fully live runtime reconfiguration (CLAUDE.md: out of current
  production-ready bar).
- Session-store stateless backend (seam exists; direction option not
  selected this round).
- Unified runtime-state view (direction option not selected this round).
