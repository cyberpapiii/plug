# Implementation Plan

## Phased Approach

Five phases, each delivering a working increment. Each phase can be validated independently.

---

## Phase 1: Core Proxy (MVP)

**Goal**: A working MCP multiplexer over stdio. One `fanout connect` command, multiple upstream servers, tool routing. No HTTP, no TUI.

### 1.1 Project Scaffolding
- [ ] Cargo workspace with `fanout` binary crate and `fanout-core` library crate
- [ ] CI: GitHub Actions for build + test on macOS (ARM), Linux (x86), Windows
- [ ] cargo-dist configuration for releases
- [ ] Release profile (strip, lto, codegen-units=1, panic=abort)

### 1.2 Configuration
- [ ] Config struct with serde Deserialize
- [ ] Figment layered loading: defaults → TOML file → env vars → CLI flags
- [ ] Config validation with actionable error messages
- [ ] `fanout config validate` command
- [ ] `fanout config path` command
- [ ] Environment variable references (`$VAR_NAME`) in config values

### 1.3 Server Management (stdio)
- [ ] Spawn upstream MCP servers as child processes (`tokio::process::Command`)
- [ ] Capture stdout (MCP messages) and stderr (server logs) separately
- [ ] Initialize each upstream server (send `initialize`, receive `InitializeResult`, send `initialized`)
- [ ] Store per-server capabilities and tool list
- [ ] Startup concurrency batching (start 3 servers at a time, not all at once)
- [ ] Graceful shutdown: close stdin → wait 5s → SIGTERM → wait 5s → SIGKILL
- [ ] No orphaned child processes on fanout exit (use process groups or signal forwarding)

### 1.4 Tool Routing
- [ ] ToolRouter with 4-tier resolution (cache → prefix → negative cache → fan-out)
- [ ] DashMap tool cache: `tool_name → server_id`
- [ ] Prefix extraction: `github__create_issue` → `github`
- [ ] Negative cache with 30s TTL
- [ ] Fan-out via `tokio::JoinSet` with 25s per-server timeout

### 1.5 Fan-Out & Merge
- [ ] `tools/list`: parallel fan-out to all servers, merge results, cache
- [ ] Merge-based cache: preserve last-known tools when a server times out
- [ ] Tool name prefixing (`__` delimiter, always on in v0.1)
- [ ] Collision detection: warn if two servers define the same unprefixed tool name
- [ ] `resources/list`: merge (always return `{resources: []}` if none — Codex compatibility)
- [ ] `prompts/list`: merge
- [ ] `prompts/get`: route to the server that owns the prompt

### 1.6 Request Routing
- [ ] `tools/call`: resolve server via ToolRouter, forward, return response
- [ ] Request ID remapping: client req ID → upstream req ID → client req ID
- [ ] `resources/read`: route by URI prefix/ownership
- [ ] Error handling: clear JSON-RPC errors for tool not found, server unavailable, timeout

### 1.7 Notification Forwarding
- [ ] `list_changed` from upstream: invalidate cache, re-fan-out, notify all downstream clients
- [ ] `progress` from upstream: forward to the client that initiated the request
- [ ] `cancelled` from downstream: forward to the upstream server

### 1.8 `fanout connect` Command
- [ ] stdio bridge: read MCP from stdin, write to stdout
- [ ] This is what clients invoke: `{"command": "fanout", "args": ["connect"]}`
- [ ] Handle initialize: synthesize capabilities from all upstreams
- [ ] Client type detection from `clientInfo.name`

### 1.9 Basic CLI
- [ ] `fanout status` — server health (pretty + --output json)
- [ ] `fanout server list` — list configured servers
- [ ] `fanout tool list` — list all tools

### 1.10 Validation
- [ ] Connect Claude Code via `.mcp.json` → verify tools/list and tools/call work
- [ ] Connect two Claude Code instances simultaneously → verify no conflicts
- [ ] Kill one upstream server → verify other servers keep working

---

## Phase 2: HTTP + Portless

**Goal**: Streamable HTTP server for remote clients. Legacy SSE for OpenCode. `.localhost` subdomain routing. Session management.

### 2.1 Streamable HTTP Server
- [ ] Axum server on port 3282 (configurable)
- [ ] POST `/mcp` — accept JSON-RPC requests, return JSON or SSE stream
- [ ] GET `/mcp` — server-initiated SSE stream for notifications
- [ ] DELETE `/mcp` — session termination
- [ ] `MCP-Session-Id` generation (UUID v4) and validation
- [ ] `MCP-Protocol-Version` header handling
- [ ] `Accept` header parsing (application/json vs text/event-stream)
- [ ] Origin header validation (DNS rebinding prevention)
- [ ] Bind to 127.0.0.1 by default (configurable)

### 2.2 Legacy SSE Server (Lower Priority — ADR-009)
- [ ] GET `/sse` — return `endpoint` event (old SSE protocol for backwards compat)
- [ ] POST to dynamic endpoint from `endpoint` event
- [ ] Based on AgentGateway's `LegacySSEService` reference implementation
- [ ] Note: OpenCode now supports Streamable HTTP; legacy SSE needed only for very old clients

### 2.3 `.localhost` Subdomain Routing
- [ ] Host header extraction in axum middleware
- [ ] Route `servername.localhost:3282` → direct proxy to that specific server
- [ ] Route `localhost:3282` → main MCP endpoint (aggregated)
- [ ] DashMap route registry: `subdomain → server_id`
- [ ] Auto-register subdomains on server start, remove on server stop

### 2.4 Streamable HTTP Client (for remote upstream servers)
- [ ] reqwest-based client for upstream Streamable HTTP servers
- [ ] Session management (MCP-Session-Id tracking)
- [ ] SSE stream parsing for server-initiated messages
- [ ] Reconnection with Last-Event-ID

### 2.5 Session Management
- [ ] Per-client session tracking (DashMap<SessionId, ClientSession>)
- [ ] Session timeout (configurable, default 30 minutes of inactivity)
- [ ] Proper session cleanup on client disconnect
- [ ] Session resumability for HTTP clients (Last-Event-ID)

### 2.6 HTTP/2 Support
- [ ] Enable HTTP/2 via axum-server + rustls (optional, for performance)
- [ ] Self-signed cert generation via rcgen for `.localhost`

### 2.7 Validation
- [ ] Connect Gemini CLI via stdio or HTTP → verify tool discovery works within 60s
- [ ] Connect OpenCode via Streamable HTTP (or SSE fallback) → verify protocol works
- [ ] Access `github.localhost:3282/mcp` → verify direct server access
- [ ] Multiple HTTP clients + stdio clients simultaneously

---

## Phase 3: Resilience + Token Efficiency

**Goal**: Circuit breakers, health checks, client-aware tool filtering, lazy schemas, tool search.

### 3.1 Circuit Breakers
- [ ] `tower-resilience` v0.7 integration per upstream server (replaces deprecated tower-circuitbreaker)
- [ ] 50% failure rate threshold, 30s open duration, 2 half-open probes
- [ ] Event bus notification on state transitions
- [ ] TUI/log visibility of circuit breaker state

### 3.2 Health Checks
- [ ] Periodic `ping` to each upstream server (interval: 60s + jitter)
- [ ] Health state machine: Healthy → Degraded → Failed
- [ ] Exponential backoff for failed health checks
- [ ] Event bus notification on health transitions

### 3.3 Concurrency Limiting
- [ ] `tokio::sync::Semaphore` for per-server concurrency limits (replaces flow-guard — TCP Vegas overkill for <20 servers)
- [ ] Default: 1 concurrent request for stdio servers, configurable higher for HTTP servers
- [ ] `backon` v1.6.0 for exponential backoff on reconnection (replaces unmaintained backoff crate)

### 3.4 Client-Aware Tool Filtering
- [ ] Client type detection from `clientInfo.name` → exact match (ADR-007), fuzzy fallback
- [ ] Cursor: no limit (Dynamic Context Discovery eliminated 40-tool limit, ADR-005)
- [ ] Windsurf: 100 tools max
- [ ] VS Code Copilot: 128 tools max
- [ ] Priority sorting: usage frequency → config `priority_tools` → alphabetical
- [ ] Event: "Windsurf: serving 100/150 tools (50 filtered)"
- [ ] Log which tools were filtered

### 3.5 Token Efficiency (REVISED — inputSchema is REQUIRED per spec, ADR-003)
- [ ] Omit optional fields from tools/list: `title`, `outputSchema`, `annotations`, `icons` (when not needed)
- [ ] Tool description truncation (opt-in, configurable max length)
- [ ] `$ref`-based schema deduplication for shared schema fragments (inspired by SEP-1576)
- [ ] Lean on client-side tool search (Claude Code already reduces 77K→8.7K tokens)

### 3.6 Tool Search / Catalog Mode
- [ ] `search_tools` meta-tool: search by name, description, category
- [ ] Return top 5-10 matching tools with full schemas
- [ ] Configurable: activate when total tools > threshold (e.g., 50)
- [ ] Research: how does Claude Code's built-in Tool Search work? Can we align with it?

### 3.7 Startup Optimization
- [ ] Concurrent server startup in batches of 3
- [ ] Pre-populate tool cache at startup (don't wait for first client request)
- [ ] Warm cache for `prompts/list` and `resources/list` at startup (Gemini CLI needs fast response)

### 3.8 Validation
- [ ] Kill/restart an upstream server → verify circuit breaker opens/closes
- [ ] Connect Windsurf with > 100 tools → verify only 100 are served
- [ ] Measure tool call overhead → must be < 5ms for cached routes

---

## Phase 4: TUI

**Goal**: Beautiful ratatui dashboard with real-time monitoring.

### 4.1 Core TUI Framework
- [x] Ratatui + crossterm setup with tokio async event loop
- [x] Event stream: crossterm key/mouse events + tick + render + engine events
- [x] Core `App` state struct (separate from Engine — TUI state only)
- [x] Modal navigation system (vim-inspired)

### 4.2 Dashboard Layout
- [x] Responsive layout: wide (side-by-side), medium (stacked), narrow (tabbed)
- [x] Servers panel: name, tool count, latency, health status
- [x] Clients panel: type, session ID, tools served, last activity
- [x] Activity panel: rolling log of MCP requests

### 4.3 Panel Views
- [x] Tools view (full screen): searchable list of all tools with server origin
- [x] Tool detail: full schema, annotations, server, description
- [x] Log view (full screen): structured log with level/server/client filters
- [x] Doctor view: diagnostic results

### 4.4 Interactivity
- [x] Server management: restart, disable/enable from TUI
- [x] Search within any panel (`/`)
- [x] Context-aware keybinding bar
- [x] Help overlay (`?`)

### 4.5 Visual Polish
- [x] Status indicators: ● ◐ ○ ↔
- [x] Colors: green/yellow/red/cyan/dim
- [x] NO_COLOR support
- [x] Smooth status transitions (server connecting → connected)

### 4.6 Headless/Daemon Mode
- [x] Same Engine, no TUI
- [x] PID file for daemon detection
- [x] Unix socket for CLI → daemon communication (`plug status` talks to running daemon)
- [x] Structured logging to file (tracing-appender)

### 4.7 Validation
- [x] TUI renders correctly at 80x24, 120x40, 200x60
- [x] All key bindings work
- [x] Real-time updates visible (connect/disconnect clients and servers)

---

## Phase 5: Polish + Distribution

**Goal**: Auto-import, export, doctor, config hot-reload, tool enrichment, release pipeline.

### 5.1 Config Auto-Import
- [x] Scan all known client config locations (see CLIENT-COMPAT.md)
- [x] Parse each format (Claude JSON, Cursor JSON, Codex TOML, Gemini JSON)
- [x] Deduplicate servers by command + args signature
- [x] First-run interactive import
- [x] `plug import <source>` for individual clients
- [x] `plug import --all --yes` for non-interactive

### 5.2 Config Export
- [x] `plug export claude-desktop` — generate claude_desktop_config.json
- [x] `plug export cursor` — generate .cursor/mcp.json
- [x] `plug export codex` — generate TOML snippet
- [x] `plug export gemini-cli` — generate settings.json snippet

### 5.3 Config Hot-Reload
- [x] Config diff algorithm in `plug-core/src/reload.rs`
- [x] `plug reload` CLI command via daemon IPC
- [x] Diff detection: add new servers, remove deleted, restart changed
- [x] No restart required for settings changes (bind address change = warning)

### 5.4 Tool Enrichment (Optional, Opt-In)
- [x] Auto-inferred annotations from tool name patterns (readOnly, destructive, idempotent)
- [x] Tool name normalization (human-readable titles from `snake_case` names)
- [x] Fill-in only — never overrides upstream annotations

### 5.5 Doctor Command
- [x] Config validation
- [x] Port availability check
- [x] Environment variable presence check
- [x] Server connectivity check (ping each)
- [x] Tool collision detection
- [x] Client tool limit warnings

### 5.6 Release Pipeline
- [x] cargo-dist configuration
- [x] GitHub Actions: build on tag push
- [x] Targets: macOS ARM, macOS Intel, Linux x86 (glibc + musl), Linux ARM, Windows
- [x] Homebrew tap: `brew install plug`
- [x] Shell installer: `curl -fsSL https://get.plug.dev | sh`
- [x] README with installation instructions
- [x] Changelog

### 5.7 Server Cards
- [x] Serve `/.well-known/mcp.json` endpoint
- [x] Include: server name, version, tool count, server list, supported transports
- [x] Ready for June 2026 spec

### 5.8 Validation
- [ ] Full end-to-end: install from Homebrew → import → connect all major clients → tools work
- [x] Binary size < 10 MB
- [x] Startup < 1 second
- [x] Tool call overhead < 5ms

---

## Milestone Summary

| Phase | Deliverable | Key Validation |
|-------|------------|----------------|
| 1 | Working stdio multiplexer | Claude Code → fanout → 4 servers, tool calls work |
| 2 | HTTP + .localhost | Gemini CLI + OpenCode connected alongside stdio clients |
| 3 | Resilient + token-efficient | Circuit breakers tested, Windsurf gets exactly 100 tools |
| 4 | Beautiful TUI | Screenshot-worthy dashboard with real-time updates |
| 5 | Polished + distributable | `brew install fanout && fanout` works end-to-end |
