---
title: "Phase 2 HTTP Transport Implementation for MCP Multiplexer (plug)"
category: integration-issues
tags:
  - rust
  - rmcp
  - arcswap
  - axum
  - mcp
  - http-transport
  - ssrf
  - sse
  - dashmap
  - cancellation-token
  - origin-validation
  - session-management
  - arc-try-unwrap
  - streamable-http
module: plug-core
date: 2026-03-03
symptom: "Phase 1 stdio-only MCP multiplexer needed HTTP transport support for web-based clients and remote upstream servers"
root_cause: "New feature implementation requiring concurrent-safe architecture migration (RwLock to ArcSwap), transport-agnostic routing extraction, axum HTTP handlers with MCP session lifecycle, security hardening (SSRF/Origin), and SSE streaming"
severity: high
resolution_time: "~6 hours across 3 sessions"
related:
  - docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md
  - docs/PLAN.md
  - docs/ARCHITECTURE.md
pr: "https://github.com/cyberpapiii/plug/pull/2"
---

# Phase 2: HTTP Transport Implementation for plug MCP Multiplexer

## Problem

Phase 1 of plug delivered a working stdio-only MCP multiplexer. Phase 2 required adding HTTP transport support for both inbound (web clients connecting to plug) and outbound (plug connecting to remote upstream MCP servers). This involved significant architectural changes to support concurrent HTTP requests, session management, security hardening, and SSE streaming.

## Key Challenges

1. **Concurrent access**: stdio is single-client; HTTP serves many clients simultaneously. `RwLock<ToolCache>` caused contention.
2. **Transport-agnostic routing**: Tool routing logic was coupled to `ProxyHandler` (rmcp `ServerHandler`), but HTTP handlers needed the same logic.
3. **rmcp SDK API**: Non-exhaustive structs, builder patterns, and `ServiceExt` trait require specific import and usage patterns.
4. **Security**: Origin header validation for localhost-only access, SSRF protection for upstream HTTP connections.
5. **Session lifecycle**: MCP Streamable HTTP requires session tracking with `MCP-Session-Id` headers.
6. **Graceful shutdown**: `CancellationToken` ownership for `'static` futures in axum, `Arc::try_unwrap` for clean server shutdown.

## Solutions

### 1. ArcSwap Migration for Wait-Free Concurrent Reads

**Problem**: `RwLock<HashMap<String, Arc<UpstreamServer>>>` caused read contention under HTTP concurrency.

**Solution**: Migrate to `ArcSwap` for wait-free reads. Writes (server start/stop) are infrequent and use `store()`.

```rust
use arc_swap::ArcSwap;

pub struct ServerManager {
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            servers: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    pub fn get_upstream(&self, name: &str) -> Option<Arc<UpstreamServer>> {
        let servers = self.servers.load(); // wait-free
        servers.get(name).cloned()
    }

    // Write path: clone-modify-swap
    fn insert_server(&self, name: String, server: Arc<UpstreamServer>) {
        let mut new_map = HashMap::clone(&self.servers.load());
        new_map.insert(name, server);
        self.servers.store(Arc::new(new_map));
    }
}
```

**Key insight**: `ArcSwap::load()` returns a `Guard` that derefs to the inner `Arc`. No lock contention, no reader starvation. Critical for HTTP where multiple requests resolve tools simultaneously.

### 2. ToolRouter Extraction for Transport-Agnostic Routing

**Problem**: `ProxyHandler` implemented `rmcp::ServerHandler` for stdio, but HTTP handlers needed the same tool routing logic (prefixing, lookup, call forwarding).

**Solution**: Extract shared routing into `ToolRouter`, let both transports reference it via `Arc<ToolRouter>`.

```rust
pub struct ToolRouter {
    server_manager: Arc<ServerManager>,
    tool_cache: ArcSwap<ToolCache>,
    prefix_delimiter: String,
}

// ProxyHandler wraps ToolRouter for stdio (implements ServerHandler)
pub struct ProxyHandler {
    router: Arc<ToolRouter>,
}

impl ProxyHandler {
    pub fn from_router(router: Arc<ToolRouter>) -> Self {
        Self { router }
    }
}

// HTTP handlers use ToolRouter directly via Arc
pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: SessionManager,
    pub cancel: CancellationToken,
    pub sse_channel_capacity: usize,
}
```

### 3. StreamableHttpClientTransport for Remote Upstreams

**Problem**: Need to connect to remote MCP servers over HTTP, not just local stdio processes.

**Solution**: Use rmcp's `StreamableHttpClientTransport` with `StreamableHttpClientTransportConfig`.

```rust
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

TransportType::Http => {
    let url = config.url.as_deref()
        .ok_or_else(|| anyhow::anyhow!("HTTP transport requires a URL"))?;

    // SSRF protection first
    let parsed = url.parse::<http::Uri>()?;
    if let Some(host) = parsed.host() {
        if is_blocked_host(host) {
            anyhow::bail!("URL host '{host}' is blocked");
        }
    }

    let mut transport_config =
        StreamableHttpClientTransportConfig::with_uri(url);

    if let Some(ref token) = config.auth_token {
        transport_config =
            transport_config.auth_header(format!("Bearer {token}"));
    }

    let transport =
        StreamableHttpClientTransport::from_config(transport_config);

    let client: McpClient = ().serve(transport).await?;
    let tools = client.peer().list_all_tools().await?;
    // ...
}
```

**Key API patterns**:
- `StreamableHttpClientTransportConfig::with_uri(url)` — constructor takes `&str`
- `.auth_header(format!("Bearer {token}"))` — builder method for auth
- `StreamableHttpClientTransport::from_config(config)` — creates transport from config
- Same `().serve(transport).await` pattern as stdio transport

### 4. Origin Header Validation (Security Fix)

**Problem**: Initial implementation used `origin.starts_with("http://localhost")` which allows bypass via `http://localhost.evil.com`.

**Solution**: Parse the Origin header properly and compare the host component exactly.

```rust
fn extract_origin_host(origin: &str) -> Option<&str> {
    let after_scheme = origin.split("://").nth(1)?;
    let host = after_scheme.split(':').next().unwrap_or(after_scheme)
        .split('/').next().unwrap_or(after_scheme);
    if host.is_empty() { None } else { Some(host) }
}

fn validate_origin(origin: &str) -> bool {
    let is_local = if let Some(host) = extract_origin_host(origin) {
        host == "localhost" || host == "127.0.0.1"
            || host == "[::1]" || host == "::1"
    } else {
        false
    };
    is_local
}
```

**Prevention rule (ORIGIN-01)**: Never use `starts_with()` or `contains()` for security-sensitive host matching. Always parse the URL and compare the host component exactly.

### 5. SSRF Protection for Upstream HTTP Connections

**Problem**: Initial blocklist only checked 2 specific IPs. Needed comprehensive RFC 1918, loopback, link-local, and metadata endpoint blocking.

**Solution**: Use Rust's `std::net::IpAddr` methods for comprehensive blocking.

```rust
fn is_blocked_host(host: &str) -> bool {
    if host == "metadata.google.internal" {
        return true;
    }
    let host_trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_trimmed.parse::<std::net::IpAddr>() {
        return is_blocked_ip(&ip);
    }
    false // Non-IP hostnames pass (DNS-based bypasses need connect-time checks)
}

fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()       // 127.0.0.0/8
                || v4.is_private() // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local() // 169.254.0.0/16 (covers cloud metadata)
                || v4.is_broadcast()  // 255.255.255.255
                || v4.is_unspecified() // 0.0.0.0
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()          // ::1
                || v6.is_unspecified() // ::
        }
    }
}
```

**Prevention rule (SSRF-01)**: Never maintain a manual IP blocklist. Use `std::net::IpAddr` methods which are comprehensive and maintained. Note: DNS-based bypasses (hostname resolving to private IP) require async DNS resolution at connect time.

### 6. SSE Streaming with CancellationToken

**Problem**: Need to stream MCP responses as SSE events with graceful shutdown support.

**Solution**: Use `async_stream::stream!` with `CancellationToken` and `biased` select.

```rust
pub fn sse_stream(
    rx: mpsc::Receiver<serde_json::Value>,
    cancel: CancellationToken,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        // SSE priming event (SHOULD per MCP spec 2025-11-25)
        yield Ok(Event::default().id("0").data(""));

        let mut rx = ReceiverStream::new(rx);
        let mut event_id: u64 = 1;
        loop {
            tokio::select! {
                biased; // Prioritize shutdown over messages
                _ = cancel.cancelled() => break,
                msg = rx.next() => {
                    match msg {
                        Some(msg) => {
                            let data = serde_json::to_string(&msg)?;
                            yield Ok(Event::default()
                                .id(event_id.to_string())
                                .data(data));
                            event_id += 1;
                        }
                        None => break, // sender dropped
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text(""), // SSE comment, not event
    )
}
```

**Key patterns**:
- **Priming event**: Empty data with `id: 0` so clients know the stream is alive (SHOULD per MCP spec)
- **`biased` select**: Ensures cancellation is checked before messages, preventing message processing after shutdown
- **KeepAlive with `.text("")`**: Sends SSE comments (`: \n\n`), not events — won't confuse MCP clients expecting only JSON-RPC events
- **Incrementing event IDs**: Start at 1 (after priming at 0) for client-side resumability

### 7. CancellationToken Ownership for axum Graceful Shutdown

**Problem**: `axum::serve().with_graceful_shutdown()` requires a `Future + 'static`, but `cancel.cancelled()` borrows the token.

**Solution**: Use `.clone().cancelled_owned()` to get an owned future.

```rust
let cancel = CancellationToken::new();

// WRONG: cancel.cancelled() borrows &self
// axum::serve(listener, router).with_graceful_shutdown(cancel.cancelled())

// RIGHT: clone + cancelled_owned() gives 'static future
axum::serve(listener, router)
    .with_graceful_shutdown(cancel.clone().cancelled_owned())
```

**Prevention rule (ASYNC-01)**: When passing `CancellationToken` futures to `'static` contexts (spawned tasks, axum shutdown), always use `.clone().cancelled_owned()`.

### 8. Arc::try_unwrap for Clean Server Shutdown

**Problem**: Original `shutdown_all` cloned the `Arc<HashMap>`, iterated, and dropped — but dropping an `Arc` clone doesn't shutdown the server if other references exist.

**Solution**: Swap in empty map, then `Arc::try_unwrap` to take ownership.

```rust
pub async fn shutdown_all(&self) {
    // Swap in empty map — no new code can access servers after this
    let old = self.servers.swap(Arc::new(HashMap::new()));

    let map = match Arc::try_unwrap(old) {
        Ok(map) => map,
        Err(arc) => {
            tracing::warn!("other references exist; dropping");
            drop(arc);
            return;
        }
    };

    for (name, upstream_arc) in map {
        match Arc::try_unwrap(upstream_arc) {
            Ok(upstream) => {
                // Sole owner — rmcp's Drop handles shutdown
                drop(upstream);
            }
            Err(arc) => {
                tracing::warn!(server = %name, "relying on Drop");
                drop(arc);
            }
        }
    }
}
```

**Prevention rule (ARC-01)**: When shutting down shared resources behind `Arc`, don't just drop a clone. Use `Arc::try_unwrap` to attempt sole ownership. If that fails, the resource is still in use — log a warning and let `Drop` handle it.

## Prevention Strategies

### Security Checklist for HTTP Endpoints

1. **Origin validation**: Parse host from URL, compare exactly — never `starts_with`/`contains`
2. **SSRF protection**: Use `std::net::IpAddr` methods for IP classification, block metadata endpoints by hostname
3. **Error messages**: Never leak internal details (serde errors, stack traces) in HTTP responses — log at debug, return generic message
4. **Session management**: UUID v4 session IDs, configurable timeouts, max session caps, background cleanup

### ArcSwap Decision Guide

| Scenario | Use |
|----------|-----|
| Many readers, rare writers | `ArcSwap` |
| Frequent writes, few readers | `RwLock` |
| Single writer, many readers | `ArcSwap` |
| Need mutable access to inner data | `RwLock` or `Mutex` |

### rmcp SDK Usage Patterns

1. **Import `ServiceExt`**: `use rmcp::ServiceExt as _;` — the `_` avoids name conflicts
2. **Non-exhaustive structs**: Use builders, not struct literals (e.g., `ErrorData::new()`)
3. **`list_all_tools()`**: Returns `Vec<Tool>`, handles pagination internally
4. **`peer().peer_info()`**: Returns `Option<&ServerInfo>` for server metadata

## Test Coverage

- 4 SSRF unit tests: loopback, private ranges, link-local/metadata, public IPs
- 3 SSE tests: priming event, message forwarding with incrementing IDs, cancellation
- 4 HTTP handler tests: session lifecycle, notification without session, notification with session, server info
- 1 Origin validation test: subdomain bypass rejection
- 57 existing unit tests + 13 integration tests = 70 total

## Architecture Diagram

```
                    +---------+
                    |  Client |  (stdio or HTTP)
                    +----+----+
                         |
              +----------+----------+
              |                     |
     ProxyHandler            axum HTTP handlers
     (rmcp ServerHandler)    (POST/GET/DELETE /mcp)
              |                     |
              +----------+----------+
                         |
                    ToolRouter (Arc<ToolRouter>)
                    - ArcSwap<ToolCache>
                    - prefix/unprefix
                    - resolve tool -> server
                         |
                    ServerManager
                    - ArcSwap<HashMap<String, Arc<UpstreamServer>>>
                    - start_all / shutdown_all
                         |
              +----------+----------+
              |                     |
         stdio transport      HTTP transport
         (TokioChildProcess)  (StreamableHttpClientTransport)
              |                     |
         Local MCP Server     Remote MCP Server
```

## Related Documentation

- [Phase 1 rmcp SDK Patterns](./rmcp-sdk-integration-patterns-plug-20260303.md) — ErrorData builders, ServiceExt import, Figment config
- [docs/PLAN.md](../../PLAN.md) — Full project plan with Phase 2 requirements
- [docs/ARCHITECTURE.md](../../ARCHITECTURE.md) — Component design and data flows
- [PR #2](https://github.com/cyberpapiii/plug/pull/2) — Phase 2 implementation
