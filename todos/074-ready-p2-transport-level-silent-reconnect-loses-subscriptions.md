---
status: ready
priority: p2
issue_id: "074"
tags: [subscriptions, resources, transport, sse, http, reconnect, correctness]
dependencies: ["073"]
---

# A transport-level silent reconnect loses resource subscriptions invisibly

## Problem Statement

`todos/073` closed the case where an upstream reconnect goes through
`Engine::reconnect_server`: `UpstreamServer.generation` moves, the subscription registry
notices, and `resources/subscribe` is re-issued. Both upstream transports can also reconnect
*below* that layer, inside the transport worker, without ever rebuilding `UpstreamServer`. The
generation does not move, so none of the 073 machinery fires, and the registry keeps reporting
the URI as `Active` on a connection that may no longer hold the subscription.

The downstream symptom is the same one 073 described: clients keep their subscriptions and
simply stop receiving `resources/updated`, with no error and no signal.

## Findings

Traced on `main` on 2026-08-08.

### Legacy SSE: the retry loop never re-initializes, and nothing above it is told

`initialize` is sent exactly once, in the worker's `run()`
(`plug-core/src/transport/sse_client.rs:182-193`, response awaited at `:198-220`), before the
steady-state loop at `:225-266`. The retry lives in a different task: `run_sse_loop` is spawned
at `:166-173` and owns only the GET stream. On stream error (`:372-381`) or clean EOF
(`:382-391`) it sleeps and re-enters `open_sse_stream` (`:307-312`, `:422-436`) carrying
`last-event-id`. Its only effect visible outside that task is `endpoint_tx.send_replace(...)` at
`:346`; the worker's request loop then silently starts POSTing to the new endpoint (`:232-241`).

Retry is effectively unbounded — `LegacySseTransportConfig::with_uri` sets
`retry_policy.max_times = None` (`:75-77`) — so the arms that would return `TransportClosed`
(`:384`) or a fatal error (`:321`, `:374`) are unreachable via exhaustion. The worker never
exits, rmcp never tears down the `RunningService`, and `UpstreamServer` is never rebuilt.

Whether the *server* still honors an earlier `resources/subscribe` after it re-advertises a new
POST endpoint is a property of the remote server and is **not determinable from this repo**.
`last-event-id` is sent, but nothing verifies the server resumed rather than minted a new
session.

### Streamable HTTP: the 404 path is a genuinely new MCP session

plug builds the transport with `StreamableHttpClientTransportConfig::with_uri(url)`
(`plug-core/src/server/mod.rs:1304`) and overrides neither `retry_config` nor
`reinit_on_expired_session`; rmcp 3.1.0 defaults the latter to `true`.

Two distinct behaviors, both below `UpstreamServer`:

- **GET-stream resume** — `spawn_common_stream` / `retry_connection` redial with the *same*
  `Mcp-Session-Id` plus `last-event-id`. Session state is preserved by construction. Benign.
- **Session-expired re-init** — on HTTP 404, `perform_reinitialization` sends a fresh
  `initialize` + `notifications/initialized`, obtains a **new `Mcp-Session-Id`**, respawns the
  streams, and replays the failed request, all inside the transport worker.

The second case is strictly worse in kind than the bug 073 fixed: it is a new upstream MCP
session on which no prior `resources/subscribe` can exist, yet the engine sees an identical
`generation` and the registry entry stays `Active`. Whether a given upstream actually emits the
404 is server-dependent and not determinable from the code.

### Only one indirect signal exists, and it is incidental

During an SSE reconnect gap, `endpoint_rx.borrow()` (`sse_client.rs:232`) still yields the old
endpoint until `:346` fires, so a request posted in that window can fail with
`UnexpectedStatus` and trip reactive recovery at `plug-core/src/proxy/mod.rs:3085-3109` — which
*would* rebuild `UpstreamServer` and let 073's fix run. A reconnect with no concurrent request
in flight produces no engine-visible signal at all. This is incidental, not a mechanism to rely
on.

### No test coverage

`sse_client.rs`'s `mod tests` (`:533+`) covers endpoint resolution, redaction, and
pre-initialize buffering only. The retry loop is untested.

## Proposed Solutions

### Option A (recommended): surface a transport-level session-identity change to the engine

Have each upstream transport publish a monotonic session/incarnation counter that the engine
folds into `UpstreamServer.generation` — or expose it as a second field the subscription
registry also compares. The 073 machinery (`owner_connection_generation`,
`RouteReconciliation::Resubscribe`) then covers both layers with no further change.

**Pros:** reuses everything 073 built; the registry stays the single place that decides
**Cons:** needs a signal out of rmcp's streamable-HTTP worker, which may not exist today without
an upstream change or a wrapper transport
**Effort:** medium-large **Risk:** medium

### Option B: verify subscriptions lazily against the upstream

Periodically, or on a `resources/updated` gap, re-issue `resources/subscribe` for `Active`
entries. Idempotent on a server that still holds the subscription.

**Pros:** no transport change; covers reconnects the engine can never see
**Cons:** unbounded latency before recovery; needs its own scheduling; relies on subscribe being
idempotent upstream, which the spec does not guarantee
**Effort:** medium **Risk:** low-medium

### Option C: disable `reinit_on_expired_session` and force reconnects up to the engine

Set `reinit_on_expired_session: false` so a 404 fails the request instead, letting reactive
recovery rebuild `UpstreamServer` and trigger 073's path.

**Pros:** small, and turns an invisible failure into one the existing machinery already handles
**Cons:** covers only the HTTP 404 case, not SSE; converts a transparently-recovered request into
a user-visible failure, so it trades one regression for another
**Effort:** small **Risk:** medium

## Acceptance Criteria

- [ ] A transport-level reconnect that establishes a new upstream MCP session re-issues
      `resources/subscribe` for entries plug still believes are active
- [ ] The legacy SSE retry loop has test coverage for at least the reconnect-and-resume path
- [ ] The streamable-HTTP `reinit_on_expired_session` behavior is either observed by the engine
      or deliberately disabled, with the choice written down

## Resources

- `plug-core/src/transport/sse_client.rs:75-77, 166-173, 182-220, 306-347, 372-391, 422-436`
- `plug-core/src/server/mod.rs:602-605, 1304, 1568-1580`
- `plug-core/src/proxy/subscriptions.rs` (the 073 machinery this would reuse)
- `todos/073-done-p2-upstream-reconnect-loses-resource-subscriptions.md`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Split out of `todos/073`, whose acceptance criteria asked for the SSE silent-reconnect case to be
traced and either covered or ruled out. It is now traced and is neither — it is a distinct defect
with a distinct cause, and the streamable-HTTP re-initialization path turned out to be the more
serious half. No code change made.
