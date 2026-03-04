# Competitive Architecture Analysis for Plug

**Date:** 2026-03-03
**Purpose:** Deep architectural analysis of MCP multiplexer/gateway competitors to extract patterns for the Plug project.

---

## Table of Contents

1. [AgentGateway (Rust, Linux Foundation)](#1-agentgateway)
2. [mcpproxy-go (Go)](#2-mcpproxy-go)
3. [FastMCP 3.0 (Python)](#3-fastmcp-30)
4. [Docker MCP Gateway (Go)](#4-docker-mcp-gateway)
5. [IBM ContextForge (Python)](#5-ibm-contextforge)
6. [Cross-Cutting Patterns & Recommendations](#6-cross-cutting-patterns--recommendations)
7. [Anti-Patterns to Avoid](#7-anti-patterns-to-avoid)
8. [Recommended Patterns for Plug](#8-recommended-patterns-for-plug)

---

## 1. AgentGateway

**Repository:** https://github.com/agentgateway/agentgateway
**Language:** Rust (56.8%), Go (28.0%), TypeScript (12.3%)
**Affiliation:** Linux Foundation / Solo.io (kgateway/Envoy ecosystem)

### Architecture Summary

AgentGateway is a full reverse-proxy data plane for agentic AI, supporting both MCP (agent-to-tool) and A2A (agent-to-agent) protocols. It follows the Envoy-style proxy architecture with xDS dynamic configuration.

```
                    +---------------------------+
                    |     Control Plane (Go)     |
                    |  Kubernetes CRDs -> xDS    |
                    +------------+--------------+
                                 | gRPC/xDS ADS
                    +------------v--------------+
                    |   AgentGateway Data Plane  |
                    |         (Rust)             |
                    |                            |
  Client --------->| Listener -> Route -> Backend|
  (MCP/A2A/HTTP)   |    |         |         |   |
                    |  Policies  Policies  Policies
                    |  (CEL)    (CEL)     (CEL)  |
                    +--+------+------+------+---+
                       |      |      |      |
                    +--v--+ +-v--+ +-v--+ +-v--+
                    | MCP | |MCP | |A2A | |LLM |
                    |Srv1 | |Srv2| |Agt | |API |
                    +-----+ +----+ +----+ +----+
```

### Session Multiplexing

AgentGateway implements session multiplexing via a `SessionManager` struct located at `crates/agentgateway/src/mcp/session.rs`. Key details:

- **Stateful sessions** pin clients to specific backends; stateless mode distributes requests
- Sessions are stored **in-process** -- no distributed session store
- When multiple backends exist, the gateway enables **MCP multiplexing** automatically
- Tool names are rewritten using the pattern `${backend_name}_${tool_name}` to prevent collisions
- The gateway sends `tools/list` to **all backends simultaneously** and merges responses

**Weakness identified by reviewers:** Running multiple agentgateway instances creates routing issues because there is no guarantee a client hits the same `SessionManager`. The suggested fix is consistent-hashing on `MCP-Session-ID` header rather than maintaining distributed session state.

> Source: https://dev.to/spacewander/agentgateway-review-a-feature-rich-new-ai-gateway-53lm

### Tool Routing

Tool routing uses a fan-out/merge pattern implemented in the `MCPRouter`:

1. Client sends `tools/list` request
2. Gateway fans out to all configured backend MCP servers simultaneously
3. Each tool name is prefixed with backend identifier (e.g., `mcp-server-everything-3001_echo`)
4. When a tool call arrives, the gateway parses the prefix to route to the correct backend
5. CEL-based authorization filters tools **before** name merging (important ordering)

CEL authorization example from `crates/agentgateway/src/mcp/authorization.rs`:
```
jwt.sub == "test-user" && mcp.tool.name == "add"
```

Available CEL context attributes: `request.*`, `response.*`, `source.*`, `backend.*`, `llm.*`, `mcp.*` (method, available tools, resources).

### Transport Abstraction

AgentGateway does **not use rmcp**. It implements its own MCP protocol handling using low-level Rust networking:

- **HTTP/1.1, HTTP/2, HTTP/3** (with QUIC) via `hyper` (1.6)
- **WebSocket** via `websocket-sans-io` (0.1)
- **gRPC** via `tonic` (0.14)
- **TLS** via `hyper-rustls` (0.27), explicitly avoiding OpenSSL (`aws-lc-rs` + `rustls`)
- **HBONE** tunneling (Istio-compatible H2 CONNECT with mTLS)
- **PROXY protocol v2** for preserving source IPs
- **Tower middleware** (0.5) for service composition

The transport layer also handles connection pooling per-backend with configurable limits, keep-alive, circuit breaking, and timeout enforcement.

### Configuration

Dual configuration model:
- **Local mode:** YAML/JSON files parsed via `LocalClient` -> `NormalizedLocalConfig`
- **xDS mode:** Kubernetes CRDs -> Go Controller -> xDS ADS -> gRPC stream
- Both paths converge at the `Store` (`crates/agentgateway/src/store/binds.rs` lines 44-60)
- Internal representation uses `Bind`, `Listener`, `Route`, `Backend` types
- Broadcast updates enable zero-downtime config reloads

### Key Source Files

```
crates/agentgateway/src/
  proxy/gateway.rs      -- TCP connection handling, tunnel protocol detection
  proxy/httpproxy.rs    -- HTTP request pipeline, routing, policy application
  mcp/mod.rs            -- MCP protocol implementation
  mcp/session.rs        -- Session management (SessionManager struct)
  mcp/relay.rs          -- Backend communication relay
  mcp/authorization.rs  -- CEL-based RBAC filtering
  store/binds.rs        -- In-memory config store with broadcast updates
  types/agent.rs        -- Core resource definitions (Bind, Listener, Route, Backend)
  types/agent_xds.rs    -- XDS protocol message types
  cel/                  -- CEL policy engine
  llm/                  -- LLM provider translation (OpenAI, Anthropic, Bedrock, etc.)
  telemetry/            -- Metrics, tracing, logging
```

### Patterns Worth Adopting

1. **CEL-based authorization** -- expressive, composable, evaluated at compile-time for performance
2. **Tool name prefixing** for multiplexing (`${backend}_${tool}`) -- simple and effective
3. **Fan-out/merge for tools/list** -- parallel discovery across all backends
4. **Tower middleware composition** -- proven pattern for layered request processing in Rust
5. **xDS-compatible config** -- future-proofs for Kubernetes-native deployments
6. **Policy hierarchy** (gateway -> route -> backend) with override semantics

### Patterns to AVOID

1. **In-process session storage** -- does not scale horizontally; they acknowledged this limitation
2. **Custom MCP implementation** -- massive engineering effort to maintain protocol compliance as MCP evolves; rmcp or the official Rust SDK would be safer
3. **Over-engineering the LLM translation layer** -- AgentGateway tries to translate between OpenAI/Anthropic/Bedrock/Vertex/Gemini APIs, which is a huge surface area to maintain and tangential to the core proxy mission
4. **Monolithic crate structure** -- everything in one large crate makes it hard to reuse components

---

## 2. mcpproxy-go

**Repository:** https://github.com/smart-mcp-proxy/mcpproxy-go
**Language:** Go
**Type:** Desktop application with system tray

### Architecture Summary

mcpproxy-go is a desktop-focused MCP proxy that aggregates multiple upstream MCP servers behind a single HTTP endpoint. Its key differentiator is BM25-based tool retrieval that replaces the traditional "load all tools" approach.

```
  AI Client (Cursor/Claude/etc.)
       |
       | HTTP (localhost:8080/mcp/)
       v
  +--------------------+
  | mcpproxy-go        |
  |                    |
  | +----------------+ |      +-----------+
  | | BM25 Search    | |      | MCP Srv 1 |
  | | Index          |<------>| (stdio)   |
  | +----------------+ |      +-----------+
  |                    |      +-----------+
  | +----------------+ |      | MCP Srv 2 |
  | | Tool Registry  |<------>| (HTTP)    |
  | +----------------+ |      +-----------+
  |                    |      +-----------+
  | +----------------+ |      | MCP Srv N |
  | | Security       |<------>| (Docker)  |
  | | Quarantine     | |      +-----------+
  | +----------------+ |
  +--------------------+
```

### BM25 Tool Filtering

**How it works:**

1. On startup, mcpproxy connects to all configured upstream MCP servers
2. It indexes all tool names and descriptions into a BM25 search index
3. Instead of exposing all tools to the client, it exposes a single meta-tool: `retrieve_tools`
4. When an agent needs a tool, it calls `retrieve_tools("query about what I need")`
5. BM25 scores tool descriptions against the query, returns top-K matches (default: 5)
6. The agent then calls the actual tool by name via `call_tool`

**Key configuration parameters:**
- `top_k`: Number of tools returned by `retrieve_tools` (default: 5)
- `tools_limit`: Maximum tools returned to client overall (default: 15)
- `tool_response_limit`: Auto-truncates responses above character threshold

### BM25 Accuracy Assessment

Based on RAG-MCP research cited in the project documentation:

| Metric | All Tools Loaded | BM25 Retrieval |
|--------|-----------------|----------------|
| Tool selection accuracy | 13.62% | 43.13% |
| Prompt token usage | Baseline | ~50% reduction |
| Overall token savings | -- | ~99% reduction |

> Source: https://dev.to/algis/mcp-proxy-pattern-secure-retrieval-first-tool-routing-for-agents-247c

**Critical assessment of BM25:**
- 43% accuracy is a **massive improvement** over 13.6% baseline, but still means **more than half of tool selections are wrong**
- BM25 is a lexical matching algorithm -- it fails on semantic queries (e.g., "send a message" won't match a tool described as "compose email notification")
- The architecture is designed to be pluggable -- the README notes users "could plug in a vector database for semantic tool retrieval in place of BM25"
- For Plug: BM25 is a solid **starting point** but should be considered a stepping stone to hybrid retrieval (BM25 + embeddings)

### Proxy Architecture

- **Headless server** (`mcpproxy serve`) binds to `127.0.0.1:8080` (localhost-only by default)
- **Optional system tray** UI for desktop users
- **Upstream management:** `mcpproxy upstream list/restart/enable/disable`
- **Secrets management:** OS-native keyrings (macOS Keychain, Linux Secret Service, Windows Credential Manager) with `${keyring:secret_name}` placeholders in config
- **Docker isolation:** Optional stdio server containerization with process/filesystem isolation, configurable network, CPU/memory limits
- **Security quarantine:** Automatic blocking of new/suspicious tools until manual approval (anti-tool-poisoning)

### Configuration

File: `~/.mcpproxy/mcp_config.json`
```json
{
  "listen": "127.0.0.1:8080",
  "data_dir": "~/.mcpproxy",
  "top_k": 5,
  "tools_limit": 15,
  "tool_response_limit": 10000,
  "enable_tray": true,
  "docker_isolation": false,
  "tls": { ... }
}
```

### Patterns Worth Adopting

1. **Meta-tool pattern** (`retrieve_tools`) -- elegantly solves the "too many tools" problem without protocol changes
2. **Tool quarantine** -- security-first approach to new/untrusted tools
3. **OS-native keyring integration** for secrets
4. **Configurable tool response truncation** -- prevents context window blowout
5. **Docker isolation for stdio servers** -- practical security measure

### Patterns to AVOID

1. **BM25-only retrieval** -- 43% accuracy is not production-grade; needs semantic layer
2. **Desktop-app-first architecture** -- system tray integration adds complexity; server-first is better
3. **Single-process aggregation** -- connecting to hundreds of upstream servers in one process creates reliability issues (one crash takes everything down)

---

## 3. FastMCP 3.0

**Repository:** https://github.com/jlowin/fastmcp (now under PrefectHQ)
**Language:** Python
**Status:** GA, powering ~70% of MCP servers

### Architecture Summary

FastMCP 3.0 is built around three foundational primitives: **Components**, **Providers**, and **Transforms**. This is the most elegant compositional architecture among all competitors.

```
  +-----------------------------------------------------+
  |                    FastMCP Server                     |
  |                                                      |
  |  Provider A          Provider B         Provider C   |
  | (LocalProvider)    (ProxyProvider)   (OpenAPIProvider)|
  |      |                  |                  |         |
  |      v                  v                  v         |
  |  +--------+        +--------+        +--------+     |
  |  |Components|      |Components|      |Components|   |
  |  |(tools,   |      |(remote  |      |(API-     |    |
  |  | resources|      | MCP     |      | derived  |    |
  |  | prompts) |      | server) |      | tools)   |    |
  |  +----+-----+      +----+----+      +----+-----+    |
  |       |                  |                |          |
  |       +--------+---------+--------+-------+          |
  |                |                  |                  |
  |          +-----v------+    +-----v------+           |
  |          | Transform  |    | Transform  |           |
  |          | (Namespace)|    | (Filter)   |           |
  |          +-----+------+    +-----+------+           |
  |                |                  |                  |
  |          +-----v------+    +-----v------+           |
  |          | Transform  |    | Transform  |           |
  |          | (Rename)   |    | (Version)  |           |
  |          +-----+------+    +-----+------+           |
  |                |                  |                  |
  |                +--------+---------+                  |
  |                         |                            |
  |                   +-----v------+                     |
  |                   |  Middleware |                     |
  |                   | (Auth/Rate |                     |
  |                   |  /Logging) |                     |
  |                   +-----+------+                     |
  |                         |                            |
  |                    MCP Protocol                      |
  +-----------------------------------------------------+
```

### The Provider + Transform Architecture

**Components** are the atomic units: tools, resources, and prompts. Each has a name, schema, metadata, and execution logic.

**Providers** answer "where do components come from?" Available provider types:

| Provider | Source | Use Case |
|----------|--------|----------|
| `LocalProvider` | Decorated Python functions | Traditional tool authoring |
| `FileSystemProvider` | Directory scanning | Hot-reload tool files |
| `OpenAPIProvider` | OpenAPI specs | REST-to-MCP conversion |
| `ProxyProvider` | Remote MCP servers | Federation/proxying |
| `FastMCPProvider` | Other FastMCP instances | Composition with middleware chains |
| `SkillsProvider` | Instruction files | Claude Code/Cursor skill integration |

**Transforms** answer "how are components shaped before reaching clients?" They operate on the **component pipeline**, not on requests:

| Transform | Effect |
|-----------|--------|
| `Namespace` | Adds prefix to names/URIs (collision prevention) |
| `ToolTransform` | Rename, rewrite descriptions, modify arguments, add tags |
| `VersionFilter` | Expose components only within version ranges |
| `Visibility` | Blocklist/allowlist by tag, name, or version |
| `ResourcesAsTools` | Expose resources as tools for limited clients |
| `PromptsAsTools` | Expose prompts as tools for limited clients |

The key insight: **Transforms operate at two levels:**
- **Provider-level** (`provider.add_transform()`) -- affects only that provider's components
- **Server-level** (`server.add_transform()`) -- affects all aggregated components

### How Transforms Work as Middleware

Transforms have two operation modes:

**List operations** receive the full sequence and return transformed results:
```python
async def list_tools(self, tools: Sequence[Tool]) -> Sequence[Tool]:
    return [t for t in tools if not t.name.startswith("_internal")]
```

**Get operations** use middleware chaining with `call_next`:
```python
async def get_tool(self, name: str, call_next: GetToolNext) -> Tool | None:
    # Transform the name before looking up
    original_name = name.removeprefix("myns_")
    return await call_next(original_name)
```

### Middleware vs. Transforms (Critical Distinction)

FastMCP 3.0 explicitly separates two concerns:

- **Transforms** = what components exist (shaping the tool/resource/prompt catalog)
- **Middleware** = how requests execute (auth, logging, rate limiting, caching)

Middleware follows the standard request/response pipeline:
```
Request -> Middleware A -> Middleware B -> Handler -> Middleware B -> Middleware A -> Response
```

Built-in middleware: Logging, Timing, Caching (TTL-based), Rate Limiting (token bucket or sliding window), Error Handling (with retry + exponential backoff), Response Limiting (truncation), Tool Injection.

### Can We Adopt This in Rust?

**Yes, with adaptations.** The Provider/Transform pattern maps well to Rust traits:

```rust
// Conceptual Rust equivalent
trait Provider: Send + Sync {
    async fn list_tools(&self) -> Vec<ToolDefinition>;
    async fn list_resources(&self) -> Vec<ResourceDefinition>;
    async fn list_prompts(&self) -> Vec<PromptDefinition>;
    async fn call_tool(&self, name: &str, args: Value) -> ToolResult;
}

trait Transform: Send + Sync {
    async fn transform_tools(&self, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition>;
    async fn transform_tool_name(&self, name: &str) -> Option<String>;
}

// Namespace transform
struct NamespaceTransform { prefix: String }

impl Transform for NamespaceTransform {
    async fn transform_tools(&self, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
        tools.into_iter()
            .map(|mut t| { t.name = format!("{}_{}", self.prefix, t.name); t })
            .collect()
    }
}
```

Tower middleware can handle the request-level concerns (auth, rate limiting, logging).

### Patterns Worth Adopting

1. **Component/Provider/Transform trifecta** -- the cleanest compositional model in the ecosystem
2. **Explicit separation of Transforms (catalog-shaping) vs. Middleware (request-handling)**
3. **Provider-level vs. server-level transforms** -- granular control
4. **Mounting = Provider + Namespace Transform** -- eliminates a dedicated mounting subsystem
5. **Per-session visibility** as a transform applied to individual sessions
6. **Component versioning** with highest-version-wins semantics
7. **`call_next` chaining** for get operations -- composable middleware pattern
8. **FileSystemProvider with hot-reload** -- excellent for development

### Patterns to AVOID

1. **Python-specific patterns that don't translate** -- decorator-based component registration is Pythonic but Rust prefers explicit registration or proc macros
2. **Session state in application memory** (`ctx.get_state/set_state`) -- needs external backing store for production
3. **Overly complex auth model** -- CIMD (Client ID Metadata Document) adds significant complexity

> Sources:
> - https://www.jlowin.dev/blog/fastmcp-3-whats-new
> - https://gofastmcp.com/servers/middleware
> - https://github.com/jlowin/fastmcp

---

## 4. Docker MCP Gateway

**Repository:** https://github.com/docker/mcp-gateway
**Language:** Go (97.5%)
**Docs:** https://docs.docker.com/ai/mcp-catalog-and-toolkit/mcp-gateway/

### Architecture Summary

Docker's MCP Gateway is a containerized proxy that manages MCP servers as Docker containers. Its key innovation is the **"primordial tools" pattern** for dynamic server discovery.

```
  AI Client (Claude/Cursor/etc.)
       |
       | stdio / streaming / SSE
       v
  +----------------------------------+
  | Docker MCP Gateway               |
  |                                  |
  | Primordial Tools:                |
  |  - mcp-find (search catalog)     |
  |  - mcp-add (add server)          |
  |  - mcp-remove (remove server)    |
  |  - mcp-config-set (configure)    |
  |  - mcp-exec (execute tool)       |
  |  - code-mode (JS sandbox)        |
  |                                  |
  | +------------------------------+ |
  | | Tool Router                  | |
  | | (server -> container mapping)| |
  | +-----+-------+-------+-------+ |
  +-------|-------|-------|---------+
          |       |       |
    +-----v-+ +---v---+ +-v------+
    |Docker | |Docker | |Docker  |
    |Contain| |Contain| |Contain |
    |er:    | |er:    | |er:     |
    |MCP    | |MCP    | |MCP     |
    |Srv A  | |Srv B  | |Srv C   |
    +-------+ +-------+ +--------+
```

### Dynamic Discovery: The Primordial Tools Pattern

This is Docker's most innovative contribution. When a client connects to the gateway, it receives a set of **meta-tools** that let the agent **self-configure its own tooling**:

| Primordial Tool | Purpose |
|----------------|---------|
| `mcp-find` | Search the Docker MCP Catalog (270+ curated servers) |
| `mcp-add` | Add a server to the current session (no restart needed) |
| `mcp-remove` | Remove a server from the current session |
| `mcp-config-set` | Configure server settings (auth, env vars) |
| `mcp-exec` | Execute a tool from any active server |
| `code-mode` | Create a JavaScript sandbox that chains multiple tools |

**How dynamic discovery works:**

1. Agent connects to gateway, gets primordial tools only (minimal context overhead)
2. Agent encounters a task needing external tools (e.g., "read this Slack channel")
3. Agent calls `mcp-find("slack messaging")` -- searches the catalog
4. Agent calls `mcp-add("slack")` -- gateway starts a Docker container running the Slack MCP server
5. Gateway handles OAuth/credential injection automatically
6. Agent now has access to Slack tools **without restart or manual configuration**
7. Dynamically added servers are **session-scoped only** (don't persist to user profiles)

### Code-Mode Architecture

`code-mode` is a powerful compositional tool:

1. Agent invokes `code-mode` with server names and desired tool name
2. Gateway creates an isolated JavaScript sandbox with access to specified servers' tools
3. A new composite tool is registered in the current session
4. Agent calls the new tool, which executes JavaScript in the sandbox
5. The sandbox can **only** access MCP tools -- no external system access

This effectively allows agents to **compose multi-tool workflows** at runtime.

### Container Isolation

- Each MCP server runs in its own Docker container
- Restricted to **1 CPU** and **2 GB memory** per container
- **No host filesystem access** by default (explicit mounts required)
- Network access configurable per server
- All catalog servers are Docker-built, signed, and maintained
- Credentials managed by the gateway, injected securely into containers

### Gateway Registry Pattern

Configuration stored at `~/.docker/mcp/`:
- `docker-mcp.yaml` -- server catalog definitions
- `registry.yaml` -- enabled server registry
- `config.yaml` -- per-server runtime configuration
- `tools.yaml` -- enabled tools per server

Server lifecycle: `docker mcp server enable/disable/inspect <name>`

### Transport Options

- **stdio** (default) -- single-client, direct process communication
- **streaming** -- HTTP-based, multi-client capable, configurable port
- **sse** -- Server-Sent Events for persistent connections

### Patterns Worth Adopting

1. **Primordial tools pattern** -- agents self-configure their tooling; reduces initial context overhead to near-zero
2. **Session-scoped dynamic servers** -- ephemeral additions that don't pollute configuration
3. **Code-mode sandboxing** -- agent-created composite tools (could be adapted as a Wasm sandbox in Rust)
4. **Container isolation** with strict resource limits -- security best practice
5. **Catalog-first approach** -- curated, signed server catalog with search
6. **Credential injection** -- gateway manages secrets, containers never see raw credentials

### Patterns to AVOID

1. **Docker Desktop dependency** -- limits deployment scenarios to Docker users
2. **JavaScript sandbox for composition** -- could be a security risk; Wasm would be safer
3. **No multi-instance support** -- single-gateway design limits scaling
4. **Session state in process memory** -- same scaling concern as AgentGateway

> Sources:
> - https://docs.docker.com/ai/mcp-catalog-and-toolkit/dynamic-mcp/
> - https://docs.docker.com/ai/mcp-catalog-and-toolkit/mcp-gateway/
> - https://github.com/docker/mcp-gateway
> - https://www.docker.com/blog/docker-mcp-gateway-secure-infrastructure-for-agentic-ai/
> - https://github.com/docker/mcp-gateway/issues/331

---

## 5. IBM ContextForge

**Repository:** https://github.com/IBM/mcp-context-forge
**Language:** Python (3.10+)
**Docs:** https://ibm.github.io/mcp-context-forge/

### Architecture Summary

ContextForge is a heavyweight enterprise gateway that federates MCP, A2A, and REST/gRPC APIs with centralized governance. It includes TOON compression for token efficiency.

```
  AI Clients
       |
       | HTTP / JSON-RPC / WebSocket / SSE / stdio
       v
  +------------------------------------------------+
  |            ContextForge Gateway                 |
  |                                                 |
  | +-------------------+  +--------------------+  |
  | | Tools Gateway     |  | Agent Gateway      |  |
  | | - MCP Federation  |  | - A2A Protocol     |  |
  | | - REST->MCP       |  | - OpenAI-compat    |  |
  | | - gRPC->MCP       |  | - Anthropic agents |  |
  | | - TOON compress   |  +--------------------+  |
  | +-------------------+                           |
  |                                                 |
  | +-------------------+  +--------------------+  |
  | | API Gateway       |  | Unified Registries |  |
  | | - Rate limiting   |  | - Tools            |  |
  | | - Auth            |  | - Resources        |  |
  | | - Retries         |  | - Prompts          |  |
  | | - Reverse proxy   |  | (all versioned)    |  |
  | +-------------------+  +--------------------+  |
  |                                                 |
  | +-------------------+  +--------------------+  |
  | | Plugin System     |  | Observability      |  |
  | | 40+ plugins       |  | - OpenTelemetry    |  |
  | | - gRPC transport  |  | - Phoenix/Jaeger   |  |
  | | - Unix sockets    |  | - Token tracking   |  |
  | | - mTLS            |  | - Cost analysis    |  |
  | | - Rust extensions |  +--------------------+  |
  | +-------------------+                           |
  +------------------------------------------------+
       |            |             |
  +----v----+  +----v----+  +----v----+
  | MCP Srv | | REST API| | gRPC Svc|
  +---------+ +---------+ +---------+
```

### TOON Compression

**TOON (Token-Optimized Object Notation)** is a compression format that reduces JSON token consumption by eliminating redundant keys:

**Before (JSON):**
```json
[
  {"id": "u_001", "name": "Alice Corp", "access_level": "admin", "region": "us-east-1"},
  {"id": "u_002", "name": "Bob Ltd", "access_level": "write", "region": "eu-west-1"}
]
```

**After (TOON):**
```
users[2]{id,name,access_level,region}:
  u_001,Alice Corp,admin,us-east-1
  u_002,Bob Ltd,write,eu-west-1
```

**Benchmarks:**

| Data Structure | Token Reduction vs. JSON |
|---------------|------------------------|
| Flat/tabular (uniform arrays) | ~58.8% |
| Mixed-structure (nested) | ~21.8% |
| API Responses (general) | 40-60% |
| Object Arrays | 55-70% |
| Configuration Files | 35-50% |

**Key properties:**
- Lossless -- perfect round-trip JSON <-> TOON conversion
- Header-row schematization (keys declared once per collection)
- Minimal delimiters (commas + newlines replace JSON brackets)
- Works best on **homogeneous arrays of objects** (tabular data)
- Much less effective on deeply nested, heterogeneous structures

**Rust implementation exists:** https://github.com/copyleftdev/toon-mcp (MIT license, Rust 1.70+, uses `serde`)

> Source: https://github.com/aj-geddes/toon-context-mcp

### Plugin Architecture

ContextForge supports 40+ plugins via:
- **gRPC transport** -- external plugins communicate over gRPC
- **Unix socket** connectivity
- **mTLS** for secure plugin communication
- **Rust plugins** for performance-critical extensions
- Plugins can add transports, authentication mechanisms, protocol adapters, and integrations

### Federation

- **Redis-backed caching** for distributed environments
- **Multi-cluster federation** across Kubernetes clusters
- **Stateless gateway design** enabling elastic horizontal scaling
- **Database backend** with 36+ schema tables (MariaDB 10.6 or PostgreSQL)
- **Namespaced tool federation** for collision prevention

### Tool Routing & Optimization

- Centralized tool catalog with metadata
- Virtual server bundling for logical tool grouping
- Tool-level rate limiting
- Bearer token, JWT, and OAuth authentication
- Header passthrough (`X-Upstream-Authorization`) for upstream auth

### Observability

OpenTelemetry integration with multiple OTLP backends:
- Phoenix (optimized for LLM applications)
- Jaeger, Zipkin, Tempo
- DataDog, New Relic
- Automatic tracking of tools, prompts, resources, and gateway operations
- LLM-specific metrics: token usage, cost analysis
- Zero overhead when disabled (graceful degradation)

### Admin UI

Built with HTMX + Alpine.js:
- Real-time config management
- Log viewer with filtering, search, export
- Server and tool lifecycle management
- Supports airgapped deployments

### Patterns Worth Adopting

1. **TOON compression** for tool responses -- significant token savings on tabular data; Rust crate already exists
2. **OpenTelemetry-native observability** -- vendor-agnostic, production-grade
3. **Tool-level rate limiting** -- prevents runaway tool calls
4. **Virtual server bundling** -- logical grouping without physical separation
5. **gRPC-based plugin architecture** -- language-agnostic extensibility
6. **REST/gRPC -> MCP virtualization** -- wrapping legacy APIs as MCP tools

### Patterns to AVOID

1. **Massive scope creep** -- ContextForge tries to be Tools Gateway + Agent Gateway + API Gateway + Registry + Admin UI all at once; 36+ database tables is a red flag
2. **Python for the gateway hot path** -- performance ceiling for a proxy
3. **Heavy database dependency** -- MariaDB/PostgreSQL required for basic operation adds operational complexity
4. **Too many plugins** -- 40+ plugins suggests an unfocused architecture
5. **Enterprise-first design** -- complex deployment (Docker Compose with Redis + DB) is hostile to individual developers

> Sources:
> - https://github.com/IBM/mcp-context-forge
> - https://ibm.github.io/mcp-context-forge/
> - https://dev.to/copyleftdev/optimizing-llm-context-windows-reducing-token-usage-by-40-with-toon-and-rust-1j10

---

## 6. Cross-Cutting Patterns & Recommendations

### Pattern Comparison Matrix

| Feature | AgentGateway | mcpproxy-go | FastMCP 3.0 | Docker MCP | ContextForge |
|---------|-------------|-------------|-------------|------------|-------------|
| **Language** | Rust | Go | Python | Go | Python |
| **MCP Impl** | Custom | Custom | Custom (SDK) | Custom | Custom |
| **Session Mgmt** | In-process | In-process | In-process | In-process | Redis/DB |
| **Tool Discovery** | Fan-out | BM25 search | Provider system | Primordial tools | Registry |
| **Tool Namespacing** | `backend_tool` | None | Transform | `server_tool` | Namespaced |
| **Auth** | CEL + JWT | None | Middleware | OAuth + Docker | JWT + OAuth |
| **Config** | YAML + xDS | JSON | Python code | YAML | DB + YAML |
| **Transport** | HTTP/WS/gRPC | HTTP | stdio/SSE/HTTP | stdio/SSE/HTTP | All |
| **Token Opt** | None | BM25 retrieval | None | Minimal context | TOON compress |
| **Observability** | Prometheus | None | OpenTelemetry | Docker logging | OpenTelemetry |
| **Scaling** | Horizontal* | Single process | Single process | Single gateway | Multi-cluster |

*AgentGateway horizontal scaling is limited by in-process session storage

### Universal Patterns

Every project implements these -- they are **table stakes**:
1. Tool aggregation across multiple upstream servers
2. Tool name collision prevention (namespacing/prefixing)
3. Multiple transport support (at minimum stdio + HTTP)
4. Some form of configuration management

### Differentiating Patterns

These patterns **separate leaders from followers**:
1. **Dynamic discovery** (Docker) -- agents configure their own tooling
2. **Transform pipeline** (FastMCP) -- compositional tool shaping
3. **CEL-based authorization** (AgentGateway) -- expressive, performant policy
4. **BM25 retrieval** (mcpproxy-go) -- reduces context window usage
5. **TOON compression** (ContextForge) -- reduces response token consumption
6. **OpenTelemetry observability** (ContextForge, FastMCP) -- production operations

---

## 7. Anti-Patterns to Avoid

### 1. Scope Creep (ContextForge)
Trying to be everything (Tools Gateway + Agent Gateway + API Gateway + Registry + Admin UI) leads to 36+ database tables and impenetrable complexity. **Plug should have a sharp, focused mission.**

### 2. In-Process Session State (Everyone)
Every single competitor stores sessions in process memory. This is the #1 scaling bottleneck. **Plug should design for external session state from day one** (even if the first implementation is in-memory with a trait boundary for future Redis/etc. backends).

### 3. Custom MCP Protocol Implementation (AgentGateway)
Writing a full MCP implementation from scratch using raw `websocket-sans-io` and `hyper` is heroic but fragile. As MCP evolves (and it is evolving rapidly), maintenance becomes a treadmill. **Plug should use rmcp or the official Rust SDK** and contribute upstream when features are missing.

### 4. Desktop-First Architecture (mcpproxy-go)
System tray integration, DMG installers, and platform-specific binaries add enormous complexity. **Plug should be server-first, CLI-first.** Desktop integration should be a thin shell over the core.

### 5. JavaScript Sandboxing for Composition (Docker)
`code-mode` runs arbitrary JavaScript in a sandbox to compose tools. This is creative but introduces an entire runtime dependency and security attack surface. **Wasm would be a safer choice for Rust-based composition.**

### 6. Over-Engineering LLM Translation (AgentGateway)
AgentGateway translates between OpenAI/Anthropic/Bedrock/Vertex/Gemini APIs. This is tangential to the MCP proxy mission and creates a massive maintenance burden. **Plug should stay focused on MCP.**

---

## 8. Recommended Patterns for Plug

### Tier 1: Must-Have (Core Architecture)

**1. Provider + Transform Architecture (from FastMCP)**
Implement Rust traits for `Provider` and `Transform`:
- `Provider` sources tools/resources/prompts from upstream MCP servers, local definitions, or OpenAPI specs
- `Transform` reshapes the component catalog (namespace, rename, filter, version)
- Separate from Tower middleware which handles request-level concerns (auth, logging, rate limiting)

**2. Tool Name Prefixing for Multiplexing (from AgentGateway)**
Use `{backend}_{tool}` naming to prevent collisions when federating multiple MCP servers. Simple, proven, reversible.

**3. Fan-Out/Merge for tools/list (from AgentGateway)**
Parallel discovery across all backends with response merging. Essential for multiplexing.

**4. Tower Middleware Stack (from Rust ecosystem)**
Use Tower for the request processing pipeline: auth, rate limiting, logging, error handling. This is idiomatic Rust and composable.

### Tier 2: High Value (Differentiators)

**5. Primordial Tools for Dynamic Discovery (from Docker)**
Expose meta-tools (`plug-find`, `plug-add`, `plug-remove`) that let agents self-configure. This is the most forward-looking pattern in the ecosystem.

**6. BM25 + Semantic Hybrid Retrieval (inspired by mcpproxy-go)**
BM25 as baseline with optional embedding-based semantic search. Expose via `retrieve_tools` meta-tool. Target >70% accuracy (vs. mcpproxy's 43%).

**7. TOON Compression for Responses (from ContextForge)**
Apply TOON encoding to tool responses containing tabular/array data. A Rust implementation already exists. Can be a Transform that runs on tool output.

**8. CEL-Based Authorization (from AgentGateway)**
CEL expressions for fine-grained access control on tools, resources, and prompts. Evaluated at compile-time for zero runtime overhead.

### Tier 3: Important (Production Readiness)

**9. OpenTelemetry Integration (from ContextForge/FastMCP)**
Native OTLP tracing with spans for tool calls, resource reads, and gateway operations. Vendor-agnostic.

**10. External Session State Trait (learning from everyone's mistakes)**
Define a `SessionStore` trait with in-memory default but Redis/DynamoDB/etc. implementations available. No competitor does this well.

**11. Tool Quarantine (from mcpproxy-go)**
Security-first: new/untrusted tools are quarantined until approved. Prevents tool poisoning attacks.

**12. OS-Native Keyring for Secrets (from mcpproxy-go)**
Use platform keyrings for credential storage with `${keyring:name}` placeholders in config.

### Conceptual Rust Sketch

```rust
// Core traits that implement the Provider+Transform pattern

#[async_trait]
pub trait Provider: Send + Sync + 'static {
    async fn list_tools(&self) -> Result<Vec<ToolDef>>;
    async fn list_resources(&self) -> Result<Vec<ResourceDef>>;
    async fn list_prompts(&self) -> Result<Vec<PromptDef>>;
    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<ToolResult>;
    async fn read_resource(&self, uri: &str) -> Result<ResourceContent>;
    async fn get_prompt(&self, name: &str, args: HashMap<String, String>) -> Result<PromptResult>;
}

#[async_trait]
pub trait Transform: Send + Sync + 'static {
    // Catalog-shaping (what exists)
    async fn transform_tools(&self, tools: Vec<ToolDef>) -> Vec<ToolDef> { tools }
    async fn transform_resources(&self, resources: Vec<ResourceDef>) -> Vec<ResourceDef> { resources }
    async fn transform_prompts(&self, prompts: Vec<PromptDef>) -> Vec<PromptDef> { prompts }
    // Name resolution (reverse mapping)
    fn resolve_tool_name(&self, name: &str) -> Option<String> { Some(name.to_string()) }
}

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn create_session(&self, id: &str) -> Result<Session>;
    async fn get_session(&self, id: &str) -> Result<Option<Session>>;
    async fn destroy_session(&self, id: &str) -> Result<()>;
}

// Concrete implementations
struct McpServerProvider { /* rmcp client connection */ }
struct NamespaceTransform { prefix: String }
struct VersionFilterTransform { min: Version, max: Version }
struct InMemorySessionStore { /* DashMap<String, Session> */ }
struct RedisSessionStore { /* redis connection */ }
```

### Architecture for Plug

```
  AI Client
       |
       | stdio / SSE / Streamable HTTP
       v
  +--------------------------------------------+
  | Plug Gateway (Rust)                        |
  |                                            |
  | +----------+  +----------+  +-----------+  |
  | |Primordial|  |Tower     |  |Session    |  |
  | |Tools     |  |Middleware|  |Manager    |  |
  | |(find/add)|  |(auth,log)|  |(trait-    |  |
  | +----+-----+  +----+-----+  | based)    |  |
  |      |             |        +-----------+  |
  |      v             v                       |
  | +----------------------------------+       |
  | |        Transform Pipeline        |       |
  | | [Namespace]->[Filter]->[Version] |       |
  | +----------------------------------+       |
  |      |             |            |          |
  | +----v----+  +-----v----+ +----v-----+    |
  | |Provider | |Provider  | |Provider  |     |
  | |(MCP via | |(OpenAPI) | |(Local)   |     |
  | | rmcp)   | |          | |          |     |
  | +---------+ +----------+ +----------+    |
  +--------------------------------------------+
       |              |              |
  +----v----+   +-----v----+  +-----v-----+
  | MCP Srv |   | REST API |  | Local Fn  |
  +---------+   +----------+  +-----------+
```

---

## Appendix: Source Index

| Project | Repository | Key Files / Docs |
|---------|-----------|-----------------|
| AgentGateway | https://github.com/agentgateway/agentgateway | `crates/agentgateway/src/mcp/session.rs`, `mcp/relay.rs`, `mcp/authorization.rs`, `store/binds.rs`, `proxy/httpproxy.rs` |
| AgentGateway Docs | https://agentgateway.dev | Multiplexing: `/docs/local/latest/mcp/connect/multiplex/` |
| AgentGateway Review | https://dev.to/spacewander/agentgateway-review-a-feature-rich-new-ai-gateway-53lm | Session management critique, CEL auth details |
| AgentGateway DeepWiki | https://deepwiki.com/agentgateway/agentgateway | Full architecture analysis |
| mcpproxy-go | https://github.com/smart-mcp-proxy/mcpproxy-go | BM25 search, `retrieve_tools` meta-tool |
| mcpproxy-go Docs | https://docs.mcpproxy.app/ | Configuration, CLI reference |
| MCP Proxy Pattern | https://dev.to/algis/mcp-proxy-pattern-secure-retrieval-first-tool-routing-for-agents-247c | BM25 accuracy benchmarks (13.62% vs 43.13%) |
| FastMCP 3.0 | https://github.com/jlowin/fastmcp | Provider/Transform architecture |
| FastMCP 3.0 Blog | https://www.jlowin.dev/blog/fastmcp-3-whats-new | Architecture deep-dive |
| FastMCP Middleware | https://gofastmcp.com/servers/middleware | Middleware vs Transform distinction |
| Docker MCP Gateway | https://github.com/docker/mcp-gateway | Gateway implementation (Go) |
| Docker Dynamic MCP | https://docs.docker.com/ai/mcp-catalog-and-toolkit/dynamic-mcp/ | Primordial tools pattern |
| Docker MCP Gateway Docs | https://docs.docker.com/ai/mcp-catalog-and-toolkit/mcp-gateway/ | Architecture, tool routing |
| Docker Blog | https://www.docker.com/blog/docker-mcp-gateway-secure-infrastructure-for-agentic-ai/ | Design philosophy |
| IBM ContextForge | https://github.com/IBM/mcp-context-forge | Plugin architecture, TOON integration |
| ContextForge Docs | https://ibm.github.io/mcp-context-forge/ | Federation, observability |
| TOON MCP Server | https://github.com/aj-geddes/toon-context-mcp | TOON format specification |
| TOON in Rust | https://dev.to/copyleftdev/optimizing-llm-context-windows-reducing-token-usage-by-40-with-toon-and-rust-1j10 | Rust implementation, benchmarks |
| TOON Rust Repo | https://github.com/copyleftdev/toon-mcp | MIT, Rust 1.70+, serde-based |
| rmcp (Rust MCP SDK) | https://docs.rs/rmcp/latest/rmcp/ | Official Rust MCP SDK |
