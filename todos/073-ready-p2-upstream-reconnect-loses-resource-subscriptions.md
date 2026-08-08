---
status: ready
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

- [ ] A same-id upstream reconnect re-issues `resources/subscribe` for entries it still owns
- [ ] Regression test driving a reconnect and asserting the upstream sees a fresh subscribe
- [ ] The SSE-layer silent-reconnect case is traced and either covered or ruled out
- [ ] `AlreadyActive` no longer masks a connection that has forgotten the subscription

## Resources

- `plug-core/src/engine.rs:654-760`, `plug-core/src/server/mod.rs:2070-2110`
- `plug-core/src/proxy/subscriptions.rs`, `plug-core/src/proxy/mod.rs:1774-2313`
- `todos/070-done-p2-subscription-cross-owner-supersede-leak.md`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Confirmed the observation carried forward from `todos/070` by tracing the reconnect path end
to end. No code change made.
