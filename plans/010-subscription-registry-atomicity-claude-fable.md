# Plan 010: Make upstream resource subscribe/unsubscribe atomic per URI

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README-claude-fable.md`.
>
> **Drift check (run first)**: `git diff --stat e341625..HEAD -- plug-core/src/proxy/subscriptions.rs plug-core/src/proxy/mod.rs`
> If the file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. Another AI agent (Codex) may be working in
> this repo concurrently.

## Status

- **Priority**: P2
- **Effort**: M/L
- **Risk**: MEDIUM (two-lock design: the sync registry guard is never held
  across an await; a per-URI async transition lock is deliberately held
  across upstream calls — see the lock-order rule in the fix design)
- **Depends on**: none — but **plan 004 (catalog perf) edits the same
  `refresh_tools` region of `proxy/mod.rs`; do NOT execute 004 and 010 in
  parallel** (either order is fine, sequenced)
- **Category**: correctness
- **Planned at**: commit `e341625`, 2026-07-11

## Why this matters

The subscription registry (`plug-core/src/proxy/subscriptions.rs`) fans multiple
downstream clients' `resources/subscribe` calls into at most ONE upstream
subscription per URI (first subscriber triggers the upstream call; later
subscribers piggy-back; last unsubscriber triggers upstream unsubscribe).
The check-then-act sequences are not atomic across the upstream round-trip:

1. **Subscribe race** (`:44-72`): `is_first` is computed and the target
   inserted under the entry guard, the guard is dropped, then the upstream
   `subscribe` call is awaited. A second client subscribing the same URI in
   that window sees "not first" and piggy-backs — on an upstream subscription
   that does not exist yet and may FAIL. The failure rollback (`:59-66`)
   removes ONLY the initiating target (the whole entry only if that leaves it
   empty) — so the raced piggy-backer STAYS registered against an upstream
   subscription that never came to exist: it believes it is subscribed and
   receives nothing. Worse, the surviving non-empty entry poisons every
   FUTURE subscriber of that URI: each sees `is_first == false` and
   piggy-backs onto nothing, until the entry happens to empty out.
2. **Unsubscribe race** (`:93-117`): the target is removed and emptiness
   computed inside a scoped guard (`:93-100`); the whole entry is then
   removed OUTSIDE that guard (`:103`) — unconditionally, so a subscriber
   that lands in the gap is silently wiped — and only AFTER that removal is
   the upstream `unsubscribe` awaited (`:105-115`, result discarded with
   `let _`). A fresh subscriber arriving after `:103` creates a new entry,
   sees `is_first`, and issues an upstream `subscribe` UNORDERED against the
   in-flight `unsubscribe`: if the unsubscribe lands second, the upstream
   drops the subscription while the registry says the new client is
   subscribed — a zombie again, this time from the upstream side.
3. **Disconnect-cleanup race** (`:126-166`):
   `cleanup_subscriptions_for_target` repeats shape (2) at fan-out scale — it
   `retain`s all entries first (removing the target and dropping emptied
   entries synchronously), then awaits the collected upstream unsubscribes
   afterward, unordered against any concurrent subscriber.
4. **Catalog-refresh prune/rebind races**
   (`plug-core/src/proxy/mod.rs:1439-1620`, inside `refresh_tools`): the
   catalog refresh mutates the SAME registry and issues upstream calls
   OUTSIDE this module — vanished-URI entries are removed by a `retain`
   (`:1446`) before their upstream unsubscribes are awaited (`:1509-1513`),
   and route-ownership rebinds issue unsubscribe-old (`:1551`) /
   subscribe-new (`:1594`) pairs with no coordination against concurrent
   downstream subscribe/unsubscribe for the same URIs (rebind-failure
   pruning also mutates the registry directly, `:1609`, `:1616`). Same
   unordered-transition zombie class as (2)/(3), from a second mutation
   source.

These are the same-shaped race the SSE sender-clobber fix (commit `98281c8`)
closed on the session side. Real trigger: two AI clients (Claude Desktop +
Claude Code via daemon) subscribing the same resource at connect time — plug's
core multi-client scenario.

## Current state

Verified at commit `e341625` in `plug-core/src/proxy/subscriptions.rs` (whole
file is 182 lines — read it all before editing). Excerpts below are the REAL
code, abbreviated (re-verified line by line 2026-07-11 after a cross-agent
review corrected this section's earlier paraphrase).

Real shapes:

- registry: `resource_subscriptions` is a `DashMap<String, HashSet<NotificationTarget>>`
  keyed by URI (the guard is a synchronous dashmap entry/`get_mut` ref —
  which is why it cannot simply be held across the upstream await)
- `pub async fn subscribe_resource(&self, uri: &str, target: NotificationTarget) -> Result<(), McpError>` (`:9`)
- `pub async fn unsubscribe_resource(&self, uri: &str, target: &NotificationTarget) -> Result<(), McpError>` (`:80`)
- `pub async fn cleanup_subscriptions_for_target(&self, target: &NotificationTarget)` (`:126`)
- `route_upstream_resource_updated` (`:169`) clones an entry's target set to
  fan out notifications — read path only

Subscribe (`:44-72`):

```rust
let mut entry = self
    .resource_subscriptions
    .entry(uri.to_string())
    .or_default();
let is_first = entry.is_empty();
entry.insert(target.clone());
drop(entry);

if is_first {
    if let Err(error) = upstream.client.peer()
        .subscribe(SubscribeRequestParams::new(uri))
        .await                                        // <- race window
    {
        // Roll back the local subscription on upstream failure
        if let Some(mut entry) = self.resource_subscriptions.get_mut(uri) {
            entry.remove(&target);        // ONLY the initiator — raced
            if entry.is_empty() {         // piggy-backers survive, registered
                drop(entry);              // against no upstream subscription
                self.resource_subscriptions.remove(uri);
            }
        }
        return Err(/* mapped error */);
    }
}
```

Unsubscribe (`:93-117`):

```rust
let should_unsubscribe_upstream = {
    let mut entry = match self.resource_subscriptions.get_mut(uri) {
        Some(e) => e,
        None => return Ok(()),
    };
    entry.remove(target);
    entry.is_empty()
};                                        // guard dropped

if should_unsubscribe_upstream {
    self.resource_subscriptions.remove(uri);   // unconditional — wipes any
                                               // subscriber landing in the gap
    if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
        let _ = upstream.client.peer()
            .unsubscribe(/* uri */)
            .await;                            // <- AFTER removal, unordered vs
                                               // a fresh subscribe, result discarded
    }
}
```

`cleanup_subscriptions_for_target` (`:126-166`) repeats the unsubscribe shape
across every URI of a disconnecting target: a synchronous `retain` removes
the target (dropping entries it empties) while collecting URIs, then the
upstream unsubscribes are awaited afterward (failures only logged).

**Registry location and the second mutation source**: the registry is NOT
declared in subscriptions.rs — `subscriptions.rs` is a seam-module impl
split (PR #65); the field lives on the router struct at
`plug-core/src/proxy/mod.rs:174`
(`resource_subscriptions: DashMap<String, HashSet<NotificationTarget>>`,
initialized at `:387`). The catalog-refresh block in `refresh_tools`
(`mod.rs:1439-1620`) mutates it directly and makes its own upstream
`peer().unsubscribe/subscribe` calls (race 4 above). Its DECISION logic —
prune URIs gone from the route cache; rebind URIs whose owning server
changed, skipping the rebind if the old owner's unsubscribe fails (to avoid
dual subscription), pruning local subscribers if the new owner is missing
or lacks subscribe support — is correct and stays; its EXECUTION is what
must route through the coordinator.

**Task-tracking infrastructure already exists**: the Engine owns a
`tokio_util::task::TaskTracker` (`plug-core/src/engine.rs:121`, exposed via
`Engine::tracker()` at `:299`) used by health/watcher tasks for ordered
shutdown; the router holds a weak Engine ref (`mod.rs:171`, set via
`set_engine`). Detached transition tasks spawn on THAT tracker.

Facts to confirm on read: how plug's three transports call into this (grep
`subscribe_resource\|unsubscribe_resource\|cleanup_subscriptions_for_target`
in `plug-core/src` and `plug/src` — the fix must not change the public
method signatures unless unavoidable), and re-locate the prune/rebind block
by its log strings ("pruning stale resource subscription", "rebinding
resource subscription") if line numbers drifted.

## Fix design (already decided)

Introduce a **per-URI async state machine** instead of widening the sync
lock. Each entry gets a state:

```rust
enum UpstreamSubState {
    /// Upstream subscribe in flight; waiters get the shared result.
    Pending(tokio::sync::watch::Receiver<Option<Result<(), SharedErr>>>),
    /// Upstream confirmed.
    Active,
}
```

- **Subscribe**: under the sync guard, insert the client AND read the state.
  - No entry → create with `Pending`, hold the `watch::Sender`, drop guard,
    perform the upstream call, then under the guard again: on Ok set
    `Active` and broadcast Ok; on Err remove ONLY IF the client set matches
    the failure cohort (see below) and broadcast Err.
  - `Pending` → drop guard, await the watch until the result is Some;
    propagate Ok/Err to THIS caller. On Err, remove self from the entry
    (under guard) — every waiter gets the real outcome instead of a silent
    zombie.
  - `Active` → done (true piggy-back on a confirmed subscription).
- **Failure cohort**: on upstream Err, under the guard, remove the entry
  entirely — every client currently in it received (or will receive via the
  watch) the Err. Clients can only be in the entry if they arrived before the
  broadcast; the watch guarantees they see the Err. This makes the rollback
  correct rather than silent.
- **Unsubscribe**: under the guard, remove the client; if now empty,
  transition the entry to a `Draining` marker (a third state) so racing
  subscribers replace the slot with a FRESH `Pending` entry under a new
  generation instead of joining the dying one. Then drop the guard, acquire
  the URI's transition lock (below), await upstream unsubscribe, and under
  the guard remove the entry ONLY IF it is still the same generation
  (`Draining` with matching generation counter — a simple `u64` bumped on
  each entry creation), then release the transition lock.
- **Per-URI upstream ordering (REQUIRED — this is the load-bearing piece)**:
  the generation machinery keeps the REGISTRY consistent under any
  interleaving, but it cannot order the UPSTREAM calls: without ordering, a
  drain's `unsubscribe` and a fresh subscriber's `subscribe` race at the
  upstream, and if the subscribe completes first and the unsubscribe last,
  the upstream ends unsubscribed while the registry says `Active` — the
  exact zombie this plan exists to kill. Subscribe/unsubscribe do NOT
  commute, and subscribe's idempotence does not help (it protects
  sub-after-sub, not sub-before-unsub). Therefore: keep a persistent
  `DashMap<String, Arc<tokio::Mutex<()>>>` of per-URI **transition locks**;
  EVERY upstream `subscribe`/`unsubscribe` call for a URI is awaited while
  holding that URI's transition lock (piggy-backers issue no upstream call
  and never touch it). A subscriber arriving during `Draining` thus installs
  its fresh `Pending` entry immediately (registry-side) but its upstream
  subscribe WAITS behind the in-flight unsubscribe — completion order is
  forced to unsubscribe-then-subscribe, and the final upstream state is
  subscribed. **Lock-order rule** (put it in the module doc): the async
  transition lock MAY be held across sync-guard acquisitions and upstream
  awaits; the sync registry guard MUST NOT be held across ANY await,
  including acquiring the transition lock. After acquiring the transition
  lock, re-check the entry's state/generation under the sync guard before
  issuing the upstream call (it may have been resolved or drained while you
  waited).
- **Cancellation-safety: transitions are owned by detached tasks, never by
  request futures.** If the requester's future owned the transition-lock
  guard, dropping that future (client disconnect, request cancellation)
  would RELEASE the lock while the already-sent upstream request can still
  complete remotely — regardless of whether the runtime emits
  `notifications/cancelled`, cancellation is advisory and races completion.
  A cancelled drain's unsubscribe could then land AFTER a new subscriber's
  subscribe, reopening the exact ordering race the lock exists to close
  (subscribe idempotence covers a cancelled subscribe; nothing covers a
  late unsubscribe). Therefore each upstream transition (first-subscribe or
  drain-unsubscribe) runs in a detached task that owns the
  transition-lock guard, the upstream call, AND the post-call registry
  bookkeeping (set `Active` + broadcast Ok; or Err broadcast + rollback; or
  generation-matched drain removal). The task runs to completion regardless
  of the requester's fate; requesters and piggy-backers only await the
  outcome via the `watch` channel, which is always safe to abandon.
  Consequences to encode: a cancelled first-subscriber's cohort receives
  the REAL outcome (the transition completes; waiters get Ok/Err, never a
  synthetic cancellation error); a cancelled requester whose target stays
  in an entry that goes `Active` simply receives notifications until it
  unsubscribes or its disconnect cleanup runs (protocol-tolerable — note in
  the module doc). Waiters still map a closed `watch` channel to Err — that
  now only fires if the transition task itself dies (panic), and the next
  subscriber's re-check under the sync guard replaces the dead generation.
  Spawn transition tasks on the Engine's EXISTING `TaskTracker`
  (`Engine::tracker()`, engine.rs:299 — upgrade the router's weak engine
  ref), NOT raw `tokio::spawn`: the tracker gives ordered shutdown
  (`TaskTracker::wait()`) so this plan does not create another instance of
  the untracked-spawn lifecycle problem plan 012 exists to fix. If the weak
  ref cannot upgrade (engine teardown in progress), fall back to
  `tokio::spawn` with a comment — the task is bounded by the upstream call
  timeout and everything is dying anyway. Do not build NEW tracking
  machinery; the tracker already exists.

- **Cleanup** (`cleanup_subscriptions_for_target`): per URI, apply the SAME
  drain protocol as unsubscribe (remove target; if now empty → `Draining` +
  generation; await upstream unsubscribe; generation-matched removal). Do
  not keep the current retain-then-await shape — it is race (3).
- **Read path**: `route_upstream_resource_updated` keeps delivering to the
  entry's current target set regardless of state (behavior-preserving); it
  adapts only to the new entry struct's field layout.
- **One coordinator, shareable state**: extract the subscription state into
  a single `SubscriptionRegistry` struct (entries map + transition-lock map
  + generation counter), held by the router as `Arc<SubscriptionRegistry>`
  (field change at `mod.rs:174`) so detached transition tasks can clone an
  owned handle — a plain struct field cannot outlive `&self` into a spawned
  task. The entry/state types stay private to the registry module; the
  router exposes the same public methods as today, now delegating.
- **Catalog prune/rebind route through the coordinator**: the
  `refresh_tools` block keeps its decision logic and failure policy
  VERBATIM (prune vanished URIs; rebind changed-owner URIs; skip rebind and
  prune local subscribers when the old-owner unsubscribe fails, the new
  owner is missing, or the new owner lacks subscribe support) but stops
  touching the map and the upstream peers directly: per URI, prune = the
  drain transition; rebind = a compound transition executed under that
  URI's single transition lock (unsubscribe old owner → subscribe new
  owner, preserving the target set, bumping the generation). Per-URI locks
  order rebinds against concurrent downstream transitions on the same URI
  — which server a call goes to is irrelevant to the lock key. Rebinds of
  DIFFERENT URIs may run concurrently, matching today's behavior
  (sequential loop) or better; do not add cross-URI ordering.

Public method signatures stay the same. All awaits happen with the sync
guard dropped. Two new synchronization primitives, each with one job: the
`watch` channel broadcasts a generation's outcome to its cohort; the per-URI
transition lock serializes upstream calls — for EVERY mutation source
(downstream subscribe/unsubscribe, disconnect cleanup, catalog
prune/rebind). After this plan, no MCP subscribe/unsubscribe call exists
outside the registry module.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Targeted tests | `cargo test -p plug-core subscriptions` | all pass |
| Full tests | `cargo test --workspace` | all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `plug-core/src/proxy/subscriptions.rs` — the state machine; all three
  write paths (`subscribe_resource`, `unsubscribe_resource`,
  `cleanup_subscriptions_for_target`); the read path
  (`route_upstream_resource_updated`) adapted to the new entry shape with no
  behavior change; tests.
- `plug-core/src/proxy/mod.rs` — ONLY two things: the registry field
  becomes `Arc<SubscriptionRegistry>` (declaration `:174`, init `:387`),
  and the `refresh_tools` prune/rebind block (`:1439-1620`) is rewritten to
  call the coordinator (decision logic and log/warn messages preserved).
- Existing proxy tests that exercise prune/rebind behavior (find them:
  `grep -rn 'rebind\|prune' plug-core/src plug-core/tests --include='*.rs' -l`)
  — update their plumbing to the new registry handle; assertions unchanged.

**Out of scope** (do NOT touch):
- Callers in the three transports — signatures are preserved.
- The proxy-side replay set (plan 007) — different layer (per-client vs
  per-upstream).
- Notification routing for `resources/updated` — separate path.
- The prune/rebind DECISION logic and semantics (todo 039 territory —
  complete on main): what gets pruned/rebound, the dual-subscription
  avoidance policy, and the log messages all stay exactly as they are. Only
  the execution path (registry mutation + upstream calls) moves into the
  coordinator.
- The rest of `refresh_tools` — snapshot building, routing tables, events —
  untouched (plan 004 owns perf changes there; see Depends on).

## Git workflow

- Branch: `fix/subscription-registry-atomicity`
- Commit: `fix(subscriptions): make per-URI subscribe/unsubscribe atomic across the upstream call`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Read the whole file and inventory ALL mutation sources

Read `plug-core/src/proxy/subscriptions.rs` fully AND the
`refresh_tools` prune/rebind block (`proxy/mod.rs:1439-1620` — re-locate by
log strings if drifted). Then
`grep -rn 'subscribe(' plug-core/src plug/src | grep -v test` to list every
caller, and
`grep -n 'resource_subscriptions' plug-core/src/proxy/mod.rs` to list every
direct registry touch outside subscriptions.rs. Confirm signature
preservation is feasible and locate `Engine::tracker()` +
the router's weak engine ref.

**Verify**: caller list AND direct-mutation list written into the
completion notes; signatures confirmed stable.

### Step 2: Implement the state machine

Per the design. Key invariants to uphold (put them in a module-level comment):

- No `.await` while holding the registry guard.
- A client is in an entry ⇔ it has received or will receive that entry
  generation's definitive outcome (Ok via Active/watch, Err via watch).
- Entry removal only under the guard, and (for drain) only generation-matched.
- Upstream transitions run in detached tasks that hold the URI's transition
  lock start-to-finish; the lock is never owned by a cancellable request
  future.
- Every upstream subscribe/unsubscribe in the crate goes through this
  registry — downstream, cleanup, AND catalog prune/rebind.

This step includes the extraction: registry state into
`Arc<SubscriptionRegistry>` (mod.rs field change) and the prune/rebind
block rewritten onto coordinator calls (decision logic verbatim — diff the
block's warn/debug messages before/after: identical).

**Verify**: `cargo check --workspace` → exit 0.

### Step 3: Tests

In the file's `#[cfg(test)]` module, with a mock `UpstreamHandle` whose
subscribe/unsubscribe complete on test-controlled signals (oneshot/Notify)
AND which models upstream state: a per-URI `subscribed: bool` flipped at
each call's COMPLETION, plus a recorded completion sequence — so tests can
assert both ordering and the final upstream state, not merely that calls
were issued. Pattern-match how existing tests in plug-core mock upstream
handles (`grep -rn 'mock\|Mock' plug-core/src --include='*.rs' -l` and read
the closest exemplar):

1. `piggy_backer_during_failed_first_subscribe_gets_error` — client A
   subscribes (upstream held pending), client B subscribes same URI, upstream
   fails; assert BOTH A and B get Err and the entry is gone.
2. `piggy_backer_during_successful_first_subscribe_gets_ok` — same, upstream
   succeeds; both Ok; entry Active with 2 clients.
3. `subscribe_during_drain_waits_for_unsubscribe` — A subscribes (Active),
   A unsubscribes (upstream unsubscribe held pending), B subscribes; assert
   B's upstream subscribe has NOT completed while the unsubscribe is held
   (the transition lock forces it to wait); release the unsubscribe; assert
   the mock's completion sequence is unsubscribe-then-subscribe, the final
   mock upstream state for the URI is SUBSCRIBED, and B's entry is `Active`
   under a fresh generation. (This test must fail against a design that lets
   the two upstream calls race — that is the point.)
4. `unsubscribe_last_client_calls_upstream_once` — plain path sanity.
5. `cleanup_during_subscribe_uses_drain_generation` — A subscribed (Active);
   A's target disconnects → `cleanup_subscriptions_for_target` runs with the
   upstream unsubscribe held pending; B subscribes the same URI; release the
   unsubscribe; assert completion order unsubscribe-then-subscribe, final
   mock upstream state SUBSCRIBED, and B's fresh-generation entry `Active`.
6. `first_subscriber_cancelled_transition_still_completes` — A subscribes
   (upstream subscribe held pending), B piggy-backs on the `Pending` entry,
   A's REQUEST future is dropped; assert the detached transition keeps
   running: release the upstream Ok and B receives Ok with the entry
   `Active` (B is never exposed to A's cancellation).
7. `last_unsubscriber_cancelled_mid_flight_then_new_subscriber` — A is the
   last subscriber and unsubscribes (upstream unsubscribe held pending);
   A's REQUEST future is dropped while the detached drain transition holds
   the URI's transition lock; B subscribes — assert B's upstream subscribe
   does NOT complete while the drain is pending; release the unsubscribe;
   assert completion order unsubscribe-then-subscribe, final mock upstream
   state SUBSCRIBED, and B `Active` under a fresh generation. (This is the
   regression for cancellation reopening the ordering race.)
8. `rebind_serializes_against_downstream_transitions` — A subscribed to a
   URI owned by S1 (Active); a catalog refresh rebinds the URI to S2 with
   the upstream calls held pending, while B concurrently
   subscribes/unsubscribes the same URI; assert the mock's completion
   sequence for that URI has NO overlapping calls (strict serialization),
   the final upstream state is subscribed on exactly the new owner S2, and
   the registry's target set matches the downstream operations that were
   applied. (Regression for race 4: rebind vs downstream transition.)
9. Concurrency smoke: 20 tasks × subscribe/unsubscribe same URI with a mock
   that yields; assert final registry state is consistent with the mock's
   net call sequence (no panics, no orphaned entries).

**Verify**: `cargo test -p plug-core subscriptions` → all pass; `cargo test --workspace` → all pass.

## Test plan

Covered by step 3. Tests 1 and 3 are the two races from "Why this matters" —
they MUST fail against the old code. Do NOT use `git stash` for this
negative check: the worktree is shared with another concurrently-editing
agent, and a stash can capture and then misapply someone else's changes.
Instead, write the race tests FIRST, before touching the implementation,
and record their failure mode (expect failure or hang — guard with a
timeout, e.g. `timeout 60 cargo test -p plug-core subscriptions -- piggy_backer_during_failed`);
then implement and watch them pass. If the implementation already exists by
the time you get here, run the negative check in a disposable worktree
pinned to the pre-fix revision instead
(`git worktree add /tmp/plug-negcheck e341625`, apply ONLY the test diff
there, run, then `git worktree remove --force /tmp/plug-negcheck`). State
the result in the completion report.

## Done criteria

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] Tests 1 and 3 demonstrated failing against the pre-fix code (negative check)
- [ ] No public signature changes (transport callers untouched; `git status`
      shows only `subscriptions.rs`, `proxy/mod.rs`, and updated proxy tests)
- [ ] No MCP subscribe/unsubscribe outside the registry module:
      `grep -n 'SubscribeRequestParams\|make_unsubscribe\|UnsubscribeRequestParams' plug-core/src/proxy/mod.rs`
      → no hits (the tokio broadcast-channel `.subscribe()` calls at
      `mod.rs:402`/`:412` are unrelated and stay)
- [ ] Transition tasks spawn via `Engine::tracker()` — `grep -n 'tokio::spawn' plug-core/src/proxy/subscriptions.rs`
      shows only the documented teardown fallback
- [ ] Prune/rebind warn/debug messages byte-identical pre/post (behavior-preservation spot-check)
- [ ] `plans/README-claude-fable.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Callers pass data that makes signature preservation impossible (step 1) —
  report the minimal signature change needed.
- The `Arc<SubscriptionRegistry>` extraction forces changes to the router's
  PUBLIC API (beyond the private field and delegating methods) — report.
- The rebind compound transition cannot preserve the existing
  dual-subscription-avoidance policy under a single per-URI lock (e.g. the
  policy turns out to depend on cross-URI ordering) — report; do not weaken
  the policy.
- Step 1 finds ADDITIONAL direct registry mutations or upstream
  subscribe/unsubscribe call sites beyond subscriptions.rs and the
  `refresh_tools` block — report the full list before proceeding (the
  coordinator invariant is only worth building if it covers every source).
- The 20-task smoke test deadlocks — likely a lock-order violation (holding
  the sync guard while acquiring the transition lock, or two transition
  locks at once); fix that; if it persists, report with the stack.

## Maintenance notes

- The generation counter + watch pattern + per-URI transition lock are now
  the file's core invariants — future features (e.g. subscription refresh on
  server reconnect) must go through the same state machine and issue
  upstream calls only under the URI's transition lock, not add side-channels.
- The transition-lock map grows by one small entry per distinct URI ever
  subscribed and is never pruned — bounded by subscribed-URI cardinality,
  acceptable for this tool; if that ever matters, prune entries whose lock
  is uncontended at drain time (do NOT do this speculatively now).
- Plan 007's proxy-side replay re-issues `resources/subscribe` after daemon
  reconnect — those arrive here as ordinary subscribes and are handled by the
  same machine (piggy-back or fresh) with no special casing. Nothing to
  coordinate beyond normal review.
- Review focus: every `.await` in the two methods happens guard-free; error
  broadcast reaches ALL cohort members exactly once.
