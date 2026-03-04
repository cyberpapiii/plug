# Architecture Decision Records

Decisions made during pre-implementation research (2026-03-03). Each ADR follows: Context → Decision → Consequences.

---

## ADR-001: Use rmcp as MCP SDK

**Context**: We need both MCP server (downstream) and client (upstream) capabilities. Options: rmcp (official), rust-mcp-sdk, roll our own.

**Decision**: Use rmcp 1.0.0 with features: client, server, transport-child-process, transport-streamable-http-client, transport-streamable-http-server, auth.

**Evidence**: AgentGateway (1,851 stars, Linux Foundation) uses rmcp for exactly our proxy pattern. ServerHandler + ClientHandler coexist cleanly. The `Relay` struct pattern is our reference implementation.

**Consequences**:
- (+) Production-validated proxy pattern
- (+) Official SDK tracks spec changes
- (-) Breaking API changes between versions (0.12→0.16→1.0 had migrations)
- (-) Must implement legacy SSE ourselves (rmcp doesn't provide it)

**Source**: `docs/research/rmcp-feasibility.md`

---

## ADR-002: Per-Client Upstream Sessions (N×M Model)

**Context**: Should N downstream clients share 1 upstream session per server (N:1), or get their own (N:M)?

**Decision**: Each downstream client gets its own upstream session to each server.

**Evidence**: MCP session state is per-session. `logging/setLevel` mutates persistent state. `resources/subscribe` creates persistent subscriptions. If Client A sets debug logging on a shared session, Client B gets debug logs too.

**Consequences**:
- (+) Correct isolation between clients
- (-) More connections: 5 clients × 4 servers = 20 upstream sessions
- (-) More memory and initialization overhead
- Mitigated by Phase 4 daemon which can pool sessions with per-client state tracking

**Source**: `docs/research/mcp-spec-deep-dive.md`

---

## ADR-003: Abandon Lazy Schema Loading

**Context**: We planned to omit `inputSchema` from `tools/list` for 91% token reduction.

**Decision**: Cannot omit inputSchema — it's REQUIRED per the MCP spec JSON schema (`required: ["name", "inputSchema"]`).

**Pivot**:
- Omit optional fields: `title`, `outputSchema`, `annotations`, `icons` (when not needed)
- Implement `$ref`-based schema deduplication (inspired by SEP-1576)
- Lean on client-side tool search (Claude Code already reduces 77K→8.7K tokens)
- Description truncation as opt-in feature

**Consequences**:
- Token reduction is less dramatic than originally planned
- But spec-compliant, which matters more
- Claude Code and Cursor (Dynamic Context Discovery) handle large tool sets well already

**Source**: `docs/research/mcp-spec-deep-dive.md`, MCP spec schema.json

---

## ADR-004: Embedded Mode First, Daemon Later

**Context**: How should multiple `plug connect` instances share upstream servers?

**Decision**: Phase 1 uses embedded mode (each `plug connect` is independent). Phase 4 migrates to shared daemon.

**Evidence**: tmux, Docker, and zellij all use the client-daemon pattern. But starting with embedded mode lets us validate core proxy logic without IPC complexity. Migration is low-risk because Engine is UI-agnostic — just swap StdioTransport for SocketTransport.

**Design for Phase 4**:
- Unix socket at `~/.local/state/plug/plug.sock`
- Length-prefixed JSON messages
- Auto-start daemon on first `plug connect`
- Socket liveness check (not PID file) for daemon detection
- Configurable shutdown: immediate / never / 30s

**Source**: `docs/research/daemon-architecture.md`

---

## ADR-005: Remove Cursor 40-Tool Hard Limit

**Context**: We planned to hard-filter Cursor to 40 tools.

**Decision**: Remove the hard 40-tool filter. Cursor's "Dynamic Context Discovery" (January 2026) effectively eliminates the limit.

**Evidence**: Users report 80+ tools working. 46.9% reduction in agent token usage. The old limit only applies to pre-v2.3 Cursor.

**Remaining limits**: Windsurf (100), VS Code Copilot (128). These are configurable, not hardcoded.

**Source**: `docs/research/client-validation.md`, Cursor blog

---

## ADR-006: Replace Deprecated/Unmaintained Crates

**Context**: Several crates in our stack are deprecated or unmaintained.

**Decisions**:
| Original | Replacement | Reason |
|----------|-------------|--------|
| `backoff` | `backon` v1.6.0 | backoff is unmaintained |
| `tower-circuitbreaker` | `tower-resilience` v0.7 | Deprecated by author |
| `flow-guard` | `tokio::sync::Semaphore` | TCP Vegas overkill for <20 servers |
| `tui-logger` | Custom TUI log widget | Pins ratatui 0.29, conflicts with 0.30 |
| `reqwest` 0.12 | `reqwest` 0.13 | Breaking changes in 0.13 |

**Additional**: Figment does NOT support `$VAR_NAME` interpolation. Need custom post-processor (~50 LOC).

**Source**: `docs/research/crate-validation.md`

---

## ADR-007: Exact-Match Client Detection

**Context**: The fuzzy `name.contains()` detection code was broken for Claude Desktop (`claude-ai` doesn't contain "desktop").

**Decision**: Use exact string match as primary, fuzzy fallback as secondary.

**Confirmed clientInfo.name values**:
| Client | clientInfo.name |
|--------|----------------|
| Claude Code | `claude-code` |
| Claude Desktop | `claude-ai` |
| Cursor | `cursor-vscode` |
| Windsurf | `windsurf-client` |
| VS Code Copilot | `Visual-Studio-Code` |
| Gemini CLI | `gemini-cli-mcp-client` |
| OpenCode | `opencode` |
| Zed | `Zed` |

**Source**: `docs/research/client-validation.md`, Apify MCP Client Capabilities Index

---

## ADR-008: Provider+Transform Internal Architecture

**Context**: How should we structure the tool aggregation pipeline?

**Decision**: Adopt FastMCP's Provider+Transform pattern, adapted for Rust.

**Design**:
- **Providers**: Each upstream server is a Provider sourcing tools/resources/prompts
- **Transforms**: Middleware for the component pipeline (namespace/prefix, filter by client, limit count, deduplicate schemas)
- Explicit separation: Transforms = what components exist; Tower middleware = how requests execute

**Source**: `docs/research/competitive-architecture.md`

---

## ADR-009: Legacy SSE at Lower Priority

**Context**: We planned legacy SSE as P1 for OpenCode compatibility.

**Decision**: Lower priority. OpenCode now supports Streamable HTTP (auto-negotiation). Legacy SSE is only needed for very old clients.

**Implementation**: Based on AgentGateway's `LegacySSEService` when we do implement it.

**Source**: `docs/research/client-validation.md`, `docs/research/rmcp-feasibility.md`

---

## ADR-010: Design for Stateless MCP Future

**Context**: SEP-1442 proposes stateless mode for June 2026. How do we prepare?

**Decision**: Build for stateful model today, but abstract session management behind a trait. Keep per-request context passing as internal model.

**Preparation**:
- Abstract session management behind `trait SessionStore`
- Design upstream connection pool for both sticky and round-robin
- When SEP-1442 is adopted, swap implementation without changing proxy logic

**Source**: `docs/research/mcp-spec-deep-dive.md`
