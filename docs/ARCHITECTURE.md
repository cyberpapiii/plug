# Architecture

## System Overview

fanout is a protocol-aware MCP multiplexer. It presents itself as a single MCP server to N downstream clients while managing connections to M upstream MCP servers. It is a single Rust binary with three operating modes: TUI (default), headless daemon, and CLI.

```
                       ┌─────────────────────────────────────────┐
                       │              fanout                      │
                       │                                         │
  Downstream           │  ┌───────────────────────────────────┐  │         Upstream
  (Clients)            │  │          Core Engine               │  │         (MCP Servers)
                       │  │                                   │  │
  Claude Code ─stdio──►│  │  SessionMgr ◄─► ToolRouter       │──stdio──► github
  Claude Code ─stdio──►│  │      │              │             │──stdio──► filesystem
  Cursor ──────stdio──►│  │      ▼              ▼             │──stdio──► postgres
  Gemini CLI ──http───►│  │  ClientRegistry  ServerRegistry   │──http───► notion (remote)
  Codex ───────stdio──►│  │      │              │             │──sse────► legacy-server
  OpenCode ────sse────►│  │      ▼              ▼             │  │
  Remote ──────http───►│  │  ToolCache ◄──► MergeEngine      │  │
                       │  │      │              │             │  │
                       │  └──────┼──────────────┼─────────────┘  │
                       │         │              │                │
                       │  ┌──────▼──────────────▼─────────────┐  │
                       │  │      Transport Layer               │  │
                       │  │  Inbound:     Outbound:            │  │
                       │  │  - stdio      - stdio (child proc) │  │
                       │  │  - HTTP/SSE   - HTTP client        │  │
                       │  │  - .localhost  - SSE client         │  │
                       │  └───────────────────────────────────┘  │
                       │                                         │
                       │  ┌─────────┐ ┌──────┐ ┌─────────────┐  │
                       │  │   TUI   │ │ CLI  │ │   Daemon    │  │
                       │  │(Ratatui)│ │(Clap)│ │ (headless)  │  │
                       │  └─────────┘ └──────┘ └─────────────┘  │
                       └─────────────────────────────────────────┘
```

---

## Component Design

### 1. Core Engine (UI-Agnostic)

The core engine contains ALL business logic. It knows nothing about TUI, CLI, or daemon mode. It exposes a pure Rust API that all three frontends consume.

```rust
pub struct Engine {
    config: ArcSwap<Config>,            // Hot-swappable config
    sessions: SessionManager,            // Client session tracking
    servers: ServerRegistry,             // Upstream server management
    tool_cache: ToolCache,              // Merged tool index
    tool_router: ToolRouter,            // Request routing logic
    event_bus: broadcast::Sender<Event>, // Internal event stream (for TUI, logging)
}
```

**Why UI-agnostic**: The TUI, CLI, and daemon are just different views of the same engine. This is the lazygit/gitui pattern — clean separation between model and view. It also means we can add a web UI later without touching core logic.

### 2. Session Manager

Manages the lifecycle of all client and upstream sessions.

**Client sessions**: One per connected client (identified by transport connection + MCP-Session-Id for HTTP clients).

```rust
pub struct ClientSession {
    id: SessionId,
    client_type: ClientType,            // Cursor, ClaudeCode, GeminiCli, etc.
    tool_limit: Option<usize>,          // Derived from client_type
    transport: TransportHandle,         // How to send messages back
    pending_requests: DashMap<RequestId, PendingRequest>,
    created_at: Instant,
    last_activity: AtomicInstant,
}
```

**Upstream sessions**: One per client per server (N×M model). Each downstream client gets its own upstream session to each server, ensuring proper isolation of session state (logging levels, resource subscriptions, capabilities).

```rust
pub struct UpstreamSession {
    server_id: String,
    client_session_id: SessionId,       // The downstream client this session belongs to
    transport: TransportHandle,         // stdio child process or HTTP client
    capabilities: ServerCapabilities,   // From initialize response
    tools: RwLock<Vec<Tool>>,           // Last known tool list
    health: AtomicHealth,               // Healthy, Degraded, Failed
    circuit_breaker: CircuitBreaker,
}
```

**Why per-client sessions (ADR-002)**: MCP session state is per-session — `logging/setLevel` mutates persistent state, `resources/subscribe` creates persistent subscriptions. If Client A sets debug logging on a shared session, Client B would also get debug logs. N clients × M servers = N×M upstream sessions. This is more expensive but correct. Phase 4 daemon can optimize with session pooling.

**Session routing**: When Client A calls `tools/call github__create_issue`, the session manager:
1. Resolves "github" as the target server via ToolRouter
2. Looks up Client A's dedicated upstream session to "github"
3. Sends the request on Client A's upstream connection (no ID remapping needed within a dedicated session)
4. When the response arrives, routes it back to Client A
5. If Client A's upstream session doesn't exist yet, creates it lazily (initialize handshake)

### 3. Tool Router

The brain of the multiplexer. Resolves which upstream server handles a given tool.

**4-tier resolution**:

```
Tier 1: Cache (O(1))
  DashMap<tool_name, server_id>
  Hit? → Return immediately.
  Miss? → Tier 2.

Tier 2: Prefix extraction
  "github__create_issue" → split on "__" → "github"
  Server "github" exists? → Cache it, return.
  No match? → Tier 3.

Tier 3: Negative cache
  DashMap<tool_name, Instant> with 30s TTL
  In cache? → Error: tool not found.
  Not in cache? → Tier 4.

Tier 4: Full fan-out
  Query all upstream servers in parallel (JoinSet + per-server timeout).
  Found? → Cache it, return.
  Not found? → Add to negative cache, error.
```

### 4. Merge Engine

Aggregates responses from multiple upstream servers into a single response.

**For `tools/list`**:
- Fan-out to all healthy servers (parallel, with per-server timeout)
- Merge results into one list
- Apply tool name prefixing for collision avoidance
  In `v0.1`, prefixing is always on. The legacy `enable_prefix` config field is ignored.
- Apply client-specific filtering (tool limits, priority tools)
- Cache the merged result

**Merge-based cache strategy**: When a server times out during fan-out, its tools from the LAST SUCCESSFUL response are preserved in the merged result. This prevents tools from disappearing during transient failures.

**For `resources/list`, `prompts/list`**: Same fan-out + merge pattern.

### 5. Transport Layer

All transports implement a common trait:

```rust
pub trait Transport: Send + Sync + 'static {
    async fn send(&self, message: JsonRpcMessage) -> Result<()>;
    fn receive(&self) -> impl Stream<Item = JsonRpcMessage>;
    async fn close(&self) -> Result<()>;
}
```

**Inbound transports** (clients connect to fanout):

| Transport | Implementation | When |
|-----------|---------------|------|
| stdio | Read stdin, write stdout (via `fanout connect`) | Claude Code, Cursor, Zed, any local client |
| Streamable HTTP | Axum server on port 3282 | Remote clients, Gemini CLI, Claude Desktop |
| Legacy SSE | Axum with SSE endpoint (backwards compat) | Older clients |
| .localhost router | Axum Host header routing | Per-server direct access |

**Outbound transports** (fanout connects to servers):

| Transport | Implementation | When |
|-----------|---------------|------|
| stdio (child process) | `tokio::process::Command` | Local MCP servers (most common) |
| Streamable HTTP | `reqwest` client | Remote MCP servers |
| Legacy SSE | `reqwest` with SSE stream | Legacy remote servers |

### 6. Config Manager

Layered configuration via Figment:

```
CLI flags (highest priority)
    ↓
Environment variables (FANOUT_*)
    ↓
Config file (~/.config/fanout/config.toml)
    ↓
Compiled defaults (lowest priority)
```

Hot-reload via `notify` crate watching the config file. Changes trigger:
- New servers: start and connect
- Removed servers: graceful shutdown
- Changed servers: restart
- Changed settings: apply immediately (bind address change requires restart warning)

Config is stored in an `ArcSwap<Config>` — readers get a snapshot with zero lock contention.

### 7. Event Bus

Internal broadcast channel for observability:

```rust
pub enum Event {
    ServerConnected { server_id: String, tool_count: usize },
    ServerDisconnected { server_id: String, reason: String },
    ClientConnected { session_id: SessionId, client_type: ClientType },
    ClientDisconnected { session_id: SessionId },
    ToolCall { client: SessionId, server: String, tool: String, duration: Duration },
    ToolListServed { client: SessionId, total: usize, served: usize },
    Error { context: String, error: String },
    ConfigReloaded,
}
```

The TUI subscribes to this bus for real-time updates. The daemon logs events. The CLI ignores it.

---

## Data Flow: Tool Call

The most common operation. Here's exactly what happens when Client A calls a tool:

```
1. Client A sends JSON-RPC request:
   {"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"github__create_issue","arguments":{...}}}

2. Inbound transport receives and parses the message.

3. Session Manager identifies Client A's session.

4. Tool Router resolves "github__create_issue":
   - Cache hit? → "github" server
   - Prefix? → "github" (before "__")
   - Fan-out? → query all servers (last resort)

5. Session Manager checks UpstreamSession for "github":
   - Health check: is circuit breaker closed?
   - Generate upstream request ID (e.g., 7001)
   - Store mapping: 7001 → (client_a_session, req_id=42)

6. Strip prefix (if configured): "github__create_issue" → "create_issue"
   Send to upstream: {"jsonrpc":"2.0","id":7001,"method":"tools/call","params":{"name":"create_issue","arguments":{...}}}

7. Upstream server processes and responds:
   {"jsonrpc":"2.0","id":7001,"result":{"content":[{"type":"text","text":"Issue created: #123"}]}}

8. Session Manager looks up mapping for upstream req 7001:
   → (client_a_session, req_id=42)

9. Rewrite response ID: 7001 → 42
   Send to Client A: {"jsonrpc":"2.0","id":42,"result":{"content":[{"type":"text","text":"Issue created: #123"}]}}

10. Event bus emits ToolCall event (for TUI/logging).
11. Update tool_cache hit count (for priority sorting).
```

**Total overhead**: < 5ms for in-memory routing. Network latency is the upstream server's problem.

---

## Data Flow: tools/list (Fan-Out + Merge)

```
1. Client A sends: {"jsonrpc":"2.0","id":1,"method":"tools/list"}

2. Check merged tool cache:
   - Fresh (< 30s since last full fan-out)? → Return cached, filtered for client.
   - Stale? → Continue to fan-out.

3. Fan-out to all healthy upstream servers (parallel JoinSet):
   - github:    {"jsonrpc":"2.0","id":101,"method":"tools/list"} → 12 tools (15ms)
   - filesystem: {"jsonrpc":"2.0","id":102,"method":"tools/list"} → 4 tools (3ms)
   - postgres:  {"jsonrpc":"2.0","id":103,"method":"tools/list"} → 8 tools (5ms)
   - notion:    {"jsonrpc":"2.0","id":104,"method":"tools/list"} → TIMEOUT (25s)

4. Merge results:
   - github: 12 tools ✓
   - filesystem: 4 tools ✓
   - postgres: 8 tools ✓
   - notion: TIMEOUT → use last cached tools (8 tools from previous successful response)
   - Total: 32 tools

5. Apply prefix (if enabled): "create_issue" → "github__create_issue"

6. Filter for Client A (Cursor, 40-tool limit):
   - 32 tools < 40 → serve all
   - If > 40: sort by priority (usage count, config priority_tools), take top 40

7. Return merged, filtered list to Client A.
8. Update tool cache.
9. Event bus: ToolListServed { total: 32, served: 32 }
```

---

## Data Flow: Initialize (Capability Synthesis)

When a new client connects, fanout must synthesize capabilities from all upstream servers:

```
1. Client sends InitializeRequest with its capabilities and clientInfo.

2. fanout detects client type from clientInfo.name:
   - "Claude Code" → ClaudeCode (no tool limit, supports tool search)
   - "Cursor" → Cursor (40 tool limit)
   - "Gemini" → GeminiCli (must return prompts fast)
   - Unknown → Unknown (conservative defaults)

3. fanout constructs a synthesized InitializeResult:
   - protocolVersion: minimum of (client's requested, fanout's supported)
   - capabilities: union of all upstream server capabilities
     - tools: { listChanged: true } if ANY upstream supports it
     - resources: { listChanged: true, subscribe: true } if ANY supports it
     - prompts: { listChanged: true } if ANY supports it
     - logging: {} if ANY supports it
   - serverInfo: fanout's own info
   - instructions: optional, configurable

4. Send InitializeResult to client.

5. Client sends notifications/initialized.

6. Create ClientSession, store in SessionManager.

7. Event bus: ClientConnected { client_type: Cursor }
```

---

## Concurrency Model

### Async Runtime

Tokio multi-threaded runtime with work-stealing scheduler. One runtime for the entire binary.

### Shared State

| State | Data Structure | Access Pattern |
|-------|---------------|----------------|
| Config | `ArcSwap<Config>` | Read-heavy, rare writes. Lock-free atomic swap. |
| Client sessions | `DashMap<SessionId, ClientSession>` | Concurrent reads, frequent insert/remove. |
| Upstream sessions | `DashMap<(SessionId, String), UpstreamSession>` | Keyed by (client_session, server_id). Lazy creation on first access. |
| Tool cache | `DashMap<String, (String, Tool)>` | High-frequency reads, writes on cache miss. |
| Negative cache | `DashMap<String, Instant>` | Read-heavy, TTL-based eviction. |
| Request mapping | `DashMap<UpstreamReqId, (SessionId, RequestId)>` | Per-request insert/remove. |
| Event bus | `tokio::sync::broadcast` | Multiple readers, single writer per event. |

### Critical Rules

1. **Never hold a lock across an `.await` point.** Use scoped blocks to release before awaiting.
2. **Never block the Tokio runtime.** Use `tokio::task::spawn_blocking` for CPU-intensive work (if any).
3. **All upstream calls have timeouts.** No unbounded waits.
4. **All shared state is behind concurrent data structures.** No `Mutex` for hot paths.

---

## Error Handling Strategy

Two categories of errors:

### Protocol Errors (JSON-RPC)
Returned as JSON-RPC error responses. The client sees these.

```rust
pub enum ProtocolError {
    ToolNotFound { tool_name: String },         // -32601 Method not found
    ServerUnavailable { server_id: String },     // -32603 Internal error
    Timeout { duration: Duration },              // -32603 Internal error
    InvalidRequest { detail: String },           // -32600 Invalid request
}
```

### Internal Errors (Operational)
Logged, emitted on event bus, shown in TUI. The client does NOT see these.

```rust
pub enum InternalError {
    ConfigParseError { path: PathBuf, detail: String },
    ServerStartFailed { server_id: String, reason: String },
    TransportError { context: String, source: Box<dyn Error> },
}
```

### Error Conversion
Every `ProtocolError` converts to a JSON-RPC error response. Every `InternalError` converts to a log entry + event bus emission. Clean separation, no leaking internals to clients.

---

## File System Layout

```
~/.config/fanout/
  config.toml              # User configuration

~/.local/share/fanout/
  logs/
    fanout.log             # Rolling log file (daily rotation)
  cache/
    tool_cache.json        # Optional persistent tool cache (for faster cold start)

~/.local/state/fanout/
  fanout.pid               # PID file (for daemon mode)
  fanout.sock              # Unix socket (for CLI → daemon communication)
```

XDG-spec compliant paths. Created on first run.

---

## Security Model

### What We Protect

1. **Secrets in config** — env var references (`$GITHUB_TOKEN`), never stored in plain text
2. **Secrets in logs** — token values are NEVER logged, even at trace level
3. **Session IDs** — cryptographically random (UUID v4), not guessable
4. **Origin validation** — HTTP server validates Origin header (prevents DNS rebinding per MCP spec)
5. **Bind address** — default to 127.0.0.1 (localhost only), never 0.0.0.0

### What We Don't Protect (Out of Scope)

1. **Upstream server auth** — we pass through bearer tokens, but don't manage OAuth flows
2. **Client authentication** — we trust clients that can connect (local-first model)
3. **Encryption at rest** — config is plain TOML, user manages file permissions
4. **Network encryption** — HTTP on localhost is fine; HTTPS is opt-in for remote

### Trust Model

fanout trusts:
- The user's config file
- Local stdio clients (they're on the same machine)
- Upstream MCP servers (as much as the user trusts them)

fanout does NOT trust:
- Tool annotations from untrusted upstream servers (advisory only, per MCP spec)
- Arbitrary HTTP connections (Origin header validation required)
