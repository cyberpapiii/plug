# rmcp Feasibility Study for Plug (MCP Multiplexer/Fanout Proxy)

> Historical research note: This is a point-in-time feasibility study from 2026-03-03. It reflects
> the SDK and project state at the time of research, not the current merged state on `main`. Use
> `docs/PROJECT-STATE-SNAPSHOT.md`, `docs/PLAN.md`, and current code on `main` for live status.

**Date:** 2026-03-03
**rmcp version evaluated:** 1.0.0 (released 2026-03-03)
**Repository:** https://github.com/modelcontextprotocol/rust-sdk

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Can ServerHandler and ClientHandler Coexist?](#1-can-serverhandler-and-clienthandler-coexist-in-one-binary)
3. [Supported Transports](#2-what-transports-does-rmcp-actually-support)
4. [Custom Transport Bridging](#3-can-we-create-a-custom-transport-that-bridges-inboundoutbound)
5. [MCP Spec 2025-11-25 Compliance](#4-does-rmcp-support-mcp-spec-2025-11-25-fully)
6. [AgentGateway Analysis](#5-does-agentgateway-use-rmcp)
7. [Proxy Feasibility and Architecture](#6-proxy-feasibility)
8. [Risks and Open Questions](#risks-and-open-questions)
9. [Recommendation](#recommendation)

---

## Executive Summary

**rmcp is highly feasible for the plug multiplexer project.** The SDK explicitly supports running both `ServerHandler` and `ClientHandler` in the same binary, provides composable transport abstractions, and has production-grade prior art in AgentGateway (which uses rmcp for exactly this proxy/fanout pattern). The `rmcp-proxy` community crate further validates the pattern. rmcp 1.0.0 supports MCP protocol versions up to `2025-06-18`, with Tasks (experimental) and robust OAuth 2.1 support. Full `2025-11-25` spec coverage (CIMD, M2M client-credentials) is not yet confirmed but the auth module is comprehensive.

---

## 1. Can ServerHandler and ClientHandler Coexist in One Binary?

**Answer: Yes, definitively.** This is a core design pattern of rmcp, not a workaround.

### Evidence

**Feature flags are independent and composable:**
The `client` and `server` features can both be enabled simultaneously. They are conditionally compiled but not mutually exclusive:

```rust
// From crates/rmcp/src/handler.rs
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;
```

Source: [handler.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/handler.rs)

**The role system is generic, not exclusive:**
The `ServiceRole` trait parameterizes services on their role (`RoleClient` or `RoleServer`). A single binary can instantiate services of both roles:

```rust
pub trait ServiceRole {
    const IS_CLIENT: bool;
    type Req; type Resp; type Not;       // Local types
    type PeerReq; type PeerResp; type PeerNot; // Peer types
    type Info; type PeerInfo;
}
```

Source: [service.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/service.rs)

**The TCP example runs both in one binary:**
The `examples/transport/src/tcp.rs` creates a TCP server accepting connections and a TCP client connecting to it in the same `main()`:

```rust
// Server side
let server = Calculator::new().serve(stream).await?;
// Client side (same binary)
let client = ().serve(client_stream).await?;
client.peer().list_tools(Default::default()).await?;
```

Source: [tcp.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/transport/src/tcp.rs)

**The HTTP upgrade example does the same:**
`examples/transport/src/http_upgrade.rs` starts an HTTP server and connects a client to it, both within a single process.

Source: [http_upgrade.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/transport/src/http_upgrade.rs)

**The `rmcp-proxy` crate validates the proxy pattern:**
The community `rmcp-proxy` crate (by stephenlacy) implements exactly this: a `ProxyHandler` that implements `ServerHandler` while holding a `RunningService<RoleClient>` internally:

```rust
pub struct ProxyHandler {
    client: Mutex<RunningService<RoleClient, ClientInfo>>,
    server_info: ServerInfo,
}
```

Source: [rmcp-proxy proxy_handler.rs](https://github.com/stephenlacy/mcp-proxy/blob/main/src/proxy_handler.rs)

### Conclusion

No architectural barriers. The `ServerHandler` and `ClientHandler` traits are independent traits on independent types. A struct can implement `ServerHandler` while owning multiple `RunningService<RoleClient>` instances for upstream connections.

---

## 2. What Transports Does rmcp Actually Support?

### Supported Transports (rmcp 1.0.0)

| Transport | Feature Flag | Role | Notes |
|-----------|-------------|------|-------|
| **Stdio** (`stdin`/`stdout`) | `transport-io` | Server | Via `rmcp::transport::io::stdio()` returning `(Stdin, Stdout)` |
| **Child Process** | `transport-child-process` | Client | `TokioChildProcess` spawns subprocess, communicates via stdio |
| **Streamable HTTP (Client)** | `transport-streamable-http-client` | Client | `StreamableHttpClientTransport` with SSE for server-to-client |
| **Streamable HTTP (Server)** | `transport-streamable-http-server` | Server | `StreamableHttpService` integrating with Tower/Axum |
| **AsyncRead/AsyncWrite** | `transport-async-rw` | Both | `AsyncRwTransport` wraps any `AsyncRead`+`AsyncWrite` |
| **Sink/Stream pairs** | (core) | Both | `SinkStreamTransport` from any `(Sink, Stream)` tuple |
| **TCP** | (via async-rw) | Both | Example in `examples/transport/src/tcp.rs` |
| **Unix Socket** | (via async-rw) | Both | Example in `examples/transport/src/unix_socket.rs` |
| **WebSocket** | (ws feature) | Both | `ws.rs` in transport module |
| **Named Pipes** | (via async-rw) | Both | Windows example in `examples/transport/src/named-pipe.rs` |
| **HTTP Upgrade** | (via async-rw) | Both | Example in `examples/transport/src/http_upgrade.rs` |

Source: [transport module docs](https://docs.rs/rmcp/latest/rmcp/transport/index.html), [Cargo.toml features](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml)

### SSE Status

**SSE as a standalone transport is deprecated in the MCP spec.** The MCP spec replaced the separate `HTTP+SSE` transport with `Streamable HTTP` starting in spec version `2025-03-26`. rmcp's `StreamableHttpClientTransport` uses SSE internally as the server-to-client channel within the Streamable HTTP protocol, but there is no standalone SSE transport feature flag in rmcp 1.0.0.

AgentGateway maintains its own legacy SSE implementation (`LegacySSEService`) for backward compatibility with older clients, but this is custom code, not an rmcp-provided transport.

Source: [MCP SSE deprecation](https://blog.fka.dev/blog/2025-06-06-why-mcp-deprecated-sse-and-go-with-streamable-http/), [AgentGateway sse.rs](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/src/mcp/sse.rs)

### Transport for Plug

For the plug multiplexer:
- **Downstream (to clients):** Streamable HTTP Server via `StreamableHttpService` (Tower/Axum integration)
- **Upstream (to servers):** Streamable HTTP Client via `StreamableHttpClientTransport`, plus `TokioChildProcess` for stdio servers
- **Internal bridging:** Not needed -- each direction gets its own transport instance

---

## 3. Can We Create a Custom Transport That Bridges Inbound/Outbound?

**Answer: Yes, rmcp's transport abstraction is highly composable, but a bridge transport is not actually needed for the proxy pattern.**

### The Transport Trait

```rust
pub trait Transport<R: ServiceRole>: Send {
    type Error: std::error::Error + Send + Sync + 'static;

    fn send(&mut self, item: TxJsonRpcMessage<R>)
        -> impl Future<Output = Result<(), Self::Error>> + Send + 'static;
    fn receive(&mut self)
        -> impl Future<Output = Option<RxJsonRpcMessage<R>>> + Send;
    fn close(&mut self)
        -> impl Future<Output = Result<(), Self::Error>> + Send;
}
```

Source: [transport.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport.rs)

### The IntoTransport Trait

```rust
pub trait IntoTransport<R, E, A>: Send + 'static
where R: ServiceRole, E: std::error::Error + Send + 'static {
    fn into_transport(self) -> impl Transport<R, Error = E> + 'static;
}
```

`IntoTransport` is automatically implemented for:
- Any type already implementing `Transport`
- Any `(Sink, Stream)` pair via `TransportAdapterSinkStream`
- Any `AsyncRead + AsyncWrite` type via `TransportAdapterAsyncCombinedRW`
- Any `Worker` implementation via `WorkerTransport`

Source: [sink_stream.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport/sink_stream.rs), [transport module docs](https://docs.rs/rmcp/latest/rmcp/transport/index.html)

### Why a Bridge Transport is Unnecessary

The proxy pattern does NOT require a custom bridging transport. Instead, it uses:

1. A `ServerHandler` implementation that receives requests from downstream clients
2. One or more `RunningService<RoleClient>` instances connected to upstream servers
3. The `ServerHandler` methods forward requests to the appropriate upstream via `client.peer().list_tools()`, `client.peer().call_tool()`, etc.

Each side (downstream server, upstream clients) has its own independent transport. The "bridge" is application-level logic in the `ServerHandler` implementation, not a transport-level concern.

This is exactly how both `rmcp-proxy` and AgentGateway implement it.

---

## 4. Does rmcp Support MCP Spec 2025-11-25 Fully?

### Protocol Version Constants

rmcp defines these protocol versions:

```rust
pub const V_2024_11_05: ProtocolVersion;
pub const V_2025_03_26: ProtocolVersion;
pub const V_2025_06_18: ProtocolVersion;
```

Source: [model.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model.rs)

**The `2025-11-25` protocol version constant is NOT present in the source code reviewed.** The latest supported protocol version appears to be `2025-06-18`.

### Feature-by-Feature Assessment

| MCP 2025-11-25 Feature | rmcp Support | Evidence |
|------------------------|-------------|----------|
| **Tasks (experimental)** | **Supported** | `ServerHandler` has `enqueue_task`, `list_tasks`, `get_task_info`, `get_task_result`, `cancel_task`. `TaskStatus` enum has all 5 states. `TasksCapability` in capabilities. `task_manager.rs` provides `OperationProcessor`. |
| **OAuth 2.1 / PKCE** | **Supported** | Comprehensive `auth.rs` module with `AuthorizationManager`, PKCE support, RFC 8414 metadata discovery, Dynamic Client Registration, scope management, token refresh. |
| **OIDC Discovery** | **Likely supported** | Auth module implements RFC 8414 metadata discovery with multi-path fallback. |
| **CIMD (Client ID Metadata Documents)** | **Partial/Uncertain** | Auth module supports SEP-991 URL-based Client IDs. Whether full CIMD fetch-and-validate is implemented needs code-level verification. |
| **Icons metadata** | **Supported** | `Tool` struct has `icons: Option<Vec<Icon>>`. `Implementation` struct has optional icons. `Icon` type supports multiple MIME types and sizes. |
| **Elicitation (form mode)** | **Supported** | `ClientHandler::create_elicitation` with `FormElicitationParams` and JSON Schema validation. `elicitation_schema.rs` provides builder API. |
| **Elicitation (URL mode)** | **Supported** | `UrlElicitationParams` in `CreateElicitationRequestParams` enum. `on_url_elicitation_notification_complete` notification handler in `ClientHandler`. |
| **Sampling with tool calling** | **Supported** | `CreateMessageRequestParams` includes `ToolChoice` modes (Auto/Required/None per SEP-1577), tool definitions in sampling requests. |
| **M2M client-credentials** | **Uncertain** | OAuth module is comprehensive but explicit `client_credentials` grant type support needs verification. The auth module focuses primarily on authorization code flow with PKCE. |
| **Extensions framework** | **Supported** | `ServerCapabilities` and `ClientCapabilities` both have `extensions: BTreeMap<String, JsonObject>` and `experimental: BTreeMap<String, JsonObject>`. |
| **Tool annotations** | **Supported** | `ToolAnnotations` with `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint`. |
| **Tool execution / task_support** | **Supported** | `ToolExecution` struct with `TaskSupport` enum (Forbidden, Optional, Required). |
| **Streamable HTTP** | **Supported** | Full client and server implementations with session management. |

Sources: [handler/server.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/handler/server.rs), [handler/client.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/handler/client.rs), [model/capabilities.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model/capabilities.rs), [model/task.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model/task.rs), [model/tool.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model/tool.rs), [transport/auth.rs](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/transport/auth.rs)

### Summary

rmcp has **strong coverage** of the 2025-11-25 spec features, but the protocol version constant `V_2025_11_25` is not yet present. The SDK appears to implement most features from the latest spec under the `2025-06-18` version identifier. The gap is likely minor (version negotiation string) rather than functional.

---

## 5. Does AgentGateway Use rmcp?

**Answer: Yes.** AgentGateway uses rmcp as a direct dependency for its MCP implementation.

### Dependency Declaration

From AgentGateway's `crates/agentgateway/Cargo.toml`:

```toml
rmcp = { version = "0.16", features = ["client", "server", "base64", "transport-child-process"] }
```

Source: [agentgateway Cargo.toml](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/Cargo.toml)

### Architecture

AgentGateway implements a full MCP proxy/fanout using rmcp:

**`Relay` struct** (in `mcp/handler.rs`):
- Implements `ServerHandler` to face downstream clients
- Contains an `UpstreamGroup` managing multiple upstream MCP connections
- Routes requests based on type: fanout for list operations, targeted for call operations

**`UpstreamGroup`** (in `mcp/upstream/mod.rs`):
- Manages multiple `Upstream` connections stored in an `IndexMap`
- Supports four transport types: `McpStreamable`, `McpSSE`, `McpStdio`, `OpenAPI`
- Each upstream wraps rmcp's client service

**Fanout pattern:**
- `list_tools`, `list_prompts`, `list_resources` broadcast to ALL upstreams and merge results
- `call_tool`, `get_prompt`, `read_resource` route to a SPECIFIC upstream based on name prefix
- Results are merged with tool/prompt name prefixing for multiplexed mode
- RBAC policies filter results at the aggregation layer

**Session management:**
- `SessionManager` maintains `HashMap<String, Session>` for stateful HTTP connections
- Each session creates a new `Relay` with its own upstream connections
- Supports both Streamable HTTP and legacy SSE transports
- `SessionDropper` ensures cleanup on disconnect

**What AgentGateway builds on top of rmcp:**
- Custom legacy SSE transport (`LegacySSEService`) -- rmcp does not provide this
- RBAC authorization using CEL expressions
- Tool/prompt/resource name prefixing for multiplexing
- Custom HTTP routing (not using rmcp's `StreamableHttpService` directly for the inbound side)
- OpenAPI-to-MCP translation layer

Sources: [mcp/handler.rs](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/src/mcp/handler.rs), [mcp/upstream/mod.rs](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/src/mcp/upstream/mod.rs), [mcp/session.rs](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/src/mcp/session.rs), [mcp/mod.rs](https://github.com/agentgateway/agentgateway/blob/main/crates/agentgateway/src/mcp/mod.rs)

---

## 6. Proxy Feasibility

### Can We Write a Minimal Proxy?

**Yes.** The architecture is well-established with two production implementations as prior art.

### Proposed Architecture for Plug

```
                         +-----------------------+
                         |        Plug           |
                         |   (single binary)     |
   Downstream            |                       |           Upstream
   Clients          +--->|  StreamableHttpService |           Servers
   (Claude, etc.)   |    |  (Tower/Axum)         |
                    |    |                       |    +---> Server A (stdio)
   HTTP POST/GET ---+--->|  FanoutHandler        |----+     TokioChildProcess
   (Streamable HTTP)     |  impl ServerHandler   |    |
                         |                       |    +---> Server B (HTTP)
                         |  - list_tools()  ---> |----+     StreamableHttpClient
                         |    fans out to all    |    |
                         |    merges results     |    +---> Server C (HTTP)
                         |                       |          StreamableHttpClient
                         |  - call_tool()  ----> |
                         |    routes to one      |
                         |    based on prefix    |
                         +-----------------------+
```

### Minimal Proxy Pseudocode

```rust
use rmcp::{ServerHandler, serve_server, serve_client, RoleServer, RoleClient};
use rmcp::model::*;
use rmcp::service::{RunningService, Peer, RequestContext};
use rmcp::transport::{StreamableHttpService, StreamableHttpClientTransport, TokioChildProcess};

struct FanoutHandler {
    /// Map of server_name -> running client connection
    upstreams: HashMap<String, RunningService<RoleClient, ()>>,
}

impl FanoutHandler {
    async fn connect_upstream_http(name: &str, url: &str) -> Result<Self> {
        let transport = StreamableHttpClientTransport::new(url);
        let client = ().serve(transport).await?;
        // Store in upstreams map...
    }

    async fn connect_upstream_stdio(name: &str, cmd: Command) -> Result<Self> {
        let transport = TokioChildProcess::new(cmd)?;
        let client = ().serve(transport).await?;
        // Store in upstreams map...
    }
}

impl ServerHandler for FanoutHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            name: "plug".into(),
            version: "0.1.0".into(),
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Fan out to all upstreams
        let mut all_tools = Vec::new();
        for (name, client) in &self.upstreams {
            let result = client.peer().list_tools(request.clone()).await?;
            for mut tool in result.tools {
                // Prefix tool name with server name for routing
                tool.name = format!("{}/{}", name, tool.name);
                all_tools.push(tool);
            }
        }
        Ok(ListToolsResult { tools: all_tools, next_cursor: None })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Parse prefix to route to correct upstream
        let (server_name, tool_name) = request.name
            .split_once('/')
            .ok_or_else(|| McpError::invalid_params("tool name must be prefixed", None))?;

        let client = self.upstreams.get(server_name)
            .ok_or_else(|| McpError::invalid_params("unknown server", None))?;

        let mut req = request;
        req.name = tool_name.to_string();
        client.peer().call_tool(req).await
    }

    // Similar patterns for list_prompts, get_prompt, list_resources, read_resource...
}

#[tokio::main]
async fn main() -> Result<()> {
    // Build the fanout handler with upstream connections
    let handler = FanoutHandler::new()
        .add_upstream_http("weather", "http://weather-server:8080/mcp")
        .add_upstream_stdio("filesystem", Command::new("mcp-server-filesystem"))
        .build()
        .await?;

    // Serve downstream via Streamable HTTP
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(SessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    // Mount in Axum
    let app = axum::Router::new()
        .route("/mcp", any(move |req| service.handle(req)));

    axum::serve(listener, app).await?;
    Ok(())
}
```

### Key Design Decisions

1. **Tool name prefixing:** Use `server_name/tool_name` format (same approach as AgentGateway)
2. **Fanout for list operations:** Broadcast to all upstreams, merge results
3. **Targeted routing for call operations:** Parse prefix, route to specific upstream
4. **Session management:** Use rmcp's built-in `SessionManager` for Streamable HTTP
5. **Error handling:** If one upstream fails during fanout, log and continue with partial results

---

## Risks and Open Questions

### Known Risks

1. **Protocol version gap:** rmcp 1.0.0 supports up to `V_2025_06_18`. The `2025-11-25` spec version constant is missing. This may cause version negotiation issues with clients expecting the latest spec version. **Mitigation:** Monitor rmcp releases; the SDK is actively maintained and likely to add the version soon.

2. **M2M client-credentials:** The OAuth module is comprehensive (PKCE, Dynamic Client Registration, scope management) but explicit `client_credentials` grant type support is unconfirmed. **Mitigation:** The `oauth2` crate dependency supports all grant types; rmcp's `AuthorizationManager` may need extension for M2M flows.

3. **CIMD full compliance:** URL-based client IDs are supported (SEP-991), but whether the full CIMD metadata document fetch-and-validate flow works needs testing. **Mitigation:** Can be implemented as a middleware layer.

4. **Legacy SSE backward compatibility:** rmcp does not provide a standalone legacy SSE transport. If plug needs to support older clients that only speak SSE, we would need to implement this ourselves (AgentGateway did this). **Mitigation:** AgentGateway's `LegacySSEService` is a reference implementation.

5. **`rmcp-proxy` crate is outdated:** The `rmcp-proxy` crate (v0.1.3, April 2025) depends on rmcp 0.1.5 and is not maintained to track rmcp 1.0.0. It should be used as a reference pattern only, not as a dependency. **Mitigation:** Implement our own `FanoutHandler` using the same pattern.

### Open Questions

1. **Task proxying:** How should the proxy handle Tasks (async operations)? Should it forward task handles transparently or manage its own task registry? AgentGateway does not appear to proxy Tasks yet.

2. **Notification forwarding:** When an upstream sends `notifications/tools/list_changed`, should the proxy re-fetch and notify downstream clients? AgentGateway handles this.

3. **Connection lifecycle:** Should upstream connections be per-session or shared? AgentGateway creates per-session upstream connections in its `Relay` struct.

4. **Capability merging:** When upstreams have different capabilities, how should the proxy advertise its own capabilities? AgentGateway takes the intersection (lowest common denominator for protocol version, union for features).

---

## Recommendation

**Proceed with rmcp 1.0.0 as the foundation for plug.**

The SDK provides:
- Proven dual-role (server + client) architecture
- Composable transport abstractions covering all needed protocols
- Production-grade prior art in AgentGateway (which uses rmcp for exactly this use case)
- Strong MCP spec coverage including Tasks, OAuth, Elicitation, and Extensions
- Active maintenance with the 1.0.0 milestone just reached

The primary reference implementation should be AgentGateway's `Relay` + `UpstreamGroup` architecture, adapted for plug's specific requirements. The `rmcp-proxy` crate's `ProxyHandler` pattern serves as a simpler reference for the basic bridge approach.

### Recommended Cargo.toml

```toml
[dependencies]
rmcp = { version = "1.0", features = [
    "client",
    "server",
    "macros",
    "base64",
    "transport-child-process",
    "transport-streamable-http-client",
    "transport-streamable-http-server",
    "auth",
] }
tokio = { version = "1", features = ["full"] }
axum = "0.8"
tower = "0.5"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```
