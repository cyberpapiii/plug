---
status: done
priority: p2
issue_id: "074"
tags: [subscriptions, resources, transport, sse, http, reconnect, correctness]
dependencies: ["073"]
---

# A transport-level silent reconnect loses resource subscriptions invisibly

## Problem Statement

`todos/073` closed the case where an upstream reconnect goes through
`Engine::reconnect_server`: the connection generation moves, the subscription registry notices,
and `resources/subscribe` is re-issued. Both upstream transports can also reconnect *below* that
layer, inside the transport worker, without ever rebuilding `UpstreamServer`. The generation did
not move, so none of the 073 machinery fired, and the registry kept reporting the URI as `Active`
on a connection that no longer held the subscription.

The downstream symptom is the same one 073 described: clients keep their subscriptions and simply
stop receiving `resources/updated`, with no error and no signal.

## Findings

Traced on `main` on 2026-08-08.

### Legacy SSE: the retry loop never re-initializes, and nothing above it is told

`initialize` is sent exactly once, in the worker's `run()`, before the steady-state loop. The
retry lives in a different task: `run_sse_loop` owns only the GET stream. On stream error or clean
EOF it sleeps and re-enters `open_sse_stream` carrying `last-event-id`. Its only effect visible
outside that task was `endpoint_tx.send_replace(...)`; the worker's request loop then silently
started POSTing to the new endpoint.

Retry is effectively unbounded — `LegacySseTransportConfig::with_uri` sets
`retry_policy.max_times = None` — so the arms that would return `TransportClosed` or a fatal error
are unreachable via exhaustion. The worker never exits, rmcp never tears down the
`RunningService`, and `UpstreamServer` is never rebuilt.

Whether the *server* still honors an earlier `resources/subscribe` after a reconnect is a property
of the remote server and is not determinable from this repo. `last-event-id` is sent, but nothing
verified the server resumed rather than minted a new session.

### Streamable HTTP: the 404 path is a genuinely new MCP session

plug builds the transport with `StreamableHttpClientTransportConfig::with_uri(url)` and overrides
neither `retry_config` nor `reinit_on_expired_session`; rmcp 3.1.0 defaults the latter to `true`.

Two distinct behaviors, both below `UpstreamServer`:

- **GET-stream resume** — `spawn_common_stream` / `retry_connection` redial with the *same*
  `Mcp-Session-Id` plus `last-event-id`. Session state is preserved by construction. Benign.
- **Session-expired re-init** — on HTTP 404, `perform_reinitialization` sends a fresh `initialize`
  + `notifications/initialized`, obtains a **new `Mcp-Session-Id`**, respawns the streams, and
  replays the failed request, all inside the transport worker.

The second case is strictly worse in kind than the bug 073 fixed: it is a new upstream MCP session
on which no prior `resources/subscribe` can exist, yet the engine saw an identical generation and
the registry entry stayed `Active`.

### Only one indirect signal existed, and it was incidental

During an SSE reconnect gap, `endpoint_rx.borrow()` still yields the old endpoint until the new
one is published, so a request posted in that window can fail with `UnexpectedStatus` and trip
reactive recovery — which *would* rebuild `UpstreamServer` and let 073's fix run. A reconnect with
no concurrent request in flight produced no engine-visible signal at all.

## Resolution

Implemented Option A. Both transports now publish a session-identity change, and the 073 machinery
covers both layers.

### The observation seam

rmcp keeps the live `Mcp-Session-Id` private to its transport worker — a local variable in
`streamable_http_client.rs`, reassigned on re-initialization, with no accessor and no event. But
the `StreamableHttpClient` trait it is generic over receives `session_id` as a parameter of every
`post_message`, `get_stream`, and `delete_session` call, and plug already supplies its own
implementation of that trait (`InitializedNotificationCompatHttpClient`, which existed for
`notifications/initialized` compatibility). That wrapper is therefore the one place in the process
that can watch the session id, and no rmcp change or fork is needed.

For legacy SSE the transport is plug's own code, so the signal is direct.

### What was built

- `ConnectionGeneration` (`plug-core/src/server/mod.rs`) — the connection identity became a shared
  `Arc<AtomicU64>` that can be advanced in place, replacing the plain `u64` field on
  `UpstreamServer`. The same value is handed to the transport before the transport is built, so
  the layer that *sees* a session change can tell the layers that *own* state established on it.
  `UpstreamServer::generation()` reads it fresh; callers must not cache it.
- `InitializedNotificationCompatHttpClient::observe_session` — records the session id seen on the
  wire and reacts when it changes. Observed on `get_stream` as well as `post_message`, because
  after a re-initialization rmcp respawns the GET stream with the new id and that can reach the
  wire before any request does. A `None` id is deliberately ignored: it means either a pre-session
  call or a stateless server, and treating it as a change would fire on every request forever.
- `LegacySseTransportConfig::session_observer` + `publish_endpoint` — the SSE retry loop reports a
  replaced session when the server re-advertises a *different* POST endpoint. An unchanged
  endpoint is treated as a resume, since the redial carries `last-event-id` and a server that
  meant to start over would have to say so by moving the endpoint. This is a heuristic, and it is
  the only discriminator the legacy protocol offers.
- `on_upstream_session_replaced` — the shared reaction. Advancing the generation is only half of
  it: route reconciliation is purely event-driven, so on a quiet system a stale entry would sit
  unnoticed until some unrelated refresh happened to run. It also schedules a resource refresh,
  whose post-publish sweep classifies entries against the current snapshot and re-issues the
  subscribe. The scheduler coalesces, so a flapping upstream costs one refresh, not one per flap.

### Observability

`active_subscription_count()` existed but was not reachable from the CLI, so there was no way to
tell a client that subscribed and went quiet from one that never subscribed. `plug status` now
reports it, in both text and `--output json`, carried over IPC as
`IpcResponse::Status.resource_subscriptions` (`#[serde(default)]`, so a newer CLI still reads an
older daemon's response).

### Why not the alternatives

- **Lazy re-verification** (periodically re-issue `resources/subscribe` for `Active` entries) has
  unbounded recovery latency, needs its own scheduling, and relies on subscribe being idempotent
  upstream, which the spec does not guarantee.
- **Disabling `reinit_on_expired_session`** was rejected after reading rmcp's source. It covers
  only the HTTP 404 case, not SSE, and it is not a free win: the reconnect-and-retry branch is
  guarded by `!is_retry` and excludes calls carrying `input_responses`, `request_state`, or
  `expected_route`, so elicitation and task-bearing calls would take a hard failure where they
  silently recover today. Engine-level reconnect is also far heavier than a transport re-init.

## Acceptance Criteria

- [x] A transport-level reconnect that establishes a new upstream MCP session re-issues
      `resources/subscribe` for entries plug still believes are active
- [x] The legacy SSE reconnect path has test coverage for the resume-versus-new-session decision
- [x] The streamable-HTTP `reinit_on_expired_session` behavior is observed by the engine, with the
      choice written down

## Resources

- `plug-core/src/server/mod.rs` — `ConnectionGeneration`, `observe_session`,
  `on_upstream_session_replaced`
- `plug-core/src/transport/sse_client.rs` — `session_observer`, `publish_endpoint`
- `plug-core/src/proxy/subscriptions.rs` — the 073 machinery this reuses
- `plug/src/views/overview.rs`, `plug-core/src/ipc.rs` — subscription count in `plug status`
- `todos/073-done-p2-upstream-reconnect-loses-resource-subscriptions.md`

## Work Log

### 2026-08-08 - Tracked

**By:** Claude Fable 5

Split out of `todos/073`, whose acceptance criteria asked for the SSE silent-reconnect case to be
traced and either covered or ruled out. It is now traced and is neither — it is a distinct defect
with a distinct cause, and the streamable-HTTP re-initialization path turned out to be the more
serious half. No code change made.

### 2026-08-08 - Fixed

**By:** Claude Fable 5

Implemented Option A for both transports. Option A had originally been scoped as "medium-large"
with the caveat that it "needs a signal out of rmcp's streamable-HTTP worker, which may not exist
today without an upstream change or a wrapper transport" — reading rmcp's source settled that: the
`StreamableHttpClient` trait passes the session id into every call, and plug already implements
that trait, so the seam was already there. Option C was dropped for the reason recorded above.
Eight new tests cover the resume-versus-replacement decision on both transports.
