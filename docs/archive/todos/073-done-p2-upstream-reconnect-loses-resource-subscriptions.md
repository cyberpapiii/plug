---
status: done
priority: p2
issue_id: "073"
tags: [subscriptions, resources, upstream, reconnect, correctness]
dependencies: []
---

# A same-id upstream reconnect silently loses its resource subscriptions

## Problem Statement

When an upstream MCP server drops and plug reconnects to it under the same server id, the
new connection has no `resources/subscribe` registered, but the subscription registry still
believes the entry is `Active` and owned by that server. Downstream clients keep their
subscriptions and simply stop receiving `resources/updated` notifications, with no error and
no signal that anything was lost.

This was recorded as an unverified observation in `todos/070` and has now been confirmed
end to end.

## Findings

Verified on `main` on 2026-08-08.

### The reconnect path rebuilds the connection and nothing else

Both entry points converge on the same tail:

- health-monitor path: `plug-core/src/health.rs:152-165` → `:177` → `:199` → `:230` →
  `engine.reconnect_server` at `:233`
- reactive/transport-error path: `plug-core/src/proxy/mod.rs:1737` (`reconnect_server_now`)
  and `:1750` (`reconnect_server_in_background`); OAuth refresh at
  `plug-core/src/engine.rs:1019`

Tail: `engine.rs:654` → `:679` (`do_reconnect`) → `:715-745` dials a brand-new
`UpstreamServer` with no subscription state passed in → `:748-752` → `commit_replacement`
(`:533`) → `plug-core/src/server/mod.rs:2077` (`replace_server`), which swaps the handle,
resets the circuit breaker and health state, and retires the old handle. No subscription
replay anywhere in `server/mod.rs:2077-2110`.

### The route-diff pass cannot see it

`engine.rs:756` then calls `refresh_tools` (`plug-core/src/proxy/mod.rs:1774`), which
classifies at `:2189-2194` via `classify_route_changes`
(`plug-core/src/proxy/subscriptions.rs:861-913`). That emits `Rebind` only when the server
id changes (`:874-891`) and `Prune` only when the URI has no route at all (`:896-911`). A
same-id reconnect with the same URI set produces no reconciliation item. The post-publish
sweep (`proxy/mod.rs:2283`, body `:2297-2313`) re-runs the same classification and is
equally inert.

Grepping `plug-core/src` for `resubscribe`, `subscribe_resource`, and `resources/subscribe`
turns up only the trait impl at `subscriptions.rs:174-185`, reached from `subscribe()`
(`:452`) and `rebind()` (`:975`) — neither runs on a same-id reconnect.

### The registry has no way to notice

`Entry.owner_server_id` (`subscriptions.rs:95-102`) is a plain server **id**. There is no
connection instance id, epoch, or upstream generation recorded anywhere, so "same id, new
connection" is indistinguishable from "same connection" to every consumer of that field.

A later downstream re-subscribe cannot heal it either: `subscribe()` classifies an existing
`Active` entry as `Action::AlreadyActive` (`:506,513`) and returns `Ok(())` with no upstream
call. That includes the downstream reconnect replay path
`IpcRequest::RestoreResourceSubscriptions` (`plug-core/src/ipc.rs:145-146`, handled at
`plug/src/daemon/mod.rs:1493`), which replays *downstream client* subscriptions and
short-circuits on `AlreadyActive`.

### The last-known-good catalog carry-forward makes it more likely, not less

`plug-core/src/server/mod.rs:702-704, 727-733`, and the comment on `replace_server` at
`:2070-2076` ("carrying its catalog forward keeps subscriptions and routes stable"),
deliberately keep the URI set identical across a degraded or reconnecting server. That is
the right call for avoiding spurious prunes, but it also guarantees the route diff is empty
and therefore guarantees the stale `Active` entry is never touched.

### Partial mitigation

If the reconnected server's fresh `resources/list` no longer contains the URI, the prune
path (`proxy/mod.rs:2201-2224`) drains the entry. That removes the stale state at the cost
of dropping the downstream subscriber — cleanup, not re-establishment.

### Not traced

- `plug-core/src/transport/sse_client.rs:56, 75-83, 304-386` retries the SSE stream
  indefinitely below the engine. If the SSE layer reconnects without the engine ever calling
  `reconnect_server`, even the (no-op) `refresh_tools` pass never runs. Whether that
  reconnect re-establishes MCP session state at the rmcp layer was not traced.
- Whether upstream streamable-HTTP session resumption (`Mcp-Session-Id`) preserves upstream
  subscription state for some transports was not traced.

## Proposed Solutions

### Option A (recommended): record a connection epoch and replay on change

Give each upstream connection an instance id or monotonic epoch, stamped onto the entry
alongside `owner_server_id` when a subscribe confirms. `replace_server` bumps the epoch;
`refresh_tools` (or a dedicated post-reconnect pass) then treats "same server id, different
epoch" exactly like a rebind and re-issues `resources/subscribe`.

**Pros:** fixes the cause; reuses the rebind machinery, which already serializes correctly
against drains and downstream transitions
**Cons:** touches the reconnect commit path and adds a field to `Entry`
**Effort:** medium **Risk:** medium — concurrent path

### Option B: replay from the registry inside `replace_server`

Have the reconnect commit ask the registry for every `Active` entry owned by that server id
and re-subscribe each on the new handle.

**Pros:** localized to the reconnect path
**Cons:** runs outside the per-URI transition locks unless carefully routed through the
registry, so it can race a concurrent drain or rebind
**Effort:** small-medium **Risk:** medium-high

### Option C: verify lazily

On the next `resources/updated` gap or on a timer, re-issue subscribes for entries whose
owner connection is newer than the entry.

**Pros:** no reconnect-path change
**Cons:** unbounded latency before recovery; needs its own scheduling machinery
**Effort:** medium **Risk:** low

## Acceptance Criteria

- [x] A same-id upstream reconnect re-issues `resources/subscribe` for entries it still owns
- [x] Regression test driving a reconnect and asserting the upstream sees a fresh subscribe
- [x] The SSE-layer silent-reconnect case is traced — it is **not** covered by this fix, and is
      tracked separately as `todos/074`
- [x] `AlreadyActive` no longer masks a connection that has forgotten the subscription

## Resources

- `plug-core/src/engine.rs:654-760`, `plug-core/src/server/mod.rs:2070-2110`
- `plug-core/src/proxy/subscriptions.rs`, `plug-core/src/proxy/mod.rs:1774-2313`
- `todos/070-done-p2-subscription-cross-owner-supersede-leak.md`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Confirmed the observation carried forward from `todos/070` by tracing the reconnect path end
to end. No code change made.

### 2026-08-08 - Resolved (engine-level reconnects)

**By:** Claude Fable 5

Took Option A. The connection epoch it proposed already existed: `UpstreamServer.generation`
(`plug-core/src/server/mod.rs:602-605`), assigned by `next_upstream_generation()` and already
covered by `reconnect_publishes_a_new_monotonic_connection_generation`. The work was plumbing it
into the subscription registry, not inventing it.

**What changed.**

- `UpstreamResourceOps` gained `connection_generation()`; the production impl returns
  `UpstreamServer.generation`.
- `Entry` gained `owner_connection_generation`, stamped from the handle that actually confirmed
  the subscription — in both `run_subscribe_transition` and the rebind transition — and cleared
  whenever the owner is.
- `classify_route_changes` emits a new `RouteReconciliation::Resubscribe` when the URI still
  routes to the same server but that server's current connection generation differs from the one
  recorded. Only settled `Active` entries with a confirmed owner qualify; `Pending` and `Draining`
  entries have their own transition in flight that owns the outcome.
- `SubscriptionRegistry::resubscribe` executes it through the same generation-and-lock machinery
  as `rebind` (both now call a shared `migrate`), with one deliberate difference: it does not
  release the old owner. That is the subtle part. A same-id rebind would resolve "the old owner"
  to the *replacement* connection and send it `resources/unsubscribe` for a subscription it never
  had; a failure there sets `failed = true`, which prunes the entry's local subscribers. The
  reconnect fix would then cause precisely the loss it exists to prevent.
- `subscribe()` no longer answers `AlreadyActive` when the recorded connection generation differs
  from the live handle's. It starts a fresh transition over the existing member set instead, so a
  downstream re-subscribe can heal a reconnect on its own. Members are carried through, and no
  `displaced_owner` is set — there is nothing to release.

Both detection paths were verified to be load-bearing: with `owner_connection_is_stale` forced to
`false` and the `subscribe()` check disabled, `classify_resubscribes_after_a_same_id_reconnect` and
`subscribe_after_reconnect_does_not_short_circuit_on_active` both fail.

**Scope — read this before assuming the class is closed.** This covers reconnects that go through
`Engine::reconnect_server` (health monitor, reactive transport-error recovery, OAuth refresh), which
is what the original trace found. It does **not** cover a reconnect that happens *below*
`UpstreamServer` inside the transport, because `generation` does not move there. Traced on 2026-08-08:
the legacy SSE client's retry loop (`plug-core/src/transport/sse_client.rs:306-347`) reopens the
stream and adopts a new POST endpoint in a task that never re-runs the `initialize` at `:182-220`,
and rmcp's streamable-HTTP client performs a full re-`initialize` with a brand-new `Mcp-Session-Id`
on an HTTP 404 (`reinit_on_expired_session` defaults to true and plug never overrides it). Neither
rebuilds `UpstreamServer`, so the registry sees an unchanged generation and stays `Active`. The HTTP
404 case is strictly worse in kind than the bug fixed here: it is a genuinely new upstream MCP
session on which no earlier `resources/subscribe` can possibly exist. Tracked as `todos/074`.
