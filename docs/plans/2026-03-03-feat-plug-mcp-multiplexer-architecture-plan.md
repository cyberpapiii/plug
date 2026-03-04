---
title: "Plug MCP Multiplexer — Architecture & Implementation Plan"
type: feat
status: active
date: 2026-03-03
---

# Plug MCP Multiplexer — Architecture & Implementation Plan

## Overview

Plug is a single-binary Rust MCP multiplexer that sits between N AI coding clients and M upstream MCP servers. One install, one config, every client connected, every server shared. This plan incorporates findings from 6 parallel deep-research agents that resolved 29+ open questions, plus 8 parallel review agents that stress-tested every architectural decision.

## Enhancement Summary

This plan was deepened by 8 parallel research/review agents:

| Agent | Key Contributions |
|-------|-------------------|
| **Simplicity** | 12 YAGNI violations found. Recommends 40-50% LOC reduction. Collapse ToolRouter, kill event bus, strip Phase 1. |
| **Performance** | Split tool cache (routing vs definitions). Request coalescing for tools/list. Semaphore(1) for stdio Phase 1. |
| **Security** | 19 findings (4 Critical, 5 High). No HTTP auth, child process orphan prevention wrong, env var expansion injection. |
| **Architecture** | 14 recommendations. Abstract rmcp behind traits, ArcSwap for tool cache, session draining state. |
| **Technical Architect** | Resequence phases. EngineCommand actor pattern. 3-crate workspace. 4-layer testing. 8 missing items. |
| **Pattern Recognition** | Critical naming inconsistency (fanout vs plug). DashMap anti-pattern for uniform reads. |
| **MCP Builder** | 10 MCP-specific recommendations. Forward annotations, structuredContent. All logging to stderr. |
| **DevOps** | Full CI workflow design. cargo-dist in Phase 1 not Phase 5. Mock MCP server framework. Binary size gates. |

---

## Problem Statement

Power users in 2026 use 5-15 AI coding clients. Each needs MCP servers configured independently, runs its own copies, conflicts on ports, and scatters config across a dozen files. The result: configuration hell, resource waste, and inconsistent tool availability.

## Proposed Solution

A Rust binary (`plug`) that presents itself as a single MCP server to all downstream clients while managing connections to all upstream MCP servers. Built on `rmcp` 0.16.0 (the official Rust MCP SDK), using the proxy pattern validated by AgentGateway and `rmcp-proxy`.

---

## Research Findings Summary

### Critical Findings That Changed The Plan

#### 1. rmcp Proxy Pattern: VALIDATED (was uncertain)
- `ServerHandler` + `ClientHandler` coexist cleanly in one binary
- AgentGateway uses rmcp 0.16 and implements exactly our proxy pattern
- `rmcp-proxy` crate provides additional reference implementation
- No custom transport bridge needed — proxy works at application level
- **Decision**: Use rmcp 0.16.0 as foundation. Model after AgentGateway's `Relay` + `UpstreamGroup` and `rmcp-proxy` crate.
- Source: `docs/research/rmcp-feasibility.md`

#### 2. inputSchema is REQUIRED — Token Strategy Must Change (was assumed optional)
- The MCP spec JSON schema defines `required: ["name", "inputSchema"]`
- Even no-parameter tools must include `{ "type": "object" }`
- **Our lazy schema strategy (91% token reduction) is NOT spec-compliant**
- Optional fields we CAN omit: `title`, `description`, `outputSchema`, `annotations`, `icons`
- **Decision**: Pivot to description-only token reduction. Implement `$ref`-based deduplication for shared schema fragments. Lean on client-side tool search.
- Source: `docs/research/mcp-spec-deep-dive.md`, MCP spec schema.json

#### 3. Session State is Per-Session — Cannot Share Upstream Sessions (was assumed shareable)
- `logging/setLevel` mutates persistent state
- `resources/subscribe` creates persistent subscriptions
- **If Client A sets debug logging, Client B would also get debug logs on a shared session**
- **Decision**: Each downstream client gets its own upstream session. N×M upstream sessions. Phase 4 daemon can pool with per-client state tracking.
- Source: `docs/research/mcp-spec-deep-dive.md`

#### 4. Cursor 40-Tool Limit is ELIMINATED (was documented as hard limit)
- Cursor released "Dynamic Context Discovery" January 2026
- Users report 80+ tools working without warnings
- **Decision**: Remove hard 40-tool filter. Keep configurable limits for Windsurf (100) and VS Code Copilot (128).
- Source: `docs/research/client-validation.md`

#### 5. Three Client Docs Were WRONG
- Gemini CLI supports ALL transports (stdio, SSE, Streamable HTTP)
- OpenCode now supports Streamable HTTP
- Claude Desktop `clientInfo.name` is `claude-ai`
- **Decision**: Update CLIENT-COMPAT.md with corrected matrix and verified `clientInfo.name` values.
- Source: `docs/research/client-validation.md`

#### 6. Legacy SSE Must Be Hand-Implemented
- rmcp does not provide standalone SSE transport
- AgentGateway implemented their own `LegacySSEService`
- **Decision**: Implement legacy SSE based on AgentGateway's reference. Lower priority since OpenCode now supports Streamable HTTP.
- Source: `docs/research/rmcp-feasibility.md`

#### 7. Key Crate Changes
- `backoff` → `backon` v1.6.0 (unmaintained)
- `tower-circuitbreaker` → `tower-resilience` v0.7 (deprecated)
- `flow-guard` → `tokio::sync::Semaphore` (overkill)
- `figment` needs custom `$VAR` post-processor (~50 LOC)
- `tui-logger` conflicts with ratatui 0.30 → custom widget
- Source: `docs/research/crate-validation.md`

#### 8. "plug" Name Conflicts
- Homebrew cask, plug.dev, GitHub org all taken
- **Decision**: Use `plug-mcp` for GitHub. Binary name remains `plug`.
- Source: `docs/research/crate-validation.md`

#### 9. Competitive Patterns Worth Adopting
- FastMCP's Provider+Transform architecture
- AgentGateway's relay pattern
- `rmcp-proxy` crate as additional reference
- **Decision**: Adopt Provider+Transform internally. Model proxy after AgentGateway.
- Source: `docs/research/competitive-architecture.md`

---

## Technical Approach

### Architecture

The core proxy uses rmcp's `ServerHandler` trait facing downstream and `RunningService<RoleClient>` instances for each upstream:

```
Downstream Clients          Plug Engine              Upstream Servers
                     ┌──────────────────────┐
Claude Code ─stdio──►│  ProxyHandler        │──stdio──► github
Cursor ──────stdio──►│  impl ServerHandler   │──stdio──► filesystem
Gemini CLI ──http───►│                      │──stdio──► postgres
Codex ───────stdio──►│  upstreams: HashMap<  │──http───► notion
OpenCode ────http───►│    String,            │──http───► remote-api
                     │    RunningService<    │
                     │      RoleClient>>     │
                     └──────────────────────┘
```

**Key architectural decisions:**
- Per-client upstream sessions (N×M model, not N:1)
- Application-level proxy (not transport bridge)
- Phase 1: Embedded mode (each `plug connect` independent)
- Phase 4: Shared daemon via Unix socket IPC
- Consistent naming: `plug` everywhere (not "fanout")

> **Research Insight (Simplicity)**: Collapse the 4-tier ToolRouter to a simple `HashMap<String, String>` + prefix extraction. The negative cache and fan-out tiers add complexity for near-zero real-world benefit — tools don't appear and disappear frequently. Populate cache eagerly at startup and on `list_changed`.

> **Research Insight (Architecture)**: Abstract rmcp behind internal traits (`trait McpClient`, `trait McpServer`) to insulate from SDK breaking changes. This adds ~200 LOC but protects against the rmcp 0.12→0.16→1.0 migration pattern repeating.

> **Research Insight (Performance)**: Split tool cache into two structures: (1) routing cache `DashMap<Arc<str>, Arc<str>>` for tool→server lookup, and (2) definition cache `ArcSwap<Vec<Tool>>` for the merged tools/list response. Routing cache is hit on every tools/call. Definition cache is hit on tools/list and rebuilt on list_changed.

> **Research Insight (Security — CRITICAL)**: `setsid` for child process cleanup is WRONG. `setsid` creates a NEW process group, making children HARDER to kill on parent exit. On Linux, use `PR_SET_PDEATHSIG` to auto-SIGTERM children when parent dies. On macOS, use `kqueue` with `EVFILT_PROC` + `NOTE_EXIT`. tokio's `kill_on_drop` sends SIGKILL (ungraceful). Implement explicit graceful shutdown: close stdin → wait 5s → SIGTERM → wait 5s → SIGKILL.

### Implementation Phases

#### Phase 1: Core Proxy (MVP)

**Goal**: Working MCP multiplexer over stdio. `plug connect` → multiple upstream servers → tool routing.

**Tasks:**
- [ ] Cargo workspace: `plug` binary + `plug-core` library + `plug-test-harness`
- [ ] CI: GitHub Actions with fmt/clippy/test/cross-check/binary-size/safety jobs
- [ ] `cargo dist init` — generate release workflow from day one (not Phase 5)
- [ ] `#![forbid(unsafe_code)]` in both crate roots
- [ ] `cargo-deny` configuration for license and advisory checks
- [ ] Release profile: `strip=true, lto="fat", codegen-units=1, opt-level="s", panic="abort"`
- [ ] Config: Figment layered loading (TOML + env + CLI) with custom `$VAR` post-processor
- [ ] Server management: Spawn stdio servers, initialize, store capabilities
- [ ] Child process cleanup: close stdin → 5s → SIGTERM → 5s → SIGKILL (NOT setsid)
- [ ] On Linux: `PR_SET_PDEATHSIG(SIGTERM)` for child processes
- [ ] ProxyHandler: impl ServerHandler with fan-out `list_tools()` and routed `call_tool()`
- [ ] Tool routing: `HashMap<String, String>` + prefix extraction (simple, not 4-tier)
- [ ] Eagerly populate tool cache at startup and on `list_changed`
- [ ] Request ID remapping: client req ID ↔ upstream req ID (per-session counter)
- [ ] Notification forwarding: `list_changed` → invalidate cache → re-fan-out → notify clients
- [ ] `plug connect`: stdio bridge command for client invocation
- [ ] Client detection from `clientInfo.name` (exact match primary, fuzzy fallback)
- [ ] `plug status --output json`: server health
- [ ] `plug server list`, `plug tool list`: basic CLI
- [ ] `resources/list` always returns `{resources: []}` (never errors — Codex compat)
- [ ] `prompts/list` returns empty list instantly (Gemini CLI compat)
- [ ] All logging to stderr (stdout is MCP protocol only)
- [ ] Semaphore(1) for stdio servers in Phase 1 (single-threaded safety)
- [ ] Forward tool annotations from upstream servers unchanged
- [ ] Mock MCP server binary for integration tests
- [ ] Integration tests: basic_connect, multi_upstream, multi_client, upstream_crash

**Validation:**
- [ ] Connect Claude Code via `.mcp.json` → tools/list and tools/call work
- [ ] Two Claude Code instances simultaneously → no conflicts
- [ ] Kill one upstream server → others keep working
- [ ] Codex connects → `resources/list` returns `{resources: []}` (not error)
- [ ] Gemini CLI → `prompts/list` responds in <100ms
- [ ] Binary size < 10 MB (CI gate)
- [ ] Tool call overhead < 5ms for cached routes

> **Research Insight (DevOps)**: Add cargo-dist and CI in Phase 1, not Phase 5. Without automated builds, you can't validate binary size, cross-compilation, or release artifacts for the first four phases. Also add a `[profile.ci]` with `lto="thin"` for faster CI builds.

> **Research Insight (Simplicity)**: Kill the event bus. Use `tracing` spans and structured fields for observability. When the TUI arrives in Phase 4, subscribe to tracing events directly. This eliminates the `broadcast::Sender<Event>` and all event types (~150 LOC saved).

> **Research Insight (Technical Architect)**: Add basic resilience to Phase 1 (startup timeouts per server, graceful degradation when a server fails to start). Don't defer ALL resilience to Phase 3 — a server that hangs on init blocks everything.

> **Research Insight (Security)**: Validate env var references in config — use allowlist pattern (`$[A-Z_]+`) to prevent shell injection via `$(...command...)` in TOML values.

#### Phase 2: HTTP + Transports

**Goal**: Streamable HTTP server for remote clients. Legacy SSE for backwards compat.

**Tasks:**
- [ ] Axum server on port 3282 with Streamable HTTP (`/mcp` endpoint)
- [ ] POST /mcp: JSON-RPC requests → JSON or SSE stream responses
- [ ] GET /mcp: server-initiated SSE stream for notifications
- [ ] DELETE /mcp: session termination
- [ ] MCP-Session-Id generation (UUID v4) and validation
- [ ] MCP-Protocol-Version header handling
- [ ] Origin header validation — strict allowlist, not just localhost check
- [ ] Bind to 127.0.0.1 by default (configurable, warn on 0.0.0.0)
- [ ] Legacy SSE: GET /sse → endpoint event (lower priority, based on AgentGateway)
- [ ] `.localhost` subdomain routing via Host header (NOT axum-extra Host extractor — deprecated)
- [ ] Streamable HTTP client for remote upstream servers
- [ ] Session timeout (30 min inactivity, configurable)
- [ ] Last-Event-ID replay with configurable buffer size

**Validation:**
- [ ] Gemini CLI connects via HTTP → tool discovery within 60s
- [ ] OpenCode connects via Streamable HTTP (or SSE fallback)
- [ ] Multiple HTTP + stdio clients simultaneously
- [ ] Origin header validation blocks cross-origin requests

> **Research Insight (Security — CRITICAL)**: The axum-extra `Host` extractor is deprecated (GitHub issue #3442). Parse the Host header manually from the request. Also: `X-Forwarded-Host` can be set by any client — do NOT trust it for security decisions. Only use the `Host` header from the direct connection.

> **Research Insight (Security)**: Rate-limit session creation (max 10 sessions/minute) to prevent session exhaustion. Session IDs must use `getrandom`-backed UUID v4, not `thread_rng`.

#### Phase 3: Resilience + Token Efficiency

**Goal**: Circuit breakers, health checks, client-aware filtering.

**Tasks:**
- [ ] `tower-resilience` v0.7 circuit breaker per upstream (50% failure → open, 30s cooldown)
- [ ] Health checks: periodic ping (60s + jitter), state machine (Healthy → Degraded → Failed)
- [ ] `backon` v1.6.0 exponential backoff for reconnection
- [ ] Per-server concurrency: Semaphore (default 1 for stdio, configurable for HTTP)
- [ ] Client-aware tool filtering: Windsurf 100, VS Code 128
- [ ] Priority sorting: usage frequency → config priority_tools → alphabetical
- [ ] Tool description truncation for token savings (optional, opt-in)
- [ ] Request coalescing for concurrent tools/list calls (singleflight pattern)
- [ ] Forward `structuredContent` in tool responses if present
- [ ] Forward `isError` flag in tool call results

**Validation:**
- [ ] Kill/restart upstream → circuit breaker opens/closes
- [ ] Windsurf with >100 tools → only 100 served
- [ ] Tool call overhead < 5ms for cached routes
- [ ] Concurrent tools/list requests → single upstream fan-out (coalesced)

> **Research Insight (Performance)**: Request coalescing (singleflight) for tools/list is critical. Without it, 5 clients connecting simultaneously trigger 5 × M fan-outs. tower-resilience v0.7 includes a `Coalesce` pattern that deduplicates in-flight requests.

> **Research Insight (MCP Builder)**: Forward `structuredContent` from tool responses — it's becoming standard for rich tool output. Also preserve `isError` flag for proper error rendering in clients.

#### Phase 4: TUI + Daemon

**Goal**: Beautiful ratatui dashboard, shared daemon mode.

**Tasks:**
- [ ] Feature-gate TUI behind `tui` cargo feature (keep `plug connect` binary lean)
- [ ] Ratatui 0.30 + crossterm with async event loop (MSRV 1.86.0)
- [ ] Dashboard: servers panel, clients panel, activity panel
- [ ] Responsive layout: wide (side-by-side), medium (stacked), narrow (tabbed)
- [ ] Custom TUI log widget (~100-200 LOC, subscribes to tracing events directly)
- [ ] Tools view: searchable list with full schema on Enter
- [ ] Log view: structured log with filters
- [ ] Status indicators: ● ◐ ○ ↔ with green/yellow/red
- [ ] Daemon mode: Unix socket IPC at `~/.local/state/plug/plug.sock`
- [ ] PID file at `~/.local/state/plug/plug.pid` (contains PID, timestamp, version)
- [ ] Socket liveness check: read PID → kill -0 → connect → health-check JSON-RPC
- [ ] Auto-start daemon on first `plug connect` if not running
- [ ] `plug connect` becomes thin stdio-to-socket bridge in daemon mode
- [ ] Graceful shutdown: stop accepting → drain in-flight (5s) → close upstream → remove socket/PID
- [ ] Fallback: if daemon unavailable, fall back to embedded mode (not crash)
- [ ] Structured JSON logging to file in daemon mode (tracing-appender, 7-day rotation)

**Validation:**
- [ ] TUI renders at 80×24, 120×40, 200×60
- [ ] Daemon mode: multiple `plug connect` instances share servers
- [ ] Daemon crash → next `plug connect` detects stale socket, cleans up, starts new daemon
- [ ] Binary with TUI feature: still < 10 MB
- [ ] Binary without TUI feature: notably smaller

> **Research Insight (DevOps)**: Feature-gate TUI to keep `plug connect` binary lean. Users who only need the stdio bridge shouldn't link ratatui+crossterm. Build the default binary with TUI but verify size without it.

> **Research Insight (DevOps)**: Daemon crash recovery must be explicit. Socket liveness check: (1) read PID → (2) kill -0 check → (3) socket connect → (4) health-check RPC with 1s timeout → (5) if any fail: kill stale PID, unlink socket, start new daemon.

#### Phase 5: Polish + Distribution

**Goal**: Auto-import, export, doctor, config hot-reload, release pipeline.

**Tasks:**
- [ ] Config auto-import: scan all client config locations, parse each format, deduplicate
- [ ] Config export: generate snippets for each client
- [ ] Config hot-reload via `notify` + SIGHUP
- [ ] "Draining" state for servers being removed: no new requests, wait for in-flight
- [ ] Doctor command: port check, env vars, connectivity, collisions, tool limits
- [ ] `/.well-known/mcp.json` Server Cards endpoint
- [ ] Homebrew tap via cargo-dist
- [ ] Shell/PowerShell install scripts via cargo-dist
- [ ] Changelog
- [ ] README with per-client setup instructions

---

## Alternative Approaches Considered

1. **Roll our own MCP implementation** — Rejected: rmcp is official, maintained, and validated.
2. **N:1 session sharing** — Rejected: MCP session state is per-session. Sharing causes cross-contamination.
3. **Lazy schema loading** — Rejected: inputSchema is REQUIRED by spec. Pivoted to description optimization.
4. **Go instead of Rust** — Rejected: Rust binary size and startup time advantages.
5. **4-tier ToolRouter** — Simplified: HashMap + prefix extraction sufficient. Negative cache and fan-out tiers add complexity for marginal benefit.
6. **Event bus (broadcast channel)** — Replaced: use tracing spans/fields for observability. TUI subscribes to tracing directly.
7. **DashMap for tool definitions** — Replaced: ArcSwap<Vec<Tool>> for the merged list (rebuilt atomically on change), DashMap only for routing cache.

## System-Wide Impact

### Interaction Graph
- `plug connect` stdin → ProxyHandler → N upstream child processes
- Axum HTTP server → ProxyHandler → N upstream connections
- Config file change → notify → ArcSwap<Config> → add/remove/restart servers
- Upstream `list_changed` → invalidate tool cache → re-fan-out → notify all downstream clients

### Error Propagation
- Upstream server crash → ProtocolError (JSON-RPC -32603) to requesting client only
- Upstream timeout → circuit breaker opens → cached tools preserved → auto-recovery
- Config parse error → InternalError logged, existing config preserved
- All errors have error codes, retryable flags, and recovery hints

### State Lifecycle Risks
- Daemon crash: stale PID file + socket. Mitigated by socket liveness check on next start.
- `plug connect` crash: detected via socket EOF, session cleaned up.
- Config reload during active calls: "draining" state — no new requests, wait for in-flight.

### Security Hardening (from 19-finding security audit)

**Critical (must fix before any release):**
1. HTTP endpoints need authentication for non-localhost binds (bearer token or mTLS)
2. Child process orphan prevention: use `PR_SET_PDEATHSIG` (Linux) / `kqueue` (macOS), NOT `setsid`
3. Origin header validation: strict allowlist with `http://localhost`, `http://127.0.0.1`, configured `.localhost` subdomains
4. Session IDs: `getrandom`-backed UUID v4, not `thread_rng`

**High (fix in Phase 1-2):**
5. Env var expansion: allowlist pattern `$[A-Z_][A-Z0-9_]*` to prevent injection
6. Rate-limit session creation (max 10/minute)
7. Config file permissions check on load (warn if >644)
8. Unix socket permissions: 0o600 (owner-only)

---

## Acceptance Criteria

### Functional
- [ ] `plug connect` works as MCP stdio bridge for Claude Code, Cursor, Codex, Zed
- [ ] HTTP server works for Gemini CLI, OpenCode, Claude Desktop
- [ ] Fan-out `tools/list` merges tools from all servers with name prefixing
- [ ] `tools/call` routes to correct server via prefix
- [ ] `resources/list` always returns `{resources: []}` (never errors — Codex compat)
- [ ] `prompts/list` responds in <100ms (Gemini CLI compat)
- [ ] Tool name collision detection with warning
- [ ] Client type detection from `clientInfo.name` (exact match + fuzzy fallback)
- [ ] Tool annotations forwarded unchanged from upstream servers
- [ ] `structuredContent` forwarded in tool responses

### Non-Functional
- [ ] Tool call overhead < 5ms (cached route)
- [ ] Startup to ready < 1 second
- [ ] Binary size < 10 MB (release, stripped)
- [ ] Memory baseline < 50 MB
- [ ] `#![forbid(unsafe_code)]` in all crates

### Quality Gates
- [ ] Integration tests with mock MCP servers
- [ ] Client compatibility test matrix (simulated clientInfo for each client)
- [ ] `--output json` works for every CLI command
- [ ] CI: fmt, clippy, test (3 platforms), cross-check (musl/ARM), binary size gate, cargo-deny
- [ ] All logging to stderr (stdout is protocol-only)

---

## Dependencies & Prerequisites

- Rust stable 1.86.0+ (ratatui 0.30 MSRV)
- rmcp 0.16.0 with features: client, server, macros, schemars, transport-io, transport-child-process, transport-streamable-http-client, transport-streamable-http-server, transport-streamable-http-client-reqwest
- tokio 1.x (full features)
- axum 0.8+ with tower 0.5+
- tower-resilience 0.7 (circuit breaker, coalesce)
- backon 1.6.0 (exponential backoff)

---

## Risk Analysis & Mitigation

See `docs/RISKS.md` for full risk register. Top 5:

1. **rmcp breaking changes** (Medium likelihood, High impact) — Mitigate: pin to 1.0.x, abstract behind internal traits
2. **N×M session scaling** (Medium likelihood, High impact) — Mitigate: lazy session creation, 5-min idle timeout, Phase 4 session pooling
3. **MCP spec June 2026 changes** (High likelihood, Medium impact) — Mitigate: sessions behind trait, support stateful and stateless
4. **Child process orphans on crash** (Medium likelihood, Medium impact) — Mitigate: PR_SET_PDEATHSIG, PID tracking, cleanup on restart
5. **Naming inconsistency** (High likelihood, Low impact) — Mitigate: resolve "plug" vs "fanout" before writing code. Use "plug" everywhere.

---

## Sources & References

### Research Documents
- `docs/research/rmcp-feasibility.md` — SDK proxy validation
- `docs/research/mcp-spec-deep-dive.md` — Spec compliance findings
- `docs/research/daemon-architecture.md` — IPC design (tmux/docker/zellij patterns)
- `docs/research/client-validation.md` — Client compatibility corrections
- `docs/research/crate-validation.md` — Dependency audit
- `docs/research/competitive-architecture.md` — Competitor analysis

### Architecture Decision Records
- `docs/DECISIONS.md` — 10 ADRs covering all major decisions

### External References
- [rmcp SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [rmcp-proxy](https://docs.rs/rmcp-proxy/0.1.0/rmcp_proxy/) — Reference proxy implementation
- [AgentGateway](https://github.com/agentgateway/agentgateway) — Closest Rust competitor
- [MCP Spec 2025-11-25](https://spec.modelcontextprotocol.io/specification/2025-11-25)
- [tower-resilience](https://github.com/joshrotenberg/tower-resilience) — Circuit breaker + 13 resilience patterns
- [papaya](https://github.com/ibraheemdev/papaya) — Lock-free concurrent HashMap (DashMap alternative if deadlocks occur)
- [ratatui async pattern](https://ratatui.rs/tutorials/counter-async-app/async-event-stream/) — Async event stream with tokio
- [Apify MCP Client Capabilities Index](https://github.com/apify/mcp-client-capabilities)
