---
title: "feat: elicitation + sampling reverse-request forwarding"
type: feat
status: active
date: 2026-03-08
parent: docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md
---

# feat: elicitation + sampling reverse-request forwarding

## Overview

Add forwarding for `elicitation/create` and `sampling/createMessage` reverse requests across all three
downstream transports (stdio, HTTP, daemon IPC). These are server-to-client requests that occur
**during** an active tool call: the upstream MCP server pauses execution, sends a request back through
plug to the specific downstream client whose tool call triggered it, waits for the client's response,
and resumes tool execution.

This is a single feature branch because elicitation and sampling share:
- the `DownstreamBridge` trait abstraction
- the `resolve_unique_downstream_target_for_upstream()` target-resolution method
- bridge registration lifecycle per downstream transport
- IPC protocol extensions for daemon-backed reverse requests
- `ClientHandler` capability advertisement

## Problem Statement

On `main` @ `cc649ba`, upstream MCP servers that send `elicitation/create` or `sampling/createMessage`
during a tool call receive a default error response from rmcp because `UpstreamClientHandler` does not
implement these `ClientHandler` methods. This blocks any upstream server that uses human-in-the-loop
elicitation or LLM sampling during tool execution.

## Proposed Solution

Introduce a `DownstreamBridge` trait that abstracts reverse-request forwarding per transport. The
`ToolRouter` resolves which downstream client originated the active tool call, looks up the registered
bridge for that client, and forwards the reverse request through it. Each transport implements the
bridge differently:

- **stdio**: calls `Peer<RoleServer>::create_elicitation()` / `create_message()` directly
- **HTTP**: sends a JSON-RPC request via SSE, awaits the client's POST response via a oneshot channel
- **daemon IPC**: sends a new `IpcClientRequest` message to the proxy, which forwards via its own
  downstream peer, and returns the response

## Scope on `main`

### In scope

1. `DownstreamBridge` trait with `create_elicitation()` and `create_message()` methods
2. `StdioBridge` implementation using `Peer<RoleServer>`
3. `HttpBridge` implementation using existing `pending_client_requests` + SSE mechanism
4. `DaemonBridge` implementation using new IPC reverse-request message types
5. Bridge registration in `ToolRouter` via `downstream_bridges: DashMap<NotificationTarget, Arc<dyn DownstreamBridge>>`
6. Target resolution via active call tracking: `resolve_unique_downstream_target_for_upstream()`
7. `UpstreamClientHandler::create_elicitation()` and `create_message()` implementations
8. Capability advertisement: add `sampling` and `elicitation` to `get_info()`
9. rmcp `"elicitation"` feature flag in workspace `Cargo.toml` (required for elicitation peer methods;
    sampling requires no separate feature flag)
10. Downstream context plumbing through daemon `tools/call` dispatch
11. IPC protocol extension with `IpcClientRequest::CreateElicitation` / `CreateMessage` and matching
    response variants
12. Tests proving each transport path

### Not in scope (explicit non-goals)

- **URL-mode elicitation completion notifications** (`notifications/elicitation/url/complete`). This is
  a separate notification-forwarding concern, not a reverse request. Can be added later.
- **Downstream capability synthesis for elicitation/sampling**. These are client-to-server capabilities
  (the client advertises support). Plug does not need to synthesize them in its `ServerCapabilities`
  sent to downstream clients. Downstream clients advertise their own capabilities in their
  `InitializeRequest`.
- **Conditional per-downstream-client upstream capability advertisement**. Plug will advertise
  elicitation + sampling unconditionally to all upstream servers. If a downstream client does not
  support one of these, plug returns an error to the upstream server at routing time. This avoids a
  chicken-and-egg problem: at `get_info()` time, plug does not know which downstream client will call
  which upstream server's tools.
- **`_meta.operationId` correlation**. The MCP spec includes this for request correlation, but rmcp
  does not currently expose it on the `RequestContext`. The target-resolution heuristic
  (single-active-caller-per-server) is sufficient for v1. If multi-caller correlation is needed later,
  it can be added without changing the bridge abstraction.
- **Observability events** (`EngineEvent` variants for reverse requests). Nice-to-have for debugging
  but not required for correctness. Can be added in a follow-up.
- **`plug doctor` / `plug status` changes**. No diagnostic surface changes for v1.
- **Modifying the HTTP reverse-request timeout for roots/list**. The 10-second timeout for roots
  remains unchanged. Only elicitation gets a longer timeout.

## Extraction Sources from `fix/subscription-rebind-confidence`

The archive branch at commit `7d5da18` has a complete, tested implementation. Extraction is
**manual** — the branch has diverged from current `main` (roots, list_changed, protocol-version were
merged separately via squash PRs), so clean cherry-pick is not viable.

| Off-main file | Approx lines | What to extract |
|---|---|---|
| `plug-core/src/server/mod.rs` | 46-62 | `get_info()` with `SamplingCapability` + `ElicitationCapability` |
| `plug-core/src/server/mod.rs` | 194-234 | `create_elicitation()` + `create_message()` in `ClientHandler` impl |
| `plug-core/src/proxy/mod.rs` | 123 | `downstream_bridges` field on `ToolRouter` |
| `plug-core/src/proxy/mod.rs` | 185-196 | `DownstreamBridge` trait definition |
| `plug-core/src/proxy/mod.rs` | 288 | `register_downstream_bridge()` method |
| `plug-core/src/proxy/mod.rs` | 330-399 | `resolve_unique_downstream_target_for_upstream()`, `create_elicitation_from_upstream()`, `create_message_from_upstream()` |
| `plug-core/src/proxy/mod.rs` | 2583-2615 | `StdioBridge` struct + `DownstreamBridge` impl |
| `plug-core/src/http/server.rs` | 54-140 | `HttpBridge` struct + `DownstreamBridge` impl |
| `plug-core/src/ipc.rs` | 194-207 | `IpcClientRequest` / `IpcClientResponse` enum variants |
| `plug/src/daemon.rs` | 551-671 | `DaemonBridge` struct + `DownstreamBridge` impl with capability checks |
| `plug/src/ipc_proxy.rs` | 264-274 | IPC proxy handling of `CreateElicitation` / `CreateMessage` |
| `plug/src/ipc_proxy.rs` | 1408+ | `daemon_backed_proxy_roundtrips_reverse_requests_over_ipc` test |

All off-main code is **extraction source only**, not current truth.

## Technical Approach

### Phase 1: Shared Infrastructure

#### 1a. Enable rmcp `elicitation` feature

Add `"elicitation"` to the rmcp feature list in workspace `Cargo.toml`:

```toml
rmcp = { version = "1.1.0", features = [
    "client",
    "server",
    "macros",
    "schemars",
    "transport-io",
    "transport-child-process",
    "transport-streamable-http-client",
    "transport-streamable-http-client-reqwest",
    "transport-streamable-http-server",
    "server-side-http",
    "reqwest-tls-no-provider",
    "elicitation",
] }
```

**Why this feature flag is needed, and only for elicitation:**

- `Peer<RoleServer>::create_elicitation()` is gated behind `#[cfg(feature = "elicitation")]` in rmcp
  1.1.0 (`service/server.rs:437-440`). Without the feature flag, the `StdioBridge` and IPC proxy
  cannot call `peer.create_elicitation()` on the downstream peer. — *verified in rmcp source*
- `Peer<RoleServer>::create_message()` (sampling) is **not** behind a feature flag. It is available
  unconditionally in rmcp 1.1.0 (`service/server.rs:436`). — *verified in rmcp source*
- The `ClientHandler` trait methods (`create_elicitation()`, `create_message()`) and all associated
  types (`CreateElicitationRequestParams`, `CreateElicitationResult`, `ElicitationCapability`,
  `CreateMessageRequestParams`, `CreateMessageResult`, `SamplingCapability`) are always available
  regardless of feature flags. — *verified in rmcp source*
- There is no `"sampling"` feature flag in rmcp 1.1.0. — *verified in rmcp Cargo.toml*

The `"elicitation"` feature adds a `dep:url` dependency. Implementation must compile against the
actual rmcp API surface on `main`, not assumed symmetry between the two features.

#### 1b. DownstreamBridge trait

Add to `plug-core/src/proxy/mod.rs`:

```rust
pub trait DownstreamBridge: Send + Sync {
    fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>>;

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>>;
}
```

Uses `Pin<Box<dyn Future>>` because the trait is object-safe (stored as `Arc<dyn DownstreamBridge>` in
the `DashMap`). — *verified off-main*

#### 1c. ToolRouter additions

Add field to `ToolRouter`:

```rust
downstream_bridges: DashMap<NotificationTarget, Arc<dyn DownstreamBridge>>,
```

Add methods:

- `register_downstream_bridge(target: NotificationTarget, bridge: Arc<dyn DownstreamBridge>)`
- `unregister_downstream_bridge(target: &NotificationTarget)`
- `resolve_unique_downstream_target_for_upstream(server_id: &str) -> Result<NotificationTarget, McpError>`
- `create_elicitation_from_upstream(server_id: &str, request: CreateElicitationRequestParams) -> Result<CreateElicitationResult, McpError>`
- `create_message_from_upstream(server_id: &str, request: CreateMessageRequestParams) -> Result<CreateMessageResult, McpError>`

**Bridge lifecycle**: register on client initialize, unregister on client disconnect. Follows the same
lifecycle as `client_roots` and `resource_subscriptions`. — *verified on main (roots pattern)*

#### 1d. Capability advertisement

Update `UpstreamClientHandler::get_info()` in `plug-core/src/server/mod.rs`:

```rust
info.capabilities.sampling = Some(SamplingCapability::default());
info.capabilities.elicitation = Some(ElicitationCapability {
    form: Some(FormElicitationCapability::default()),
    url: Some(UrlElicitationCapability::default()),
});
```

Advertised **unconditionally** to all upstream servers. If the downstream client that triggered a tool
call does not support elicitation/sampling, plug returns `McpError::internal_error` to the upstream
server at routing time. This is the correct behavior because:
- Upstream connections are shared across all downstream clients
- `get_info()` runs before plug knows which downstream client will use which server
- Returning an error is a valid MCP response; silently not advertising prevents any client from using
  the feature

— *verified off-main (same design)*

#### 1e. UpstreamClientHandler implementations

Add to `plug-core/src/server/mod.rs`:

```rust
fn create_elicitation(
    &self,
    request: CreateElicitationRequestParams,
    _context: RequestContext<rmcp::RoleClient>,
) -> impl Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_ {
    let router = self.router.clone();
    let server_id = Arc::clone(&self.server_id);
    async move {
        if let Some(router) = router.upgrade() {
            router.create_elicitation_from_upstream(server_id.as_ref(), request).await
        } else {
            Err(McpError::internal_error("router unavailable", None))
        }
    }
}
```

Same pattern for `create_message()`. — *verified off-main (identical structure)*

### Phase 2: Stdio Transport

#### 2a. StdioBridge

Add to `plug-core/src/proxy/mod.rs` near the `ProxyHandler` struct:

```rust
struct StdioBridge {
    peer: Peer<RoleServer>,
}

impl DownstreamBridge for StdioBridge {
    fn create_elicitation(&self, request: CreateElicitationRequestParams)
        -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>>
    {
        let peer = self.peer.clone();
        Box::pin(async move {
            peer.create_elicitation(request)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))
        })
    }
    // create_message: same pattern
}
```

Uses `Peer<RoleServer>::create_elicitation()` directly. Errors propagate naturally if the downstream
peer is disconnected. — *verified off-main*

#### 2b. Bridge registration in ProxyHandler

In `ProxyHandler::on_initialized()` (after storing peer reference and fetching roots), register the
bridge:

```rust
let bridge = Arc::new(StdioBridge { peer: peer.clone() });
router.register_downstream_bridge(
    NotificationTarget::Stdio { client_id: Arc::clone(&self.client_id) },
    bridge,
);
```

On `ProxyHandler::Drop`, unregister:

```rust
router.unregister_downstream_bridge(&NotificationTarget::Stdio {
    client_id: Arc::clone(&self.client_id),
});
```

— *inferred from roots registration pattern on main*

### Phase 3: HTTP Transport

#### 3a. HttpBridge

Add to `plug-core/src/http/server.rs`:

```rust
struct HttpBridge {
    state: Arc<HttpState>,
    session_id: String,
}

impl DownstreamBridge for HttpBridge {
    fn create_elicitation(&self, request: CreateElicitationRequestParams)
        -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>>
    {
        let state = Arc::clone(&self.state);
        let session_id = self.session_id.clone();
        Box::pin(async move {
            let result = send_http_client_request(
                &state,
                &session_id,
                ServerRequest::CreateElicitationRequest(request.into()),
                None, // no timeout for elicitation
            ).await?;
            match result {
                ClientResult::CreateElicitationResult(r) => Ok(r),
                other => Err(McpError::internal_error(
                    format!("unexpected response: {other:?}"), None,
                )),
            }
        })
    }
    // create_message: same pattern, with standard timeout
}
```

— *verified off-main*

#### 3b. Timeout changes to send_http_client_request

Current signature at `plug-core/src/http/server.rs:387`:

```rust
async fn send_http_client_request(state: &HttpState, session_id: &str, request: ServerRequest)
    -> Result<ClientResult, McpError>
```

Change to accept an optional timeout:

```rust
async fn send_http_client_request(
    state: &HttpState,
    session_id: &str,
    request: ServerRequest,
    timeout: Option<Duration>,
) -> Result<ClientResult, McpError>
```

- `Some(duration)` → timeout after that duration (current behavior with 10s for roots)
- `None` → no timeout, only cancelled if the session is cleaned up (oneshot sender dropped)

Callers:
- `maybe_request_http_roots()` passes `Some(Duration::from_secs(10))` (unchanged behavior)
- `HttpBridge::create_elicitation()` passes `None` (human input, unbounded)
- `HttpBridge::create_message()` passes `Some(Duration::from_secs(60))` (LLM completion, bounded)

— *inferred from off-main pattern + SpecFlow analysis*

#### 3c. Bridge registration in HTTP path

In the HTTP `post_mcp()` handler, after processing `InitializedNotification` (where roots are already
fetched), register the bridge:

```rust
let bridge = Arc::new(HttpBridge {
    state: Arc::clone(&state),
    session_id: session_id.clone(),
});
state.router.register_downstream_bridge(
    NotificationTarget::Http { session_id: Arc::from(session_id.as_str()) },
    bridge,
);
```

On HTTP DELETE (session disconnect), unregister the bridge alongside existing cleanup.

— *inferred from roots pattern on main*

#### 3d. Cleanup on session expiry

Existing session expiry cleanup in `plug-core/src/http/server.rs` (DELETE handler) removes
`pending_client_requests` and `roots_capable_sessions`. Add `unregister_downstream_bridge` to the same
cleanup path.

Additionally: resolve any pending oneshot senders for expired sessions with an error instead of
silently dropping them. This prevents upstream servers from hanging on a dead elicitation.

— *inferred from SpecFlow analysis*

### Phase 4: Daemon IPC Transport

This is the most complex transport because the current IPC protocol is client-initiated only.

#### 4a. IPC protocol extension

Add to `plug-core/src/ipc.rs`:

```rust
pub enum IpcClientRequest {
    CreateElicitation { params: CreateElicitationRequestParams },
    CreateMessage { params: CreateMessageRequestParams },
}

pub enum IpcClientResponse {
    CreateElicitation { result: CreateElicitationResult },
    CreateMessage { result: CreateMessageResult },
}
```

These are **daemon-to-proxy** messages (the daemon initiates a request to the IPC proxy client). This
is the reverse direction from the normal `IpcRequest` / `IpcResponse` flow.

— *verified off-main (identical types)*

#### 4b. IPC reverse-request delivery mechanism

The current IPC read loop in `plug/src/ipc_proxy.rs` reads `IpcResponse` messages from the daemon
after sending an `IpcRequest`. For reverse requests, the daemon needs to push `IpcClientRequest`
messages interleaved with normal responses while a tool call is in flight.

The off-main branch solves this with a `send_request()` method on the daemon connection that:
1. Assigns a reverse-request ID
2. Serializes the `IpcClientRequest` as a tagged message
3. Sends it over the Unix socket
4. Awaits the proxy's `IpcClientResponse`

The proxy's read loop is modified to handle both normal `IpcResponse` and interleaved
`IpcClientRequest` messages. When it receives an `IpcClientRequest`, it calls the downstream peer
(`peer.create_elicitation()` / `peer.create_message()`) and sends back an `IpcClientResponse`.

— *verified off-main*

#### 4c. Downstream context through daemon tool calls

On `main`, the daemon's `tools/call` handler at `plug/src/daemon.rs:1248` calls
`tool_router.call_tool(&name, arguments)`, which passes `None` for downstream context. This means
`ActiveCallRecord.downstream` is unpopulated for daemon IPC calls, breaking target resolution.

Change to `tool_router.call_tool_with_context()`, passing a `DownstreamCallContext` that represents
the daemon IPC session. Since daemon IPC sessions ultimately represent stdio clients behind the proxy,
use `NotificationTarget::Stdio { client_id: session_id }` as the target — consistent with how
daemon sessions are identified elsewhere in the codebase (logging, roots).

— *verified off-main (same approach)*

#### 4d. DaemonBridge

Add to `plug/src/daemon.rs`:

```rust
struct DaemonBridge {
    session_id: String,
    connection: /* reference to the IPC connection for this session */,
    capabilities: ClientCapabilities,
}

impl DownstreamBridge for DaemonBridge {
    fn create_elicitation(&self, request: CreateElicitationRequestParams)
        -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>>
    {
        if self.capabilities.elicitation.is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    format!("client {} does not support elicitation", self.session_id), None,
                ))
            });
        }
        Box::pin(async move {
            let resp = self.connection
                .send_request(IpcClientRequest::CreateElicitation { params: request })
                .await?;
            match resp {
                IpcClientResponse::CreateElicitation { result } => Ok(result),
                other => Err(McpError::internal_error(
                    format!("unexpected response: {other:?}"), None,
                )),
            }
        })
    }
    // create_message: same pattern with sampling capability check
}
```

The `DaemonBridge` checks `capabilities.elicitation` / `capabilities.sampling` before forwarding.
These capabilities come from the downstream client's `InitializeRequest`, which the IPC proxy already
parses and sends to the daemon during registration.

— *verified off-main (identical design)*

#### 4e. IPC proxy handler for reverse requests

In `plug/src/ipc_proxy.rs`, when the proxy's IPC read loop receives an `IpcClientRequest`:

```rust
IpcClientRequest::CreateElicitation { params } => {
    match peer.create_elicitation(params).await {
        Ok(result) => IpcClientResponse::CreateElicitation { result },
        Err(e) => /* error response */,
    }
}
IpcClientRequest::CreateMessage { params } => {
    match peer.create_message(params).await {
        Ok(result) => IpcClientResponse::CreateMessage { result },
        Err(e) => /* error response */,
    }
}
```

— *verified off-main*

## Target Resolution

### Method

`resolve_unique_downstream_target_for_upstream(server_id: &str)` iterates all `active_calls`, filters
by `upstream_server_id == server_id`, extracts unique `NotificationTarget` values via
`downstream.notification_target()`, and:

| Active callers to this server | Behavior |
|---|---|
| 0 | Return `McpError::internal_error("no active downstream call for upstream server {server_id}")` |
| 1 unique target | Return that `NotificationTarget` |
| 2+ unique targets | Return `McpError::internal_error("ambiguous downstream ownership for upstream server {server_id}")` |

### Rationale

The MCP spec says reverse requests occur during a tool call and should be routed to the session that
triggered it. Since `UpstreamClientHandler::create_elicitation()` receives a `RequestContext` with the
elicitation's own request ID (not the tool call's request ID), there is no direct correlation field.

The server-scoped heuristic works because:
- In the common case, one client has one active tool call to a server
- If the same client has multiple concurrent calls to the same server, they share a
  `NotificationTarget`, so deduplication yields 1 target — correct behavior
- If different clients have concurrent calls to the same server, the result is genuinely ambiguous and
  returning an error is the safe choice

— *verified off-main (identical algorithm)*

### Edge case: race on completion

If a tool call completes and `remove_active_call()` fires at the same moment the upstream server sends
a reverse request, the lookup may find 0 active calls. This is handled by the 0-callers error path.
The upstream server will receive an error, which is correct — the tool call is already done.

## Capability Advertisement Rules

### V1 architectural constraint: process-wide upstream advertisement

Upstream capability advertisement is a **process-wide, shared-connection** concern. This is a
deliberate v1 tradeoff driven by plug's architecture, not an incidental choice.

**The constraint:** `UpstreamClientHandler::get_info()` runs once per upstream server connection,
before any downstream client has connected or called a tool. The upstream connection is then shared
across all downstream clients for the lifetime of the process. At `get_info()` time, plug does not
know — and cannot know — which downstream clients will eventually connect, which of those will support
elicitation or sampling, or which client will call which server's tools.

**The v1 choice:** Advertise `sampling` and `elicitation` capabilities unconditionally to all upstream
servers, and perform downstream capability checks at reverse-request routing time.

**What this means in practice:**

1. **Upstream servers will believe plug supports elicitation and sampling.** Compliant upstream servers
   check client capabilities before sending reverse requests. Because plug advertises support, they
   will send these requests when their tool logic requires them.

2. **If the downstream client that triggered the tool call does not support elicitation/sampling,**
   plug returns `McpError::internal_error` to the upstream server at routing time. The upstream server
   receives a clear error and can handle it (e.g., skip the elicitation, fall back to a default).

3. **This means unsupported downstream clients fail at reverse-request time, not at initialize time.**
   The downstream client sees no error — it just receives the tool call result (which may be an error
   from the upstream server due to the failed elicitation). The upstream server sees the error and
   decides how to proceed.

**Why per-client upstream advertisement is not available in v1:**

- Upstream connections are process-wide singletons managed by `ServerManager`
- `get_info()` is called once at connection time, not per downstream session
- Re-initializing upstream connections when a new downstream client connects would add complexity and
  break the shared-connection model
- The MCP spec does not provide a mechanism to update client capabilities after initialization

**Acceptable because:**

- Returning an error to an elicitation/sampling request is a valid MCP response
- Upstream servers that send reverse requests must handle errors gracefully (the downstream human may
  be unavailable, the client may not support the feature, etc.)
- The alternative — never advertising support — would block all downstream clients from using
  elicitation/sampling, even those that support it

### Capability rules by layer

1. **Upstream (plug as client to upstream servers)**: `UpstreamClientHandler::get_info()` always
   advertises `sampling` + `elicitation`. Unconditional, process-wide.

2. **Downstream (plug as server to downstream clients)**: No change needed. Elicitation and sampling
   are CLIENT capabilities (the client advertises support to the server). Plug does not need to add
   anything to `synthesized_capabilities()`. Downstream clients advertise their own support in their
   `InitializeRequest`.

3. **Routing-time capability gating**: Each `DownstreamBridge` implementation checks the downstream
   client's advertised capabilities before forwarding. If the client did not advertise `elicitation`
   or `sampling` in its `InitializeRequest`, the bridge returns an error immediately rather than
   attempting a reverse request that will fail. This check happens at the bridge layer, not the router
   layer, so transport-specific behavior can vary (e.g., `DaemonBridge` checks `ClientCapabilities`
   from registration; `StdioBridge` can rely on rmcp's own error if the client doesn't handle it).

4. **Capability tracking**: On `main` today, `roots_supported` (stdio) and `roots_capable_sessions`
   (HTTP) track per-client capabilities at initialize time. Extend this pattern: capture
   `elicitation` and `sampling` support from the downstream client's `InitializeRequest.capabilities`
   and store it alongside existing capability tracking. The bridge reads this at routing time.

## Timeout and Lifecycle Behavior

### V1 decision: reverse requests consume the tool-call timeout budget

This is a deliberate v1 architectural choice.

Reverse requests (elicitation, sampling) happen **inside** an active tool call. In v1, they consume
the originating tool call's timeout budget — there is no separate timeout or timeout-pause mechanism
for reverse requests. The existing per-server `call_timeout_secs` (default: varies per `ServerConfig`)
governs the entire tool call, including any time spent waiting for downstream reverse-request
responses.

**Consequences of this choice:**

1. If `call_timeout_secs` is 30s and an elicitation takes 25s of human input, the upstream server has
   only 5s remaining to finish tool execution after the elicitation response is returned.
2. If the timeout fires while waiting on an elicitation/sampling response, the entire tool-call future
   is cancelled by rmcp / the `send_cancellable_request()` timeout. This triggers the `ActiveCallGuard`
   RAII cleanup, which calls `remove_active_call()`.
3. Operators who use elicitation-heavy upstream servers should set a higher `call_timeout_secs` for
   those servers. This is a configuration concern, not a code concern.

**What happens when timeout fires during a reverse request:**

- The tool-call future drops (rmcp cancels it or `send_cancellable_request` times out)
- `ActiveCallGuard::drop()` fires, calling `remove_active_call()` to clean up tracking maps
- The bridge call (`create_elicitation` / `create_message`) is cancelled via future drop:
  - **stdio**: the `peer.create_elicitation().await` is cancelled; rmcp handles connection cleanup
  - **HTTP**: the oneshot receiver is dropped; the pending sender in `pending_client_requests` becomes
    orphaned and must be cleaned up (see session expiry cleanup below)
  - **daemon IPC**: the `send_request().await` is cancelled; the IPC connection remains valid for
    future requests
- The upstream server receives a timeout/cancellation error via the existing error path

**Why not pause the timeout during reverse requests:**

- Adds complexity (pausing/resuming a tokio timeout, or replacing with a state machine)
- Makes tool-call duration unpredictable from the upstream server's perspective
- The simpler fix (increase per-server timeout) is available today and sufficient for v1
- Can be revisited if real-world usage shows this is too constraining

### Per-transport reverse-request behavior

**Elicitation:**

- **stdio**: No bridge-level timeout. `Peer<RoleServer>::create_elicitation()` blocks until the client
  responds or disconnects. The tool-call timeout is the only time bound.
- **HTTP**: No bridge-level timeout (`send_http_client_request()` called with `timeout: None`).
  Elicitation waits for human input. Bounded only by the tool-call timeout and session lifecycle. The
  reverse request is also cancelled if the HTTP session expires (DELETE) or the SSE connection drops
  (oneshot sender dropped).
- **daemon IPC**: No bridge-level timeout. `DaemonBridge::send_request()` blocks until the proxy
  responds or the connection drops. Bounded by tool-call timeout.

**Sampling:**

- **stdio**: No bridge-level timeout (same as elicitation).
- **HTTP**: 60-second bridge-level timeout. LLM completions are bounded and should not take arbitrarily
  long. This is in addition to the tool-call timeout (whichever fires first wins).
- **daemon IPC**: No bridge-level timeout.

### Client disconnect during reverse request

- **stdio**: `peer.create_elicitation().await` returns an error from rmcp when the peer disconnects.
  This error propagates back to `UpstreamClientHandler`, which returns it to the upstream server.
- **HTTP**: The SSE connection drops, the oneshot sender in `pending_client_requests` is dropped on
  session cleanup, `rx.await` returns `Err(RecvError)`, which is mapped to `McpError`. Additionally,
  session expiry must resolve any pending oneshot senders with an error instead of silently dropping
  them. This prevents upstream servers from hanging on a dead elicitation. Implementation: iterate
  `pending_client_requests` entries for the expired session and send errors through the oneshot
  channels before removing them.
- **daemon IPC**: The Unix socket closes, `send_request()` returns an error, which propagates.

### Upstream disconnect during reverse request

The upstream server disconnecting kills the tool call future (rmcp drops the connection). The
`ActiveCallGuard` RAII cleanup fires, calling `remove_active_call()`. The downstream client may still
be showing an elicitation UI — the user's response will be silently discarded when it arrives (the
oneshot sender / peer call has no upstream to return to). This is acceptable for v1; active
cancellation of in-flight downstream reverse requests is a follow-up optimization.

### Cleanup obligations summary

When a reverse request is torn down (timeout, disconnect, or cancellation), the following cleanup
must happen:

| Trigger | Active call tracking | Bridge state | Pending HTTP requests |
|---|---|---|---|
| Tool-call timeout | `ActiveCallGuard::drop()` removes record | Future cancelled automatically | Orphaned sender cleaned up on session expiry |
| Downstream disconnect | Error propagates to upstream | Bridge unregistered on disconnect | Pending senders resolved with error |
| Upstream disconnect | `ActiveCallGuard::drop()` removes record | Downstream may show stale UI (v1 accepted) | N/A (upstream is the receiver) |
| Session expiry (HTTP) | Record remains until tool-call timeout | Bridge unregistered | Pending senders resolved with error before removal |

## Tests

### Required

1. **Unit: target resolution — 0 active calls**
   - Call `resolve_unique_downstream_target_for_upstream("unknown-server")`
   - Assert `Err` with "no active downstream call"

2. **Unit: target resolution — 1 active call**
   - Register an `ActiveCallRecord` for server "s1" from a stdio client
   - Call `resolve_unique_downstream_target_for_upstream("s1")`
   - Assert `Ok(NotificationTarget::Stdio { client_id })` with correct ID

3. **Unit: target resolution — 2 calls from same client (dedup)**
   - Register two `ActiveCallRecord`s for server "s1" from the SAME stdio client (different call IDs)
   - Call `resolve_unique_downstream_target_for_upstream("s1")`
   - Assert `Ok` (deduplicated to 1 target)

4. **Unit: target resolution — 2 calls from different clients (ambiguous)**
   - Register `ActiveCallRecord`s for server "s1" from client A (stdio) and client B (HTTP)
   - Call `resolve_unique_downstream_target_for_upstream("s1")`
   - Assert `Err` with "ambiguous"

5. **Integration: stdio elicitation round-trip**
   - Start a mock upstream server that sends `elicitation/create` during `tools/call`
   - Connect via `ProxyHandler` (stdio path)
   - Verify the elicitation request reaches the downstream client
   - Return a response from the downstream client
   - Verify the tool call completes with the expected result

6. **Integration: HTTP elicitation round-trip**
   - Start a mock upstream server with elicitation
   - Start an HTTP server with `HttpState`
   - Connect a test client via SSE + POST
   - Verify the elicitation request arrives on the SSE stream
   - POST the response back
   - Verify the tool call completes

7. **Integration: daemon IPC elicitation round-trip**
   - Similar to the existing `daemon_backed_proxy_roundtrips_reverse_requests_over_ipc` test on the
     off-main branch
   - Start daemon, connect IPC proxy, call tool, verify elicitation forwarded through IPC

8. **Integration: sampling round-trip (stdio)**
   - Same shape as test 5, with `sampling/createMessage`

9. **Unit: DaemonBridge capability gating**
   - Create a `DaemonBridge` with `capabilities.elicitation = None`
   - Call `create_elicitation()`
   - Assert immediate `Err` without IPC round-trip

### Nice-to-have (not required for merge)

- HTTP session disconnect during elicitation (verify upstream gets error)
- Concurrent tool calls from same client to same server (verify both complete)
- Bridge cleanup on disconnect (verify `downstream_bridges` entry removed)

## Acceptance Criteria

- [ ] `UpstreamClientHandler` implements `create_elicitation()` and `create_message()`
- [ ] `UpstreamClientHandler::get_info()` advertises sampling + elicitation capabilities
- [ ] `DownstreamBridge` trait defined with both methods
- [ ] `StdioBridge` implemented and registered on `ProxyHandler` initialize
- [ ] `HttpBridge` implemented and registered on HTTP session initialize
- [ ] `DaemonBridge` implemented with capability gating
- [ ] `IpcClientRequest` / `IpcClientResponse` variants added to IPC protocol
- [ ] IPC proxy handles reverse requests from daemon
- [ ] Daemon passes `DownstreamCallContext` through `call_tool_with_context()`
- [ ] Target resolution returns error for 0 or 2+ unique callers
- [ ] HTTP `send_http_client_request()` accepts optional timeout
- [ ] Reverse requests consume the tool-call timeout budget (no separate timeout mechanism)
- [ ] HTTP elicitation has no bridge-level timeout; HTTP sampling has 60s bridge-level timeout
- [ ] Session expiry resolves orphaned pending HTTP requests with error before removal
- [ ] `"elicitation"` feature added to rmcp in `Cargo.toml` (sampling needs no feature flag)
- [ ] All required tests pass
- [ ] `cargo test`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo fmt --check`

## Implementation Phases

### Phase 1: Shared infrastructure
- `Cargo.toml`: add `"elicitation"` feature (sampling needs no feature flag)
- `DownstreamBridge` trait
- `ToolRouter` fields + methods (bridge map, target resolution, router methods)
- `UpstreamClientHandler` implementations + capability advertisement
- Unit tests for target resolution (tests 1-4)

### Phase 2: Stdio bridge
- `StdioBridge` struct + impl
- Bridge registration/cleanup in `ProxyHandler`
- Integration test (test 5)
- Integration test for sampling (test 8)

### Phase 3: HTTP bridge
- `HttpBridge` struct + impl
- Timeout parameter on `send_http_client_request()`
- Bridge registration/cleanup in HTTP handlers
- Session expiry error resolution
- Integration test (test 6)

### Phase 4: Daemon IPC bridge
- `IpcClientRequest` / `IpcClientResponse` types
- IPC proxy reverse-request handling
- `DaemonBridge` struct + impl with capability gating
- Downstream context plumbing in daemon `tools/call`
- Integration test (test 7)
- Capability gating unit test (test 9)

## Dependencies and Risks

### Dependencies

- rmcp 1.1.0 (already on `main`) provides all required types and trait methods
- rmcp `"elicitation"` feature flag must compile cleanly with existing features

### Risks

| Risk | Severity | Mitigation |
|---|---|---|
| IPC protocol deadlock: daemon cannot push reverse request while proxy awaits tool call response | High | Off-main branch has a proven solution; extract the bidirectional IPC pattern |
| Ambiguous target resolution with concurrent multi-client calls to same server | Medium | Error on ambiguity is the safe default; `_meta.operationId` correlation can be added later |
| HTTP SSE connection dropped during long elicitation | Medium | Oneshot sender drop → error propagation; session expiry resolves pending senders with error before removal |
| Tool-call timeout fires during elicitation wait | Medium | V1 decision: reverse requests consume the tool-call timeout budget (see Timeout section). Operators configure per-server `call_timeout_secs` for elicitation-heavy servers. Timeout-pause is a post-v1 option if needed. |
| Orphaned HTTP pending requests after timeout | Medium | `ActiveCallGuard::drop()` cleans up tracking maps; session expiry cleanup resolves orphaned oneshot senders; bridge unregistered on disconnect |

## Sources

### On main (verified)

- `plug-core/src/server/mod.rs:39-63` — `UpstreamClientHandler::get_info()` + `list_roots()`
- `plug-core/src/proxy/mod.rs:90-120` — `ToolRouter` fields
- `plug-core/src/proxy/mod.rs:104-107` — active call tracking maps
- `plug-core/src/proxy/mod.rs:129-162` — `DownstreamCallContext`
- `plug-core/src/proxy/mod.rs:193-199` — `ActiveCallRecord`
- `plug-core/src/proxy/mod.rs:525-579` — active call registration/cleanup
- `plug-core/src/http/server.rs:37-52` — `HttpState` with `pending_client_requests`
- `plug-core/src/http/server.rs:387-442` — `send_http_client_request()` + `handle_client_response()`
- `plug-core/src/notifications.rs:37-41` — `NotificationTarget` enum
- `Cargo.toml:14-28` — rmcp workspace dependency

### Off main (extraction source only)

- `fix/subscription-rebind-confidence` @ `7d5da18` — complete implementation with tests

### rmcp 1.1.0 (verified in source)

- `handler/client.rs:90-172` — `ClientHandler` trait with `create_message()`, `create_elicitation()`
- `service/server.rs:436-441` — `Peer<RoleServer>` methods behind `elicitation` feature
- `model/capabilities.rs:212-266` — `ElicitationCapability`, `SamplingCapability`

### Institutional learnings

- `docs/solutions/integration-issues/resource-subscribe-forwarding-lifecycle-20260307.md` — three-transport parity checklist, rollback patterns
- `docs/solutions/integration-issues/mcp-logging-notification-forwarding-20260307.md` — bulkhead pattern for broadcast channels, session cleanup
