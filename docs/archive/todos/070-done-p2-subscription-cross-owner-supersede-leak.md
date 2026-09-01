---
status: done
priority: p2
issue_id: "070"
tags: [subscriptions, resources, upstream, leak, correctness]
dependencies: []
---

# Cross-owner supersede leaks an upstream resource subscription

## Problem Statement

When a resource-subscription entry is draining and a new subscriber arrives in the
drain window, the registry replaces the entry and discards the recorded owner. If the
new subscriber resolves to a *different* upstream server, the original upstream keeps a
live `resources/subscribe` that nothing can ever release.

This residual was recorded during the 2026-07 improve program as "pre-existing" and
carried forward unrepaired through three Codex counter-review waves. It was never
tracked in `todos/` or `docs/PROJECT-STATE-SNAPSHOT.md` — the only records on `main` are
`docs/RELEASE-NOTES-2026-07-12-codex-5.6-sol.md:124` and two `plans/` reports. This todo
gives it a home.

## Terminology

In `plug-core/src/proxy/subscriptions.rs`, **"owner" means the upstream MCP server**
(`owner_server_id`, `OwnerResolver`), not a downstream client. Downstream clients are
**members** and are properly refcounted. The name "cross-owner supersede" refers to two
different *upstreams*, which is why the earlier shorthand reading — one downstream client
clobbering another — is wrong. Downstream-client sequences were audited and are safe.

## Findings

Verified on `main` @ `e3b562e` on 2026-08-08.

### Residual 1 — upstream subscription leak (the actual "cross-owner supersede")

Sequence:

1. Entry for URI X is `Active`, `owner_server_id = S1`, generation `G`.
2. A drain starts — last member leaves (`subscriptions.rs:620-627`), disconnect cleanup
   (`:664-670`), or a refresh prune (`:847-857`). None of these bump the generation, and
   the drain task is detached (`:680-694`), so there is a window between the synchronous
   `Draining` mark and the task acquiring the per-URI transition lock (`:704-716`).
3. A new subscriber lands inside that window and takes the `Current::Draining` arm at
   **`:475-497`**, which replaces the whole `Entry` with a fresh generation and
   **`owner_server_id: None` (`:494`)**. The recorded `S1` is discarded.
4. The drain reads `still_current` (`:713-716`), sees the generation mismatch, and makes
   no upstream call at all (`:753-756`).
5. If the new subscriber resolved a different server `S2`, `S1` retains a live
   `resources/subscribe` with no handle left to release it.

Same-owner variant is benign — one redundant `subscribe`. Every existing test uses the
same owner on both sides (`subscriptions.rs:2211-2242`, `proxy/tests.rs:3717-3785`); the
different-owner variant has **no coverage**.

### Residual 2 — different-destination rebind reports `Ok` to its displaced waiter

`rebind` piggybacks only when the in-flight `Pending`'s `intended_owner_server_id` equals
the new destination (`:924-936`). A rebind to a different destination bumps the
generation and supersedes; the superseded transition then sends **`Ok(())` having done no
upstream work** (`run_rebind_transition:1017-1020`). Documented as remaining at
`:891-895`.

Caller-side mitigation is real, so no downstream client sees a false success:
`ToolRouter::subscribe_resource` re-verifies against the registry
(`plug-core/src/proxy/mod.rs:1197-1227`) and returns "resource subscription lost during
route reconciliation; retry subscribe" unless `member_active_on_owner` holds
(`subscriptions.rs:349-363`). The inaccurate `Ok` is internal. Contrast the *equivalent*
same-destination case, which was fixed and has a regression test
(`equivalent_concurrent_rebinds_share_authoritative_failure`, `:1986`).

### Not a defect

The member-set reset at `:485-489` looks like a cross-client clobber but is intended: the
only path that sets `Draining` on an entry with members is `prune()` (`:847-857`, called
from `mod.rs:2216-2219`), whose own drain removes the entry anyway (`:750-751`). Asserted
as intended at `subscriptions.rs:2251-2254` and `proxy/tests.rs:3774-3780`. The real gap
there is silence, not loss — entry-wide teardowns (`:580`, `:751`, `:1071`) drop every
member with no error and no cessation signal.

### Unverified observation

No re-subscribe path was found for **upstream reconnect**. `classify_route_changes`
(`:779-834`) rebinds only when the server *id* changes, so a same-id upstream that
reconnects would leave the registry believing the entry is `Active` while the upstream
has forgotten the subscription. The `server/mod.rs` reconnect path was not traced end to
end — confirm before acting on this.

## Proposed Solutions

### Option A: carry the draining owner forward (recommended)

At `:475-497`, preserve the displaced `owner_server_id` on the replacement entry (or hand
it to the superseding path) so that when the new subscriber resolves to a different
server, the old owner's `resources/unsubscribe` still fires.

**Pros:** smallest change; fixes the leak at its source
**Cons:** needs care that the release happens exactly once under the transition lock
**Effort:** small-medium **Risk:** medium — concurrent path with a detached task

### Option B: bump the generation when marking `Draining`

Make the drain window non-reentrant so a new subscriber cannot replace an entry whose
drain has not yet reached the lock.

**Pros:** removes the window rather than compensating for it
**Cons:** changes supersede semantics more broadly; likely disturbs existing tests
**Effort:** medium **Risk:** medium-high

### Option C: reconcile leaked upstream subscriptions out of band

Track upstream subscriptions per server and sweep any not backed by a registry entry.

**Pros:** also catches the unverified reconnect case
**Cons:** new machinery for a narrow race **Effort:** large **Risk:** low

## Acceptance Criteria

- [x] A different-owner supersede releases the original upstream's subscription
- [x] Regression test covering the different-owner variant specifically (none exists today)
- [x] Residual 2 either fixed or its internal `Ok` documented as intentional at the
      send site, given the caller-side re-verification already prevents client-visible harm
- [x] The upstream-reconnect observation is confirmed or ruled out

## Resources

- `plug-core/src/proxy/subscriptions.rs`
- `plug-core/src/proxy/mod.rs:1197-1227`
- `docs/RELEASE-NOTES-2026-07-12-codex-5.6-sol.md:124`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Re-verified the residual against `main` and gave it a tracked home. Corrected the
"cross-owner" terminology, which had been read as downstream-client-vs-downstream-client;
downstream client sequences (A/B subscribe, duplicate subscribe, disconnect cleanup,
successful rebind) were audited and are safe. No code change made.

### 2026-08-08 - Resolved

**By:** Claude Fable 5

Fixed with Option A, plus the two documentation criteria.

**The leak.** `EntryState::Draining` now carries `prior_owner_server_id`, populated at all
four mark sites (`unsubscribe`, `cleanup_target`, `prune`, and `rebind`'s empty-entry drain)
from a new `Entry::owner_to_release`. A subscriber that supersedes a drain inherits that
value and, in `run_subscribe_transition`, releases the displaced upstream itself before
doing anything else — the superseded drain will find its generation replaced and make no
upstream call, so the obligation has to move with the entry.

Three details worth keeping in mind when reading it:

- The release runs *before* the `still_current` check, because this transition's own
  generation may in turn have been superseded and the obligation does not transfer a second
  time.
- Same-owner supersedes deliberately skip the release. Dropping the subscription only to
  immediately re-add it on the same server would open a window where a failed resubscribe
  leaves the client worse off than the one redundant `subscribe` it costs instead.
- `owner_to_release` prefers the confirmed `owner_server_id` but falls back to a `Pending`
  entry's intended owner. A subscribe in flight when the drain was marked can land after its
  generation is replaced, subscribe successfully, and then decline to record anything.
  Releasing a server that turned out never to be subscribed is harmless — the drain path
  already treats a failed unsubscribe as best-effort — whereas skipping it leaks.

**Regression tests.** `different_owner_supersede_releases_the_displaced_upstream` and
`same_owner_supersede_does_not_release_the_upstream`. The race is deterministic rather than
timing-dependent: the Draining classification and entry replacement inside `subscribe` are
synchronous, so they complete before the detached drain task gets its first poll. Verified
the first test genuinely catches the defect by disabling the release and watching it fail
with `left: 0, right: 1`.

**Residual 2.** Documented as intentional at the send site in `run_rebind_transition`. An
`Err` there would be equally wrong — the newer transition may well succeed, so failing would
report a loss that did not happen — and `ToolRouter::subscribe_resource` already re-verifies
against the registry, so no client sees a false success. The comment also contrasts the
equivalent same-destination case, which is fixed rather than tolerated.

**Upstream reconnect: confirmed, not ruled out.** There is no re-subscribe path for a
same-id upstream reconnect. `replace_server` swaps the handle without replaying
subscriptions, and `classify_route_changes` emits nothing because the server id and URI set
are unchanged — the last-known-good catalog carry-forward actively guarantees that. The
registry keeps the entry `Active`, and a later downstream re-subscribe short-circuits on
`AlreadyActive` without any upstream call, so it cannot self-heal. Tracked separately as
`todos/073`, since fixing it needs a connection epoch on the entry rather than anything in
this todo's supersede path.
