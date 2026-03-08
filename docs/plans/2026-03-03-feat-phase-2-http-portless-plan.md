---
title: "feat: Phase 2 — Streamable HTTP Server, HTTP Client"
type: feat
status: active
date: 2026-03-03
---

# feat: Phase 2 — Streamable HTTP Server, HTTP Client

> Historical planning note: This file is implementation history from 2026-03-03. It is not a
> current-state source of truth. Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for the
> current project state.

## Enhancement Summary

**Deepened on:** 2026-03-03
**Sections enhanced:** All sub-phases + architecture + error handling + config + dependencies
**Research agents used:** Security Sentinel, Architecture Strategist, Performance Oracle, Code Simplicity Reviewer, Pattern Recognition Specialist, MCP Spec Researcher, rmcp SDK Researcher, Axum SSE Best Practices Researcher

### Key Improvements

1. **Scope reduction**: Cut Legacy SSE (2D), HTTP/2+TLS (2E), defer .localhost routing (2B) to Phase 3 — ~40-50% LOC reduction
2. **Spec compliance**: Target MCP spec **2025-11-25** (not 2025-03-26) — adds `MCP-Protocol-Version` header (MUST), SSE priming events, polling pattern
3. **Separate `HttpError` enum**: HTTP-layer errors (403, 404, 400) stay as HTTP status codes — don't pollute `ProtocolError` (JSON-RPC error codes)
4. **ServerManager bottleneck fix**: Replace `RwLock<HashMap>` with `ArcSwap<HashMap<String, Arc<UpstreamServer>>>` — wait-free reads under HTTP concurrency
5. **Security hardening**: `RequestBodyLimitLayer`, `max_sessions` cap, `Secret<String>` for auth tokens, SSRF URL validation, Content-Type validation
6. **SSE production patterns**: `CancellationToken` for graceful shutdown (axum known issue), bounded channels with `try_send`, drop guard disconnect detection

### New Considerations Discovered

- rmcp `server_side_http` utilities (`sse_stream_response`, `expect_json`, `accepted_response`) are `pub(crate)` — NOT public API; must build our own
- rmcp header constant uses `"Mcp-Session-Id"` (older capitalization) vs spec `"MCP-Session-Id"` — HTTP headers are case-insensitive, use rmcp constant
- JSON-RPC batching removed in MCP spec 2025-06-18+ — POST body MUST be single message
- Need `transport-streamable-http-server` rmcp feature (plan was originally missing it)
- Must handle client responses (id+result, no method) in POST for sampling/elicitation support
- SSE resumption always via GET even for POST-initiated streams (fundamental to spec architecture)
- Open SSE connections block axum's graceful shutdown — CancellationToken required

### Scope Changes from Original Plan

| Sub-Phase | Status | Rationale |
|-----------|--------|-----------|
| 2A: Streamable HTTP Server | **KEEP** | Core value |
| 2B: .localhost Routing | **DEFER to Phase 3** | Adds complexity, not needed for MVP HTTP |
| 2C: HTTP Client | **KEEP** | Small, fills existing stub |
| 2D: Legacy SSE | **CUT** | Zero current users need it; session fixation risk; OpenCode supports Streamable HTTP |
| 2E: HTTP/2 + TLS | **CUT** | Localhost doesn't need TLS; premature optimization |
| 2F: CLI Updates | **KEEP** | Required for `plug serve` |
| 2G: Validation | **KEEP** | Reduced scope (remove 2B/2D/2E validation tasks) |

---

## Overview

Add HTTP transport to the plug MCP multiplexer, enabling web-based AI clients (Gemini CLI, OpenCode, browser-based tools) alongside the existing stdio bridge. This includes a Streamable HTTP server (MCP spec **2025-11-25**), an HTTP client for remote upstream MCP servers, and per-client session management.

Phase 1 (PR #1, merged) delivered a working stdio multiplexer. Phase 2 makes plug accessible over HTTP without breaking existing stdio functionality.

## Problem Statement / Motivation

**Current limitation**: plug only serves MCP over stdio. Clients must invoke `plug connect` as a child process. This blocks:

1. **Web-based clients** — Gemini CLI, OpenCode, and browser tools expect HTTP endpoints
2. **Remote access** — No way to connect to plug from a different machine or container
3. **Multi-client concurrency** — Each `plug connect` starts isolated upstream servers; they're not shared

**Phase 2 delivers**: HTTP transport that runs alongside stdio, sharing the same upstream server pool.

## Proposed Solution

### Architecture Decision: Custom Axum Handlers (Not rmcp's Built-In StreamableHttpService)

rmcp 1.0.0 provides `StreamableHttpService` — a tower Service that handles POST/GET/DELETE /mcp automatically. However, **plug is a proxy**, not a simple MCP server. Like AgentGateway (which also builds custom handlers), we need:

- Fan-out to multiple upstream servers per request
- Custom tool routing via `ArcSwap<ToolCache>`
- Per-client session tracking with tool filtering
- Shared `ServerManager` across stdio + HTTP transports

**Decision**: Build custom axum route handlers that use rmcp's model types (`ClientJsonRpcMessage`, `ServerJsonRpcMessage`, header constants) but implement our own request routing, session management, and SSE streaming.

**What we DO use from rmcp**:
- `transport-streamable-http-client-reqwest` — for connecting to remote upstream servers
- `transport-streamable-http-server` feature — for HTTP model types and SSE stream types
- `server-side-http` feature — for header constants and session ID generation
- All existing model types (`Tool`, `CallToolResult`, `InitializeResult`, etc.)

**What is NOT public API in rmcp** (must reimplement):
- `sse_stream_response()` — `pub(crate)`, build our own SSE response
- `expect_json()` — `pub(crate)`, use `axum::Json` extractor instead
- `accepted_response()` — `pub(crate)`, return `StatusCode::ACCEPTED` directly

### High-Level Component Diagram

```
                    ┌─────────────────────────────────────┐
                    │         plug process                 │
                    │                                      │
 stdio clients ──►  │  ┌───────────┐   ┌────────────────┐ │
 (plug connect)     │  │ stdio     │   │ ToolRouter     │ │ ──► upstream stdio servers
                    │  │ bridge    ├──►│ (Arc, shared)  │ │ ──► upstream HTTP servers
 HTTP clients  ──►  │  │           │   │                │ │
 (axum server)      │  │ axum HTTP ├──►│ ServerManager  │ │
                    │  │ server    │   │ (ArcSwap)      │ │
                    │  └───────────┘   └────────────────┘ │
                    │         │                            │
                    │  ┌──────┴──────┐                     │
                    │  │  Session    │                     │
                    │  │  Manager    │                     │
                    │  └─────────────┘                     │
                    └─────────────────────────────────────┘
```

## Technical Approach

### Pre-Implementation: ServerManager Performance Fix (CRITICAL)

**Before Phase 2 implementation**, fix the ServerManager concurrency bottleneck. The current `RwLock<HashMap>` serializes all concurrent tool calls — under HTTP concurrency this becomes a critical bottleneck.

**Current** (`plug-core/src/server/mod.rs`):
```rust
pub struct ServerManager {
    servers: Arc<RwLock<HashMap<String, UpstreamServer>>>,
}
```

**Replace with**:
```rust
pub struct ServerManager {
    servers: Arc<ArcSwap<HashMap<String, Arc<UpstreamServer>>>>,
}
```

This gives wait-free reads (same pattern as `ArcSwap<ToolCache>` already used in ProxyHandler).

Also wrap `ToolCache.tools` in `Arc<Vec<Tool>>` to avoid deep clones on every `list_tools` call:

```rust
struct ToolCache {
    routes: HashMap<String, String>,
    tools: Arc<Vec<Tool>>,  // was: Vec<Tool>
}
```

### Implementation Phases

#### Sub-Phase 2A: Streamable HTTP Server (Core)

**Files to create/modify:**

- `plug-core/src/http/mod.rs` — HTTP module root
- `plug-core/src/http/server.rs` — Axum router, handlers (POST/GET/DELETE /mcp)
- `plug-core/src/http/session.rs` — SessionManager with DashMap
- `plug-core/src/http/sse.rs` — SSE stream helpers (channel-based, disconnect detection)
- `plug-core/src/http/error.rs` — `HttpError` enum implementing axum `IntoResponse`
- `plug-core/src/lib.rs` — Add `pub mod http`
- `plug-core/src/config/mod.rs` — Add nested `HttpConfig` struct
- `plug/src/main.rs` — Add `plug serve` command
- `Cargo.toml` (workspace) — Add axum, tower-http, tokio-stream, async-stream, tokio-util dependencies

**Tasks:**

- [ ] **2A.1** Add workspace dependencies:
  ```toml
  axum = "0.8"
  tower-http = { version = "0.6", features = ["cors", "limit"] }
  tokio-stream = "0.1"
  async-stream = "0.3"
  tokio-util = { version = "0.7", features = ["rt"] }
  http = "1"
  bytes = "1"
  ```

- [ ] **2A.2** Add rmcp features:
  ```toml
  rmcp = { version = "1.0.0", features = [
      "client", "server", "macros", "schemars",
      "transport-io", "transport-child-process",
      # NEW for Phase 2:
      "transport-streamable-http-client",
      "transport-streamable-http-client-reqwest",
      "transport-streamable-http-server",
      "server-side-http",
  ] }
  ```

- [ ] **2A.3** Create `plug-core/src/http/error.rs` — **Separate `HttpError` enum** (NOT added to `ProtocolError`):
  ```rust
  /// HTTP-layer errors that map to HTTP status codes.
  /// Separate from ProtocolError which maps to JSON-RPC error codes.
  pub enum HttpError {
      InvalidOrigin(String),        // 403
      SessionRequired,              // 400
      SessionNotFound(String),      // 404
      InvalidContentType,           // 415
      InvalidAcceptHeader,          // 406
      BadRequest(String),           // 400
      TooManySessions,              // 429
      BodyTooLarge,                 // 413
  }

  impl IntoResponse for HttpError {
      fn into_response(self) -> axum::response::Response {
          // Map each variant to appropriate HTTP status code
          // IMPORTANT: Do NOT echo session_id in error responses (timing oracle)
      }
  }
  ```

- [ ] **2A.4** Create `plug-core/src/http/session.rs` — Minimal `SessionManager`:
  ```rust
  pub struct SessionManager {
      sessions: DashMap<String, Instant>,  // session_id → last_activity
      max_sessions: usize,
      timeout: Duration,
  }
  ```
  - `create_session()` → UUID v4 session ID via `rmcp::transport::common::session_id()`; return `Err(HttpError::TooManySessions)` if cap reached
  - `validate(session_id) -> bool` — check exists + not expired, update last_activity
  - `remove(session_id)` → cleanup
  - Background cleanup task (every 30s, evict expired sessions) with `MissedTickBehavior::Skip`

- [ ] **2A.5** Create `plug-core/src/http/sse.rs` — SSE stream with production patterns:
  ```rust
  /// Create an SSE stream from an mpsc receiver with disconnect detection.
  /// Uses async_stream drop guard for cleanup and CancellationToken for shutdown.
  pub fn sse_stream(
      rx: mpsc::Receiver<ServerJsonRpcMessage>,
      cancel: CancellationToken,
  ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
      let stream = async_stream::stream! {
          // SSE priming event (SHOULD per spec 2025-11-25)
          yield Ok(Event::default().id("0").data(""));

          let mut rx = ReceiverStream::new(rx);
          loop {
              tokio::select! {
                  biased;
                  _ = cancel.cancelled() => break,
                  msg = rx.next() => {
                      match msg {
                          Some(msg) => {
                              let data = serde_json::to_string(&msg).unwrap();
                              yield Ok(Event::default().data(data));
                          }
                          None => break, // sender dropped
                      }
                  }
              }
          }
          // Drop guard: cleanup SSE sender from session
      };
      Sse::new(stream).keep_alive(
          KeepAlive::new()
              .interval(Duration::from_secs(15))
              .text("")  // SSE comment, not event — won't confuse MCP clients
      )
  }
  ```

- [ ] **2A.6** Add Origin validation as axum middleware in `plug-core/src/http/server.rs`:
  - Accept missing Origin (non-browser MCP clients don't send it)
  - Accept `localhost:*`, `127.0.0.1:*`, `[::1]:*`
  - Reject `null` as literal string (DNS rebinding vector)
  - Reject all other Origins (external origins)
  - Validate Host header too (Host+port validation for DNS rebinding prevention)
  - Apply to ALL routes, not just /mcp

- [ ] **2A.7** Create `plug-core/src/http/server.rs` — the core HTTP server:
  - `build_router(state: Arc<HttpState>) -> Router`
  - `HttpState` struct (NOT `AppState` — too generic) holding `Arc<ToolRouter>`, `SessionManager`, `CancellationToken`
  - Apply `RequestBodyLimitLayer(4 * 1024 * 1024)` (4MB, DoS prevention)
  - Apply security headers: `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`
  - **POST /mcp handler**:
    1. Validate `Content-Type: application/json` (reject others with 415)
    2. Parse JSON-RPC message from body (single message only — batching removed in spec 2025-06-18+)
    3. Check `MCP-Protocol-Version` header on all requests (MUST per spec 2025-11-25)
    4. If `initialize` request → create session, process via ToolRouter, return JSON with `Mcp-Session-Id` header + `MCP-Protocol-Version` response header
    5. If request (has `id` + `method`) → validate session, route via ToolRouter, return JSON
    6. If client response (has `id` + `result`, no `method`) → validate session, process for sampling/elicitation, return 202
    7. If notification (no `id`) → validate session, process, return 202 Accepted
  - **GET /mcp handler**:
    1. Validate `Mcp-Session-Id` header
    2. Validate `Accept` header includes `text/event-stream`
    3. Create bounded mpsc channel (capacity: 32)
    4. Register sender in session's SSE senders
    5. Return `Sse<impl Stream>` via `sse_stream()` helper
    6. Add `X-Accel-Buffering: no` header (reverse proxy compatibility)
    7. Send priming event (empty data + event ID) as first event
  - **DELETE /mcp handler**:
    1. Validate `Mcp-Session-Id` header
    2. Remove session, close SSE channels
    3. Return 200 OK or 404 Not Found

- [ ] **2A.8** Extract `ToolRouter` from `ProxyHandler` for shared use:
  - `ToolRouter` holds `Arc<ArcSwap<ToolCache>>` + `Arc<ServerManager>` + prefix config
  - `ProxyHandler` wraps `Arc<ToolRouter>` for stdio (implements `ServerHandler`)
  - HTTP handlers use `Arc<ToolRouter>` directly via axum State
  - Keep `refresh_tools()` on `ToolRouter` (SRP — separate from routing logic)
  - `ToolRouter` methods: `list_tools()`, `call_tool(name, args)`, `refresh_tools()`
  - `list_tools()` returns `Arc<Vec<Tool>>` (no clone needed)

- [ ] **2A.9** Add `plug serve` command to `plug/src/main.rs`:
  - Load config, start ServerManager, build ToolRouter (shared), refresh tools
  - Start axum server on `config.http.bind_address:config.http.port`
  - CancellationToken for coordinated shutdown of HTTP server + SSE streams
  - Handle SIGINT/SIGTERM: cancel token → axum graceful_shutdown → wait for SSE drain → shutdown ServerManager
  - Log: "HTTP server listening on http://127.0.0.1:3282"
  - Warn if `config.http.bind_address` is not loopback

- [ ] **2A.10** Tests:
  - Unit tests for SessionManager (create, validate, expire, cleanup, max_sessions cap → 429)
  - Unit tests for Origin validation (missing OK, localhost OK, null rejected, external rejected)
  - Unit tests for HttpError → HTTP status code mapping
  - Integration test: POST /mcp with initialize request → get session ID + MCP-Protocol-Version header
  - Integration test: POST /mcp with tools/list → get tool list
  - Integration test: DELETE /mcp → session removed
  - Integration test: GET /mcp → SSE stream opens with priming event
  - Integration test: POST without Content-Type: application/json → 415
  - Integration test: exceed max_sessions → 429

#### Sub-Phase 2C: Streamable HTTP Client (Remote Upstreams)

**Files to create/modify:**

- `plug-core/src/server/mod.rs` — Add HTTP transport handling in `start_server`
- `plug-core/src/config/mod.rs` — Add `auth_token` with env var expansion + SSRF validation

**Tasks:**

- [ ] **2C.1** Implement `TransportType::Http` branch in `ServerManager::start_server`:
  ```rust
  TransportType::Http => {
      let url = config.url.as_deref()
          .ok_or_else(|| anyhow!("HTTP transport requires url"))?;

      // SSRF validation: only allow http:// and https:// schemes
      // Reject file://, ftp://, metadata IPs (169.254.169.254, etc.)
      validate_upstream_url(url)?;

      let mut transport_config = StreamableHttpClientTransportConfig::default();
      if let Some(token) = &config.auth_token {
          transport_config = transport_config.with_auth_header(
              format!("Bearer {}", token)
          );
      }

      let transport = StreamableHttpClientTransport::from_config(url, transport_config);
      let client: McpClient = ().serve(transport)
          .await
          .map_err(|e| anyhow!("failed to connect: {e}"))?;

      let tools = client.peer().list_all_tools().await?;

      Ok(UpstreamServer { name, config, client, tools, health: ServerHealth::Healthy })
  }
  ```

- [ ] **2C.2** Add `auth_token` to `ServerConfig` with `Secret<String>` wrapper:
  - `auth_token: Option<Secret<String>>` — redacts on Debug/Display to prevent log leakage
  - Add auth_token to env-var expansion loop in `load_config()` (Phase 1 missed this)
  - Share single `reqwest::Client` across all HTTP upstream connections (connection pooling)

- [ ] **2C.3** Add `validate_upstream_url(url: &str) -> Result<()>`:
  - Only allow `http://` and `https://` schemes
  - Reject metadata IPs: `169.254.169.254`, `fd00::`, link-local ranges
  - Reject `localhost`/`127.0.0.1` pointing back to self (loop prevention)

- [ ] **2C.4** Tests:
  - Start a mock Streamable HTTP MCP server (use rmcp's StreamableHttpService)
  - Connect to it via ServerManager with `transport = "http"` config
  - Verify tool discovery works
  - Verify tool calls route correctly
  - SSRF validation: reject `file://`, reject metadata IPs, accept valid HTTPS URLs

#### Sub-Phase 2F: CLI Updates & `plug serve` Command

**Files to modify:**

- `plug/src/main.rs` — Add `serve` command, update `connect`

**Tasks:**

- [ ] **2F.1** Add `Commands::Serve` variant:
  ```rust
  /// Start the HTTP server (and optionally stdio bridge)
  Serve {
      /// Also start stdio bridge on stdin/stdout
      #[arg(long)]
      stdio: bool,
  }
  ```

- [ ] **2F.2** Implement `cmd_serve`:
  - Load config, validate
  - Start ServerManager (with ArcSwap migration), build ToolRouter (shared), refresh tools
  - Build and refresh ProxyHandler (wrapping ToolRouter) — used only if `--stdio`
  - Start axum HTTP server with CancellationToken
  - If `--stdio`: also start stdio bridge on stdin/stdout (parallel with HTTP via `tokio::select!`)
  - Shutdown coordination: SIGINT → cancel token → HTTP graceful shutdown → wait for SSE drain → ServerManager shutdown
  - Wait for shutdown signal

- [ ] **2F.3** Update `plug status` to work with running HTTP server:
  - If HTTP server is running, query it
  - Otherwise, start servers locally (current behavior)

#### Sub-Phase 2G: Validation

**Tasks:**

- [ ] **2G.1** Connect Claude Code via stdio while HTTP server runs → verify both work simultaneously
- [ ] **2G.2** Connect via HTTP POST /mcp → verify tools/list returns merged tools with MCP-Protocol-Version header
- [ ] **2G.3** Call a tool via HTTP POST /mcp → verify tool call routes correctly
- [ ] **2G.4** Multiple HTTP clients simultaneously → verify session isolation
- [ ] **2G.5** GET /mcp SSE stream → verify priming event + server notifications forwarded
- [ ] **2G.6** DELETE /mcp → verify session cleanup
- [ ] **2G.7** Kill one upstream server → verify HTTP clients see remaining servers' tools
- [ ] **2G.8** Exceed max_sessions → verify 429 response
- [ ] **2G.9** POST with invalid Content-Type → verify 415 response
- [ ] **2G.10** POST with external Origin → verify 403 response

## System-Wide Impact

### Interaction Graph

1. HTTP request arrives → `RequestBodyLimitLayer` → `validate_origin` middleware → axum handler
2. Handler validates Content-Type → parses JSON-RPC → checks MCP-Protocol-Version → validates session (SessionManager)
3. Request routes to ToolRouter → ArcSwap<ToolCache> lookup → ServerManager (ArcSwap) → upstream peer.call_tool
4. Response flows back: upstream → ToolRouter → handler → HTTP response (JSON or SSE)
5. For SSE GET: handler → bounded mpsc(32) channel → SSE stream → client. Server notifications → try_send on channel (drop if full — backpressure)

### Error & Failure Propagation

| Error Source | Error Type | Handler Response |
|-------------|-----------|-----------------|
| Body too large | `HttpError::BodyTooLarge` | 413 Payload Too Large |
| Invalid Origin | `HttpError::InvalidOrigin` | 403 Forbidden |
| Invalid Content-Type | `HttpError::InvalidContentType` | 415 Unsupported Media Type |
| Missing session ID | `HttpError::SessionRequired` | 400 Bad Request |
| Expired/unknown session | `HttpError::SessionNotFound` | 404 Not Found |
| Too many sessions | `HttpError::TooManySessions` | 429 Too Many Requests |
| Invalid JSON body | `HttpError::BadRequest` | 400 Bad Request |
| Tool not found | JSON-RPC error (-32601) | 200 OK with error body |
| Upstream timeout | JSON-RPC error (-32000) | 200 OK with error body |
| Upstream crash | JSON-RPC error (-32000) | 200 OK with error body |
| SSE disconnect | Channel dropped | Drop guard cleans up session SSE sender |

### State Lifecycle Risks

1. **Session without SSE cleanup**: If HTTP client disconnects without DELETE, session lingers until timeout. Mitigation: 30-minute default timeout + background cleanup task with `MissedTickBehavior::Skip`.
2. **Stale tool cache after upstream restart**: Same as Phase 1 — ArcSwap ensures readers see consistent snapshot, refresh rebuilds cache.
3. **Partial initialization**: If `plug serve` starts HTTP server but some upstreams fail, HTTP clients see only available servers' tools. This is correct behavior.
4. **SSE channel full**: Bounded channel (32) with `try_send` — if client is slow, messages are dropped. This prevents one slow client from blocking the entire MCP pipeline.
5. **Open SSE blocks shutdown**: CancellationToken signals all SSE streams to close. `tokio::select! { biased; }` ensures shutdown signal takes priority.

### API Surface Parity

| Operation | stdio (Phase 1) | HTTP POST /mcp (Phase 2) |
|-----------|-----------------|--------------------------|
| initialize | via rmcp | Custom handler |
| tools/list | ServerHandler | Custom handler |
| tools/call | ServerHandler | Custom handler |
| resources/list | ServerHandler | Custom handler |
| notifications | rmcp transport | SSE GET stream |

### Integration Test Scenarios

1. **Stdio + HTTP simultaneous**: Start `plug serve --stdio`, connect via stdio AND HTTP, call same tool from both → both succeed, responses match
2. **Session expiry mid-stream**: Open SSE GET, wait for timeout, try POST → should get 404, re-initialize
3. **Upstream crash during HTTP call**: HTTP client calls tool, upstream dies mid-call → clean JSON-RPC error, not HTTP 500
4. **Concurrent session cleanup**: Many sessions expire simultaneously → cleanup task handles without deadlock (DashMap is shard-level concurrent)
5. **Graceful shutdown with open SSE**: SIGINT during active SSE streams → CancellationToken fires → streams close cleanly → server exits

## SpecFlow Analysis — Key Gaps Identified

A comprehensive flow analysis identified 32 gaps and 13 critical questions. The key findings incorporated into this plan:

### Resolved Decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Q1: How does ProxyHandler serve multiple HTTP sessions? | Extract `ToolRouter` shared via `Arc`, HTTP handlers use it directly (not via ServerHandler trait) | rmcp's `ServerHandler` is designed for single-client; HTTP needs shared routing without per-session trait instances |
| Q2: Origin validation rules | Accept missing Origin (non-browser), accept `localhost:*`, `127.0.0.1:*`; reject `null` and external Origins | Non-browser MCP clients (majority) don't send Origin; be permissive for localhost |
| Q4: Build vs use rmcp StreamableHttpService | Build custom axum handlers | Proxy needs fan-out, custom session mgmt — rmcp's built-in service assumes simple server |
| Q5: POST response format (JSON vs SSE) | Return JSON by default | Simplicity first; SSE only on GET /mcp |
| Q6: Upstream session sharing | Shared upstream sessions (1 per server) for HTTP clients | NxM sessions would create hundreds of connections; session isolation is Phase 3 |
| Q11: HTTP server lifecycle | New `plug serve` command; `plug connect` stays stdio-only | Clear separation of concerns |
| Q-new: MCP spec version | Target 2025-11-25 (not 2025-03-26) | Latest stable spec; adds MCP-Protocol-Version header (MUST), SSE priming |
| Q-new: Legacy SSE | Cut from Phase 2 | Zero current users; OpenCode supports Streamable HTTP; session fixation risk |
| Q-new: HTTP/2 + TLS | Cut from Phase 2 | Localhost doesn't need TLS; premature optimization |
| Q-new: .localhost routing | Defer to Phase 3 | Adds complexity; not needed for MVP HTTP transport |

### Gaps Addressed in Implementation Tasks

| Gap | Addressed In |
|-----|-------------|
| Gap 1-2: Session-to-ProxyHandler binding | Task 2A.8 (ToolRouter extraction) |
| Gap 5: Accept header negotiation | Task 2A.7 (POST handler — JSON response) |
| Gap 9: Origin validation rules | Task 2A.6 (middleware) |
| Gap 14: Server name DNS validation | Deferred with 2B to Phase 3 |
| Gap 16: HTTP upstream auth | Task 2C.2 (auth_token field) |
| Gap 22: HTTP error codes | Task 2A.3 (separate HttpError enum) |
| Gap 29: Missing config fields | Config Additions section below |
| Gap 30: Port conflict | axum bind error is descriptive; no special handling needed |

### Deferred to Phase 3

| Gap | Why Deferred |
|-----|-------------|
| Gap 20: Per-client tool filtering | Phase 3 scope (client-aware tool filtering) |
| Gap 25: Rate limiting | Phase 3 scope (resilience) |
| Gap 31-32: Notification broadcast | Phase 3 scope (notification forwarding) |
| .localhost subdomain routing | Phase 3 scope (adds complexity without MVP value) |
| Last-Event-ID SSE resumability | Phase 3 scope (complex, needs event store) |

### Config Additions Required

Add nested `HttpConfig` struct to `Config`:

```rust
#[derive(Debug, Deserialize)]
pub struct HttpConfig {
    pub port: u16,                     // default: 3282
    pub bind_address: String,          // default: "127.0.0.1"
    pub session_timeout_secs: u64,     // default: 1800 (30 min)
    pub max_sessions: usize,           // default: 100
    pub sse_channel_capacity: usize,   // default: 32
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            port: 3282,
            bind_address: "127.0.0.1".to_string(),
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: 32,
        }
    }
}
```

Config in TOML:
```toml
[http]
port = 3282
bind_address = "127.0.0.1"
session_timeout_secs = 1800
max_sessions = 100
```

Env var access via Figment `__` delimiter: `PLUG__HTTP__PORT=8080`

Add to `ServerConfig`:

```rust
pub auth_token: Option<Secret<String>>,  // Bearer token for HTTP upstreams
```

### Additional Validation Rules

Add to `validate_config()`:
- Warn on non-loopback `http.bind_address` (security)
- Validate `http.port` is in valid range (1-65535)
- Validate HTTP upstream URLs (SSRF prevention: scheme, no metadata IPs)

### HTTP-Specific Error Types

**Separate `HttpError` enum** (in `plug-core/src/http/error.rs`) — does NOT modify `ProtocolError`:

```rust
/// HTTP-layer errors implementing axum's IntoResponse.
/// These map to HTTP status codes, NOT JSON-RPC error codes.
pub enum HttpError {
    InvalidOrigin(String),     // → 403 Forbidden
    SessionRequired,           // → 400 Bad Request
    SessionNotFound(String),   // → 404 Not Found (DO NOT echo session_id in body)
    InvalidContentType,        // → 415 Unsupported Media Type
    InvalidAcceptHeader,       // → 406 Not Acceptable
    BadRequest(String),        // → 400 Bad Request
    TooManySessions,           // → 429 Too Many Requests
    BodyTooLarge,              // → 413 Payload Too Large
}
```

**Security note**: `SessionNotFound` must NOT include the session_id in the error response body — this prevents timing oracle attacks.

## Dependencies & New Crates

### Workspace Dependency Additions

```toml
# HTTP Server
axum = "0.8"
tower-http = { version = "0.6", features = ["cors", "limit"] }
tokio-stream = "0.1"
async-stream = "0.3"
tokio-util = { version = "0.7", features = ["rt"] }
http = "1"
bytes = "1"
```

### rmcp Feature Additions

```toml
rmcp = { version = "1.0.0", features = [
    "client", "server", "macros", "schemars",
    "transport-io", "transport-child-process",
    # NEW for Phase 2:
    "transport-streamable-http-client",
    "transport-streamable-http-client-reqwest",
    "transport-streamable-http-server",
    "server-side-http",
] }
```

## Acceptance Criteria

### Functional Requirements

- [ ] `plug serve` starts HTTP server on configurable port (default 3282)
- [ ] POST /mcp accepts JSON-RPC requests and returns JSON responses
- [ ] POST /mcp returns `MCP-Protocol-Version` header on initialize response
- [ ] POST /mcp handles client responses (id+result, no method) for sampling/elicitation
- [ ] GET /mcp opens SSE stream with priming event for server-initiated notifications
- [ ] DELETE /mcp terminates session
- [ ] `Mcp-Session-Id` header generated on initialize, required on subsequent requests
- [ ] Origin header validated (localhost only, reject external origins, reject `null`)
- [ ] Content-Type validated on POST (must be application/json)
- [ ] HTTP transport config `transport = "http"` + `url` connects to remote MCP servers
- [ ] stdio (`plug connect`) and HTTP (`plug serve`) can run simultaneously sharing upstreams
- [ ] `plug serve --stdio` runs both transports in single process

### Non-Functional Requirements

- [ ] HTTP request overhead < 10ms for cached tool routes
- [ ] Support 100+ concurrent HTTP sessions (configurable max_sessions cap)
- [ ] Session cleanup runs every 30s with `MissedTickBehavior::Skip`
- [ ] Request body limited to 4MB (DoS prevention via `RequestBodyLimitLayer`)
- [ ] No `unsafe` code (`#![forbid(unsafe_code)]`)
- [ ] All new code has unit tests
- [ ] Integration tests cover all HTTP endpoints
- [ ] Bind to 127.0.0.1 by default (security)
- [ ] Graceful shutdown: CancellationToken → SSE streams close → server exits cleanly
- [ ] Security response headers: `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`

### Quality Gates

- [ ] `cargo check` passes with no warnings
- [ ] `cargo clippy` passes with no warnings
- [ ] `cargo test` — all tests pass
- [ ] No new dependencies with known security advisories
- [ ] Code review approval

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| rmcp `server-side-http` utility functions are `pub(crate)` | Confirmed | Medium | Build our own SSE response helpers, JSON extraction; small amount of code |
| DashMap ref held across await in session code | Medium | High | Apply Phase 1 lesson: clone-and-drop pattern, never hold ref across await |
| axum 0.8 breaking changes from 0.7 docs/examples | Low | Low | Use official axum 0.8 examples as reference |
| Session timeout too aggressive for slow clients | Low | Medium | Make configurable, default 30 min is generous |
| Open SSE connections block graceful shutdown | Confirmed (known axum issue) | High | CancellationToken + `tokio::select! { biased; }` ensures shutdown signal priority |
| SSRF via HTTP upstream URLs | Medium | High | validate_upstream_url() rejects non-http schemes and metadata IPs |
| Session creation DoS | Medium | Medium | max_sessions cap with 429 response |

## Implementation Order

Recommended order to minimize risk and enable incremental validation:

1. **Pre-2A** ServerManager ArcSwap migration + ToolCache Arc<Vec<Tool>> (unblocks concurrent HTTP)
2. **2A** (Streamable HTTP Server) — core value, largest chunk
3. **2C** (HTTP Client) — small, mostly filling in existing `TransportType::Http` stub
4. **2F** (CLI updates) — `plug serve` command to tie it together
5. **2G** (Validation) — runs throughout, final comprehensive check

## Key Patterns from Phase 1 to Follow

From `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md`:

1. **Non-exhaustive structs**: Use builder methods (`::new()`, `.with_*()`, `::success()`)
2. **Module re-exports**: Import from `rmcp::` root, not internal modules
3. **Atomic cache**: Keep related data in single `ArcSwap` struct
4. **Async lock safety**: Clone what you need, drop guard before `.await`
5. **Figment delimiter**: Use `__` for env var nesting

## MCP Spec 2025-11-25 Compliance Checklist

| Requirement | Spec Section | Implementation |
|------------|-------------|----------------|
| MCP-Protocol-Version header on requests | Transport | POST handler validates + returns in initialize response |
| Single JSON-RPC message per POST (no batching) | Transport | Parse as single message, not array |
| SSE priming event (empty data + event ID) | Transport | First event in GET /mcp SSE stream |
| Client responses (id+result, no method) | Transport | POST handler recognizes and routes for sampling/elicitation |
| SSE keep-alive as comments | Transport | `KeepAlive::text("")` sends SSE comment |
| No-broadcast rule for multiple SSE streams | Transport | Route each message to exactly ONE stream (single sender per session for now) |
| X-Accel-Buffering: no on SSE | Transport | Header added to GET /mcp response |

## Sources & References

### Internal References

- `plug-core/src/proxy/mod.rs` — ProxyHandler with ArcSwap<ToolCache>
- `plug-core/src/server/mod.rs` — ServerManager with TransportType::Http stub
- `plug-core/src/config/mod.rs` — Config with bind_address, port, TransportType
- `docs/PLAN.md` — Phase 2 requirements (sections 2.1-2.7)
- `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md` — Phase 1 learnings

### External References

- [MCP Specification 2025-11-25 — Transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [AgentGateway — Rust MCP proxy reference](https://github.com/agentgateway/agentgateway)
- [rmcp SDK — Rust MCP implementation](https://github.com/modelcontextprotocol/rust-sdk)
- [Axum SSE example](https://github.com/tokio-rs/axum/blob/main/examples/sse/src/main.rs)
- [DNS rebinding attacks on MCP servers](https://www.straiker.ai/blog/agentic-danger-dns-rebinding-exposing-your-internal-mcp-servers)
- [Shuttle: Streamable HTTP MCP in Rust](https://www.shuttle.dev/blog/2025/10/29/stream-http-mcp)

### Related Work

- PR #1: Phase 1 — Core stdio multiplexer (merged)
