---
title: "feat: MCP Spec Compliance Roadmap - Protocol Correctness & Connectivity Expansion"
type: feat
status: active
date: 2026-03-07
deepened: 2026-03-07
last_audit: 2026-03-07
---

# MCP Spec Compliance Roadmap

> Roadmap note: This file is the roadmap and planning reference for remaining work. It contains a
> current-status section, but it is not the canonical single-source current-state document. Use
> `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for the current implementation baseline on
> `main`.

## Current Status (audited 2026-03-07)

### Merged on main

| Feature | PRs | Evidence |
|---------|-----|----------|
| Pre-phase: downstream HTTP bearer auth | #25 | `auth.rs`, `http/server.rs:219` |
| Phase A1: logging notification forwarding | #26 | `server/mod.rs:99`, `proxy/mod.rs:2179`, `ipc_proxy.rs:409` |
| Phase A2: structured output pass-through | #27 | `proxy/mod.rs:1614`, `proxy/mod.rs:1734` |
| Phase A3: completion forwarding | #28 | `proxy/mod.rs:1333`, `proxy/mod.rs:2542`, `ipc_proxy.rs:612` |
| Resource subscribe/unsubscribe forwarding | #30 | `proxy/mod.rs:2293`, `http/server.rs:491` |
| Resources/prompts/templates forwarding | (pre-existing) | `proxy/mod.rs:1215`, `proxy/mod.rs:1286`, `daemon.rs:952` |
| Progress/cancellation routing | (pre-existing) | `proxy/mod.rs:504`, `proxy/mod.rs:532` |
| tools/list_changed forwarding | (pre-existing) | `server/mod.rs:36`, `http/server.rs:58` |
| Downstream HTTPS serving | #22 | `runtime.rs:339` |
| Dead TUI dependency removal | #29 | ratatui/crossterm/color-eyre removed from Cargo.toml |

### Partially implemented / follow-up needed

| Item | Status | Tracking |
|------|--------|----------|
| resources/subscribe over daemon IPC | Intentionally unsupported (returns honest error) | `daemon.rs:1205` |
| MCP-Protocol-Version on upstream requests | Downstream validation done (PR #31), upstream send-side not yet | -- |
| Runtime hot-reload beyond server add/remove | "restart required" boundary | `reload.rs:135` |
| Daemon IPC notification parity | Only logging push frames; progress/cancelled/list_changed not pushed over IPC | -- |

### Implemented by PR #31 (moved from partial/missing)

| Item | PR | Notes |
|------|----|-------|
| Stale subscriptions after route refresh | #31 | Subscription pruning + rebind in `refresh_tools()`, todo 039 closed |
| resources/list_changed forwarding | #31 | Coalesced refresh + fan-out (stdio/HTTP), IPC masked to `false` |
| prompts/list_changed forwarding | #31 | Coalesced refresh + fan-out (stdio/HTTP), IPC masked to `false` |
| MCP-Protocol-Version downstream validation | #31 | `validate_protocol_version_for_post()` in `http/server.rs` |
| HTTP completion/complete handler | #31 | `CompleteRequest` branch in `handle_request()` |

### Not implemented in code yet

- sampling/createMessage
- elicitation/create
- roots/list
- Legacy SSE upstream transport
- OAuth / remote commercial MCP auth flows
- Broader Stream B connectivity-expansion work

---

## Enhancement Summary

**Deepened on:** 2026-03-07
**Research agents used:** 7 (rmcp SDK audit, OAuth best practices, architecture review, security audit, institutional learnings, SSE transport research, performance analysis)

### Key Improvements from Research
1. **Split logging into separate broadcast channel** -- prevents log volume from causing `Lagged` loss of Progress/Cancelled signals
2. **rmcp 1.1.0 has NO legacy SSE transport** -- `client-side-sse` is internal; need custom transport via `reqwest-eventsource`
3. **rmcp confirms all other features exist** -- `Tool.output_schema`, `CallToolResult.structured_content`, `Content::ResourceLink`, `ClientHandler::on_logging_message/create_elicitation/list_roots/create_message`
4. **Downstream HTTP needs auth before remote access** -- bearer token at minimum; existing `generate_auth_token` pattern is a ready-made template
5. **OAuth requires RFC 8707 resource indicators** -- MCP-specific addition to standard OAuth 2.1
6. **Existing client-response stub at `http/server.rs:192-195`** -- B3 elicitation infrastructure partially exists

### New Considerations Discovered
- OAuth module boundary: token lifecycle in `plug-core`, browser flow in `plug` CLI (daemon cannot open browser)
- Token refresh crash safety: write new refresh token to disk BEFORE using new access token
- Default log level should be `warning` to prevent broadcast channel overflow
- SSE auto-detection: POST first, GET fallback on 4xx (matches MCP spec exactly)

---

## Overview

Implement the highest-impact missing MCP spec features in plug, organized into two work streams: **Stream A** (protocol correctness -- low effort, benefits every session) and **Stream B** (connectivity expansion -- higher effort, unlocks remote access from phone). This plan is informed by four parallel research reports covering adoption data, protocol compatibility, competitor analysis, and UX impact assessment.

Out of 20 originally identified missing features, this plan covers the **10 that matter**. The remaining 10 are explicitly deferred with rationale.

## Problem Statement / Motivation

plug currently handles the core multiplexing job well (stdio + Streamable HTTP, tool routing, daemon lifecycle), but has gaps in MCP spec compliance that cause three categories of problems:

1. **Silent data loss**: `outputSchema` is actively stripped from tool definitions (`strip_optional_fields()` at `plug-core/src/proxy/mod.rs:1608-1627`). Server log messages are swallowed. Clients lose visibility into upstream server behavior.

2. **Connectivity gaps**: Cannot connect to the long tail of SSE-only remote servers (Neon, Firecrawl, Figma, Linear, Atlassian). Cannot authenticate to OAuth-protected commercial servers (Snowflake, Microsoft 365, Azure). Remote phone access lacks standardized auth.

3. **Protocol incorrectness**: Capabilities are over-advertised (resources/prompts advertised with `list_changed: false` even when not fully forwarded). No `MCP-Protocol-Version` header validation on incoming requests.

## Proposed Solution

### Stream A: Protocol Correctness (3 phases, ~1 week each)

Low-effort changes that improve every session for every client.

### Stream B: Connectivity Expansion (3 phases, ~1-2 weeks each)

Higher-effort features that unlock new use cases (remote servers, phone access).

### Pre-Phase: Downstream HTTP Authentication

Before any remote access deployment, add bearer token authentication to the downstream HTTP server. This is a prerequisite for Stream B's remote access goal.

### Deferred: Tier 4 items with clear "revisit when" triggers.

---

## Technical Approach

### Architecture

All changes build on existing patterns in the codebase:

- **Notification forwarding**: Extends the existing `ProtocolNotification` enum + `broadcast::channel` pattern in `plug-core/src/notifications.rs`. **Research finding: use a separate broadcast channel for logging** (capacity 512-1024) to isolate high-volume log messages from delivery-critical Progress/Cancelled signals. The control channel stays at 128.
- **Structured output**: Remove the `strip_optional_fields()` call that strips `outputSchema`. **Confirmed**: rmcp 1.1.0 has `Tool.output_schema: Option<Arc<JsonObject>>` and `CallToolResult.structured_content: Option<Value>`. Pass-through works automatically.
- **Legacy SSE**: Add `TransportType::Sse` variant alongside existing `Stdio`/`Http`. **Confirmed**: rmcp 1.1.0 does NOT have a legacy SSE client transport (removed before 1.0). Use `reqwest-eventsource` or `eventsource-client` crate to build a custom transport that integrates with rmcp's `Service` trait.
- **Capability gating**: Refine `synthesized_capabilities()` at `plug-core/src/proxy/mod.rs:868-893` to accurately reflect what plug actually forwards. **Note**: Architecture review found that `list_resources`, `read_resource`, `list_prompts`, and `get_prompt` ARE implemented in ProxyHandler (lines 2032-2074), so basic resource/prompt capabilities are correctly advertised. The fix is `list_changed` for resources/prompts.
- **OAuth**: New module split across two crates: token lifecycle in `plug-core/src/auth/` (headless-safe), browser-based auth flow in `plug/src/commands/auth.rs` (CLI-only). Uses `oauth2` 5.x crate with `keyring` for secure storage.

### Implementation Phases

---

#### Pre-Phase: Downstream HTTP Authentication ✅

**Status**: Complete — PR #25 (`feat/pre-phase-http-auth`), plan at `docs/plans/2026-03-07-feat-downstream-http-bearer-auth-plan.md`

**Goal**: Add bearer token authentication to the downstream HTTP server before enabling remote access.

**Tasks:**

- [x] Extract auth utilities to `plug-core/src/auth.rs` (from daemon.rs)
- [x] Generate persistent bearer token at `~/.config/plug/http_auth_token_{port}` with 0600 permissions
- [x] Require `Authorization: Bearer <token>` on all `/mcp` requests when `bind_address` is not loopback
- [x] Backward-compatible: localhost works without auth
- [x] Auth-aware origin validation: authenticated remote clients bypass origin check
- [x] Auth-aware discovery: minimal card for unauthenticated non-loopback requests
- [x] `plug doctor` check: warns if non-loopback without auth token
- [x] `plug status --show-token` surfaces the bearer token

**Acceptance criteria:**
- [x] Non-loopback HTTP server requires bearer token
- [x] Localhost HTTP server works without auth (backward-compatible)
- [x] `plug doctor` warns about unauthenticated non-loopback binding

---

#### Phase A1: Logging & Notification Forwarding -- COMPLETE

**Status**: Merged -- PR #26 (`feat/phase-a1-logging-forwarding`)

**Tasks:**

- [x] **Create a separate logging broadcast channel** -- isolated from control notification channel
- [x] Implement `on_logging_message` callback in `UpstreamClientHandler` with server-prefixed logger names
- [x] Add fan-out branch for logging in stdio consumer with `Lagged` handling
- [x] Add fan-out branch for logging in HTTP consumer (broadcast to all sessions)
- [x] Forward `logging/setLevel` requests from downstream clients to all upstream servers (stdio, HTTP, IPC)
- [x] Multi-client level tracking with most-permissive-wins semantics
- [x] Add `logging` capability to `synthesized_capabilities()` when any upstream supports it
- [x] Per-client log level cleanup on session expiry/disconnect
- [x] IPC push notifications for logging (daemon transport parity)
- [x] Tests: server prefix, level filtering, level lifecycle, channel isolation

**Acceptance criteria:**
- [x] `notifications/message` from any healthy upstream server reaches all connected downstream clients
- [x] Logger name includes server identifier for disambiguation
- [x] `logging/setLevel` propagates to all upstream servers
- [x] `logging` capability correctly advertised downstream
- [x] Log volume does not cause loss of Progress/Cancelled signals (separate channels)

---

#### Phase A2: Structured Output Pass-Through -- COMPLETE

**Status**: Merged -- PR #27 (`feat/phase-a2-structured-output`), plan at `docs/plans/2026-03-07-feat-structured-output-pass-through-plan.md`

**Tasks:**

- [x] Remove `tool.output_schema = None` from `strip_optional_fields()`
- [x] Update doc comment on `strip_optional_fields` to reflect preserved fields
- [x] Update test to verify `output_schema` is preserved
- [x] `structuredContent` in `CallToolResult` passes through (automatic -- proxy returns upstream result as-is)
- [x] `resource_link` content items pass through (automatic via `RawContent::ResourceLink` variant)

**Acceptance criteria:**
- [x] `outputSchema` preserved on all forwarded tool definitions
- [x] `structuredContent` in `CallToolResult` passes through unmodified
- [x] `resource_link` content items in tool results pass through unmodified
- [x] No regression in existing tool forwarding behavior

---

#### Phase A3: Completion Forwarding -- COMPLETE

**Status**: Merged -- PR #28 (`feat/phase-a3-completion-pass-through`)

**Note**: The original plan called this "Protocol Version & Capability Gating". Completion forwarding was prioritized instead as a higher-value protocol correctness fix. Protocol version headers and list_changed forwarding remain as follow-up items (see "Partially implemented" section above).

**Tasks:**

- [x] Forward `completion/complete` requests to the upstream server that owns the referenced prompt/resource
- [x] Advertise `completions` capability when any upstream supports it
- [x] Wire up completion forwarding over stdio, HTTP, and IPC transports
- [x] Tests: capability synthesis, serde roundtrip

**Acceptance criteria:**
- [x] `completion/complete` requests route to correct upstream server
- [x] `completions` capability correctly advertised downstream
- [x] All three transports (stdio, HTTP, IPC) support completion forwarding

---

#### Remaining Stream A Follow-ups

All Stream A follow-ups are complete except one send-side item:

- [ ] **MCP-Protocol-Version on upstream requests**: Ensure header sent on all upstream HTTP requests
- [x] **MCP-Protocol-Version validation on downstream**: Validate header on incoming POST requests (PR #31)
- [x] **resources/list_changed forwarding**: Advertised + forwarded via coalesced refresh (PR #31)
- [x] **prompts/list_changed forwarding**: Advertised + forwarded via coalesced refresh (PR #31)
- [x] **HTTP completion/complete handler**: `CompleteRequest` routing in HTTP server (PR #31)
- [x] **Stale subscription cleanup after route refresh**: Pruning + rebind in `refresh_tools()` (PR #31)
- [x] **IPC capability honesty for list_changed**: Masked to `false` for resources/prompts (PR #31)

---

#### Phase B1: Legacy SSE Upstream Transport

**Goal**: Connect to upstream MCP servers that only speak the deprecated HTTP+SSE transport (2024-11-05).

**Why first in Stream B**: 5/6 competitors support this. Many vendor-hosted servers (Neon, Firecrawl, Figma, Linear, Atlassian) are SSE-only. This is the single most impactful connectivity gap.

**Research insights:**
- **Confirmed: rmcp 1.1.0 does NOT have legacy SSE client transport.** The `client-side-sse` feature is an internal SSE parser for Streamable HTTP, not a standalone transport. The `transport-sse-client` feature existed in pre-1.0 rmcp but was removed.
- **Recommended crate**: `reqwest-eventsource` (158K monthly downloads, wraps reqwest which plug already uses via rmcp) or `eventsource-client` (LaunchDarkly, production-proven)
- **Auto-detection pattern** (per MCP spec): POST first to try Streamable HTTP, GET fallback on 4xx. This is what Claude Desktop and Windsurf do.
- **Note on B1-B2 dependency**: Many legacy SSE servers (Atlassian, Linear) also require OAuth. B1 delivers full value for SSE servers using static API keys. Full SSE value for OAuth servers requires B2.

**Tasks:**

- [ ] Add `reqwest-eventsource` (or `eventsource-client`) dependency to `Cargo.toml`
  - Check `cargo deny` license compatibility before choosing
  - `reqwest-eventsource` reuses existing `reqwest` dependency (smaller footprint)
- [ ] Build a custom legacy SSE transport that integrates with rmcp's `Service` trait
  - Implement the legacy SSE protocol: GET `/sse` for SSE stream, parse `endpoint` event for POST URL, POST JSON-RPC to that URL
  - Wrap in rmcp's transport abstraction so `UpstreamClientHandler` works identically
- [ ] Add `Sse` variant to `TransportType` enum at `plug-core/src/config/mod.rs:189-195`
  - Config: `transport = "sse"` with `url` field (pointing to the server base URL; plug appends `/sse`)
- [ ] Add SSE connection branch in `ServerManager::connect_server()` at `plug-core/src/server/mod.rs`
  - Connection should use the same `auth_token` mechanism as HTTP
  - Use exponential backoff with jitter for reconnection (rmcp's `client-side-sse` module provides `ExponentialBackoff` primitive, or use `backon`)
- [ ] **Auto-detection**: For `transport = "http"` servers, try Streamable HTTP POST first. On 4xx, fall back to legacy SSE GET
  - Log the detected transport type at info level
  - This matches the MCP spec's backwards compatibility procedure exactly
- [ ] Update `plug doctor` to report transport type per upstream server
- [ ] Test: configure an upstream server with `transport = "sse"` -> plug connects and tool calls work
- [ ] Test: `transport = "http"` against an SSE-only server -> auto-detects and falls back

**Key files:**
- `plug-core/src/config/mod.rs` (TransportType enum)
- `plug-core/src/server/mod.rs` (connection logic)
- New: `plug-core/src/transport/sse_client.rs` (custom SSE transport)
- `Cargo.toml` (new dependency)

**Acceptance criteria:**
- [ ] `transport = "sse"` in config connects to legacy SSE servers
- [ ] `transport = "http"` auto-detects and falls back to SSE when needed
- [ ] Tool calls route correctly through SSE transport
- [ ] Notifications from SSE servers are forwarded to downstream clients
- [ ] Auth tokens work with SSE connections
- [ ] Reconnection with exponential backoff on connection loss

---

#### Phase B2: OAuth 2.1 + PKCE + Token Refresh

**Goal**: Authenticate to upstream remote MCP servers that require OAuth. Implement token refresh to prevent session expiry.

**Why second in Stream B**: 3-4/6 competitors support OAuth. Commercial servers (Snowflake, Atlassian, Microsoft 365) increasingly require it. Token refresh is essential -- even Cursor and Copilot CLI are failing because they lack it.

**Research insights:**
- **Recommended crate stack**: `oauth2` 5.x (core OAuth + PKCE), `keyring` 3.x (OS keychain), `open` (browser launch)
- **MCP-specific requirements**: RFC 8707 resource indicators (`resource` param MUST be included in auth + token requests), Protected Resource Metadata discovery (RFC 9728), PKCE S256 mandatory (must verify AS supports it via metadata before proceeding)
- **Module boundary**: Token lifecycle management in `plug-core` (headless-safe), browser auth flow in `plug` CLI. The daemon CANNOT open a browser -- if refresh fails, daemon sets "auth required" state and `plug status` surfaces it.
- **Token refresh crash safety**: Write new refresh token to disk/keychain BEFORE using new access token. A crash between "received tokens" and "wrote to disk" permanently locks out the user.
- **Concurrent refresh serialization**: Use `tokio::sync::Mutex` per server to prevent thundering herd on refresh.
- **Proactive refresh**: Refresh at 80% of `expires_in` window, not on 401. Store absolute `expires_at` timestamps, not relative `expires_in`.

**Tasks:**

- [ ] Add dependencies: `oauth2 = "5"`, `keyring = { version = "3", features = ["apple-native", "linux-native"] }`, `open = "5"`
- [ ] Design OAuth module architecture with clean core/CLI split:
  - `plug-core/src/auth/mod.rs` -- token provider trait, token state machine
  - `plug-core/src/auth/token_store.rs` -- persistent storage (keyring primary, file fallback at `~/.config/plug/tokens/`)
  - `plug-core/src/auth/refresh.rs` -- background refresh loop per server
  - `plug/src/commands/auth.rs` -- browser launch, localhost callback, user prompts
- [ ] Implement OAuth 2.1 + PKCE authorization flow
  - Bind localhost callback on ephemeral port (port 0) to prevent port hijacking
  - PKCE: `PkceCodeChallenge::new_random_sha256()` from `oauth2` crate. Never fall back to `plain`.
  - Validate `state` parameter on callback (cryptographically random, single-use)
  - Include `resource` parameter (RFC 8707) in both authorization and token requests
  - Callback response: simple HTML with `Referrer-Policy: no-referrer`, no external resources
  - Exchange authorization code for access_token + refresh_token
  - **Write tokens to storage BEFORE using them** (crash safety)
  - Wrap all tokens in `SecretString` from moment of receipt
- [ ] Implement Protected Resource Metadata discovery (RFC 9728)
  - On unauthenticated request, parse `WWW-Authenticate` header for `resource_metadata` URL
  - Fetch AS metadata, verify `code_challenge_methods_supported` includes `S256`
  - If S256 not supported, refuse to proceed
- [ ] Implement token refresh loop (in `plug-core`, headless-safe)
  - Background `tokio::spawn` per OAuth server
  - Refresh at `expires_at - 5 minutes` (proactive, not reactive)
  - Use `backon` for retry with exponential backoff (3 retries)
  - Store current token in `ArcSwap<TokenSet>` per server -- tool call handlers read without blocking
  - On permanent failure (`invalid_grant`): mark server as "auth required" (distinct from "unhealthy"), stop sending requests, surface via `plug status`
  - Serialize concurrent refresh attempts with `tokio::sync::Mutex` per server
- [ ] Config format for OAuth servers:
  ```toml
  [[servers]]
  name = "snowflake"
  transport = "http"
  url = "https://my-org.snowflakecomputing.com/mcp"
  auth = "oauth"
  oauth_client_id = "plug-mcp"
  oauth_scopes = ["mcp:read", "mcp:write"]
  # auth_url and token_url discovered via Protected Resource Metadata
  # Can be overridden manually if discovery fails:
  # oauth_authorization_url = "..."
  # oauth_token_url = "..."
  ```
- [ ] CLI commands:
  - `plug auth login --server <name>` -- trigger OAuth flow (interactive, needs browser)
  - `plug auth login --server <name> --no-browser` -- manual copy-paste flow for headless
  - `plug auth status` -- show token status per OAuth server
  - `plug auth logout --server <name>` -- revoke and delete tokens
- [ ] Integrate with upstream connection: replace static `auth_token` with dynamic token from OAuth module
  - At `plug-core/src/server/mod.rs:353-356`, check if server uses OAuth and get current token from `ArcSwap`
- [ ] Add `plug doctor` checks:
  - Warn if `client_secret` accidentally present in config (public clients should not have one)
  - Warn if token files have permissions wider than 0600
- [ ] Test: configure OAuth server -> `plug auth login snowflake` opens browser -> token stored -> server connects
- [ ] Test: access token expires -> refresh token used automatically -> no interruption
- [ ] Test: refresh token fails -> server marked "auth required" -> `plug auth status` shows it

**Key files:**
- New: `plug-core/src/auth/mod.rs` (token provider trait, state machine)
- New: `plug-core/src/auth/token_store.rs` (keyring + file storage)
- New: `plug-core/src/auth/refresh.rs` (background refresh loop)
- New: `plug/src/commands/auth.rs` (browser flow, CLI interface)
- `plug-core/src/config/mod.rs` (OAuth config fields)
- `plug-core/src/server/mod.rs` (dynamic token injection)

**Acceptance criteria:**
- [ ] OAuth 2.1 + PKCE flow works with at least one real OAuth provider (e.g., GitHub)
- [ ] Tokens stored securely (keychain on macOS, file with 0600 on Linux)
- [ ] Token refresh happens proactively before expiry
- [ ] Failed refresh produces actionable user-facing error via `plug status`
- [ ] Daemon never attempts browser-based re-auth (headless-safe)
- [ ] `plug auth status` shows token status per OAuth server
- [ ] RFC 8707 resource indicators included in auth requests

---

#### Phase B3: Elicitation, Roots Forwarding & SSE Resumability

**Goal**: Forward elicitation requests and roots queries. Add SSE stream resumability for mobile connection reliability.

**Why third in Stream B**: Elicitation is the fastest-growing MCP feature (Codex merged it March 2026, Cursor announced it). Roots forwarding matters for Claude Code (plug's primary client). SSE resumability improves the remote/mobile experience.

**Note**: These three sub-features are independently shippable and can be landed in separate PRs. Elicitation requires the most new infrastructure (reverse-direction requests). Roots is simpler. SSE resumability is independent but directly improves the mobile/phone access experience where connections drop frequently.

**Research insights:**
- **Confirmed**: rmcp `ClientHandler` has `create_elicitation()` returning `Result<CreateElicitationResult, McpError>` and `list_roots()` returning `Result<ListRootsResult, McpError>`. These are request handlers (return values), not fire-and-forget notifications.
- **Existing infrastructure**: `plug-core/src/http/server.rs:192-195` already has a stub for handling client responses to sampling/elicitation. This is partially built.
- **HTTP transport challenge**: For elicitation over HTTP, plug sends the request via the SSE stream, client POSTs a JSON-RPC response back. The existing HTTP handler stub was designed for this.
- **Timeout consideration**: Elicitation requests wait for human input -- may take arbitrarily long. Do not apply plug's standard tool call timeouts to elicitation forwarding.
- **SSE replay buffer**: Use `VecDeque<(u64, Arc<SseEvent>)>` with `partition_point` for O(log n) lookup on monotonic IDs. No external crate needed. `Arc<SseEvent>` avoids cloning event payloads per session.

**Tasks:**

**B3a: Roots forwarding (simplest, ship first):**
- [ ] Implement `list_roots` on `UpstreamClientHandler` to forward to the downstream client
  - Look up the downstream client that owns this upstream connection via `DownstreamCallContext`
  - For stdio: call the downstream `Peer<RoleServer>` to send a `roots/list` request
  - For HTTP: use the existing client-response mechanism at `http/server.rs:192-195`
- [ ] Forward `notifications/roots/list_changed` from downstream clients to upstream servers
  - When the downstream client sends this notification, broadcast to all upstream servers
- [ ] Advertise `roots` capability to upstream servers only when the downstream client supports it

**B3b: Elicitation forwarding (most complex, needs design):**
- [ ] Implement `create_elicitation` on `UpstreamClientHandler`
  - Route to the specific downstream client whose tool call triggered the elicitation
  - Use existing `DownstreamCallContext` for routing (stores transport type + client_id/session_id)
  - For stdio: send elicitation request via `Peer<RoleServer>`, await response
  - For HTTP: send via SSE stream, await POST response (using existing stub at `http/server.rs:192-195`)
  - Do NOT apply standard tool call timeouts -- elicitation waits for human input
- [ ] Handle disconnected client during elicitation
  - If the downstream client disconnects mid-elicitation, return `McpError` to the upstream server
- [ ] Advertise `elicitation` capability downstream only when the downstream client supports it (check client capabilities during initialize)

**B3c: SSE Resumability:**
- [ ] Implement per-session replay buffer using `VecDeque<(u64, Arc<SseEvent>)>`
  ```rust
  struct SseReplayBuffer {
      buf: VecDeque<(u64, Arc<SseEvent>)>,
      max_len: usize,  // default: 1000
  }
  ```
  - Use `partition_point` for O(log n) lookup on monotonic event IDs
  - `Arc<SseEvent>` shared across sessions to avoid cloning event payloads
- [ ] On client reconnect with `Last-Event-ID` header, replay missed events from buffer
- [ ] Graceful degradation: if requested event ID is not in buffer, start fresh (client re-initializes)
- [ ] Session reaper: drop replay buffers for sessions with no activity in 5 minutes (prevent memory leaks from disconnected clients)

**Key files:**
- `plug-core/src/proxy/mod.rs` (elicitation/roots routing)
- `plug-core/src/server/mod.rs` (upstream handler for reverse requests)
- `plug-core/src/http/server.rs` (existing client-response stub at line 192-195)
- `plug-core/src/http/sse.rs` (resumability + replay buffer)
- `plug-core/src/notifications.rs` (new variants if needed)

**Acceptance criteria:**
- [ ] `roots/list` requests from upstream servers are answered with downstream client's roots
- [ ] `notifications/roots/list_changed` propagates to upstream servers
- [ ] Elicitation requests from upstream servers reach the correct downstream client
- [ ] User responses to elicitation flow back to the requesting upstream server
- [ ] Disconnected client during elicitation returns error to upstream (no hang)
- [ ] SSE reconnect with `Last-Event-ID` replays missed events
- [ ] Reconnect without `Last-Event-ID` starts fresh (no error)
- [ ] Stale session replay buffers are cleaned up

---

## Explicitly Deferred Features

These 10 features are deferred based on research findings. Each has a "revisit when" trigger.

| Feature | Rationale | Revisit When |
|---------|-----------|--------------|
| **sampling/createMessage** | 12% client support, stagnant. No major client (Claude, Cursor) supports it. | Claude Desktop or Cursor ships sampling support |
| ~~**resources/subscribe**~~ | ~~Deferred~~ **Now implemented** (PR #30). Stdio + HTTP supported, IPC returns honest error. | -- |
| ~~**completion/complete**~~ | ~~Deferred~~ **Now implemented** (PR #28). All three transports supported. | -- |
| **resource_link namespacing** | Pass-through works today. URI namespacing only needed if plug resolves resources. | Plug adds resource resolution/caching |
| **JSON-RPC batch requests** | Removed from spec in June 2025. Implementing it is non-compliant. | Never (removed from spec) |
| **Dynamic Client Registration** | Deprecated in favor of CIMD (Nov 2025 spec). Ecosystem moved on. | Never (superseded by CIMD) |
| **Per-client rate limiting** | Personal tool, single user. Upstream servers have their own limits. | Plug adds multi-user or team features |
| **Tasks (experimental)** | Experimental in spec. No client or server uses it in production. | Tasks promoted to stable in MCP spec |
| **Legacy SSE downstream** | Plug's downstream uses Streamable HTTP. Phone apps should support it. | Evidence that target phone apps only speak legacy SSE |
| **Downstream OAuth (plug as auth server)** | Downstream auth uses bearer token (pre-phase). Full OAuth is overkill for personal tool. | Standard MCP clients require OAuth to connect to plug |

---

## System-Wide Impact

### Interaction Graph

- **Logging forwarding**: `upstream server -> UpstreamClientHandler.on_logging_message() -> logging broadcast channel (separate, capacity 512) -> stdio fan-out / HTTP fan-out -> downstream client`
- **Structured output**: `upstream server tool definition -> rmcp deserialization (Tool.output_schema: Option<Arc<JsonObject>>) -> strip_optional_fields() (STOP stripping outputSchema) -> merged tool cache -> downstream client`
- **Legacy SSE**: New transport path: `config parse -> ServerManager::connect_server() -> custom SSE transport (reqwest-eventsource) -> same UpstreamClientHandler -> same ToolRouter`. All downstream behavior unchanged.
- **OAuth**: New lifecycle: `plug auth login -> browser flow -> token store (keyring/file) -> daemon reads tokens -> ArcSwap<TokenSet> per server -> token injection into transport headers -> existing connection flow`. Token refresh runs as background task per server.
- **Elicitation**: Reverse request path: `upstream server -> UpstreamClientHandler.create_elicitation() -> DownstreamCallContext lookup -> downstream client (stdio peer / HTTP SSE + POST response) -> response back to upstream`

### Error Propagation

- Logging forwarding errors are non-fatal (drop the notification, log internally). `Lagged` on logging channel is tolerable; `Lagged` on control channel is a bug.
- SSE connection failures use existing retry/backoff (`backon`) with exponential backoff + jitter
- OAuth token refresh failures: mark server as "auth required" (NOT "unhealthy"), surface via `plug status`
- Elicitation with disconnected client: return `McpError` to upstream server immediately
- Capability gating errors are initialization-time (fail-fast, clear error message)

### State Lifecycle Risks

- **Token storage**: OAuth tokens persist across daemon restarts. Risk: stale refresh tokens after long offline periods. Mitigation: detect expired refresh tokens and prompt re-auth. **Crash safety**: write new refresh token before using new access token.
- **SSE event buffer**: Per-session `VecDeque` ring buffer. Memory: ~600 KB for 5 sessions with 1000 events. Session reaper cleans up disconnected clients after 5 minutes.
- **Capability cache**: Capabilities synthesized on tool refresh. Risk: brief window where advertised capabilities don't match reality during server reconnection. Mitigation: existing `healthy_capabilities()` filter already handles this.
- **ArcSwap token state**: Token reads are lock-free (no contention with refresh loop). Atomic swap ensures no torn reads.

---

## Acceptance Criteria

### Functional Requirements

- [x] Downstream HTTP requires bearer token when bound to non-loopback address
- [x] Server log messages flow through plug to downstream clients with server identification
- [x] `logging/setLevel` propagates to upstream servers
- [x] Tool `outputSchema` preserved in forwarded tool definitions (not stripped)
- [x] `structuredContent` in tool results passes through unmodified
- [x] `completion/complete` requests forwarded to correct upstream server
- [x] Resource subscribe/unsubscribe forwarded with lifecycle cleanup
- [ ] `MCP-Protocol-Version` header sent on upstream HTTP requests ~~and validated on downstream~~ (downstream validation done, PR #31)
- [x] Capabilities advertised downstream accurately reflect plug's actual forwarding ability (PR #31: list_changed forwarded for stdio/HTTP, masked for IPC)
- [ ] Legacy SSE upstream servers connectable via `transport = "sse"` config (or auto-detected)
- [ ] OAuth 2.1 + PKCE flow authenticates to remote MCP servers
- [ ] Token refresh prevents session expiry for OAuth-protected servers
- [ ] Elicitation requests from upstream servers reach downstream clients and responses flow back
- [ ] Roots queries from upstream servers answered by downstream client's roots
- [ ] SSE reconnect with `Last-Event-ID` replays missed events

### Non-Functional Requirements

- [ ] Binary size remains under 10MB (CI gate)
- [ ] No new required runtime dependencies (OAuth browser flow uses system browser)
- [ ] Token storage uses restrictive file permissions (0600), keychain preferred on macOS
- [ ] All features are behind existing config -- no behavior changes for users who don't configure new features
- [ ] Log volume does not degrade control-plane notification delivery

### Quality Gates

- [ ] All existing tests pass (no regressions)
- [ ] New integration tests for each phase
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo deny check licenses bans sources` passes (no new problematic licenses)

---

## Success Metrics

- **Logging**: When using MCP Inspector or Claude Desktop developer console, log messages from upstream servers are visible through plug
- **Structured output**: Servers using outputSchema (e.g., database query servers) return typed results through plug
- **Legacy SSE**: Can configure and use Neon, Firecrawl, or Figma MCP servers through plug
- **OAuth**: Can authenticate to at least one OAuth-protected commercial server (GitHub, Snowflake, or Atlassian)
- **Remote access**: Can use MCP tools from phone app via plug's HTTPS server with bearer token + OAuth authentication

---

## Dependencies & Prerequisites

| Dependency | Status | Blocking |
|------------|--------|----------|
| rmcp 1.1.0 `Tool.output_schema` field | **Confirmed exists**: `Option<Arc<JsonObject>>` | Phase A2 (resolved) |
| rmcp 1.1.0 `CallToolResult.structured_content` | **Confirmed exists**: `Option<Value>` | Phase A2 (resolved) |
| rmcp 1.1.0 `ClientHandler::on_logging_message` | **Confirmed exists**: default no-op | Phase A1 (resolved) |
| rmcp 1.1.0 `ClientHandler::create_elicitation` | **Confirmed exists**: returns `Result<CreateElicitationResult>` | Phase B3 (resolved) |
| rmcp 1.1.0 `ClientHandler::list_roots` | **Confirmed exists**: returns `Result<ListRootsResult>` | Phase B3 (resolved) |
| rmcp 1.1.0 `Content::ResourceLink` | **Confirmed exists**: `RawContent::ResourceLink(RawResource)` | Phase A2 (resolved) |
| rmcp 1.1.0 SSE client transport | **Confirmed DOES NOT EXIST** | Phase B1 (need custom transport) |
| Downstream HTTPS serving (PR #22, merged) | Complete | Phase B2 (OAuth) |
| Notification infrastructure (existing) | Complete | Phase A1 |

---

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Legacy SSE needs custom transport** | **Confirmed** | High (effort for B1) | Use `reqwest-eventsource` crate to build custom transport; wrap in rmcp's Service trait |
| OAuth browser flow UX on headless systems | Medium | Medium | `plug auth login --no-browser` for manual copy-paste; device-code flow if AS supports it |
| Token storage security on shared systems | Low | High | OS keychain via `keyring` crate (macOS Keychain, Linux secret-service) with file fallback |
| Broadcast channel overflow with logging | **Addressed** | High | Separate logging channel (capacity 512+) isolates from control-plane notifications |
| OAuth token refresh race conditions | Medium | Medium | `tokio::sync::Mutex` per server to serialize concurrent refresh attempts |
| Elicitation timeout (waiting for human input) | Medium | Medium | Do not apply standard tool call timeouts; let upstream server control timeout |
| Downstream HTTP unauthenticated for remote | **Addressed** | Critical | Pre-phase adds bearer token auth before any remote deployment |
| `cargo deny` rejects new OAuth/SSE dependencies | Low | Medium | Check licenses before adding; `oauth2` is MIT/Apache-2.0, `reqwest-eventsource` is MIT |

---

## Recommended Crate Additions

| Crate | Version | Purpose | License | Phase |
|-------|---------|---------|---------|-------|
| `reqwest-eventsource` | latest | Legacy SSE client transport | MIT | B1 |
| `oauth2` | 5.x | OAuth 2.1 + PKCE | MIT/Apache-2.0 | B2 |
| `keyring` | 3.x | OS keychain token storage | MIT/Apache-2.0 | B2 |
| `open` | 5.x | Open system browser | MIT | B2 |

---

## Execution Order

**Recommended sequence** (Stream A and Stream B can partially overlap):

```
Pre-Phase: Downstream HTTP bearer token auth (1-2 days)
Week 1:    Phase A1 (Logging forwarding with separate channel)
Week 2:    Phase A2 (Structured output -- one-line fix + tests)
           Phase B1 start (SSE transport investigation + custom transport)
Week 3:    Phase A3 (Version/capability gating)
           Phase B1 complete (SSE transport implementation + auto-detection)
Week 4:    Phase B2 design (OAuth architecture, module split)
Week 5:    Phase B2 implementation (OAuth + PKCE + token refresh)
Week 6:    Phase B3a (Roots forwarding -- simplest)
           Phase B3b (Elicitation forwarding -- most complex)
           Phase B3c (SSE resumability -- independent)
```

Stream A phases are independent and can be done by one developer sequentially. Stream B phases have dependencies (B2 needs HTTPS from PR #22, B3b needs notification infrastructure from A1).

**Parallelization opportunity**: A2 is a small change (remove one line + tests). It can be done in parallel with anything.

---

## Sources & References

### Research Reports (Primary Input)

- `docs/research/2026-03-07-mcp-feature-adoption-analysis.md` -- Server/client adoption data for 10 features
- `docs/research/2026-03-07-missing-mcp-features-impact-analysis.md` -- Real breakage vs spec-letter compliance for 10 features
- `docs/research/2026-03-07-competitor-feature-matrix.md` -- 6 competitor feature comparison
- `docs/research/2026-03-07-missing-feature-ux-impact-assessment.md` -- UX impact ranking for all 20 features

### Institutional Learnings Applied

- `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md` -- Non-exhaustive structs, ArcSwap atomic consistency, lock guards across async
- `docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md` -- Notification bus patterns, coalescing, channel sizing
- `docs/solutions/integration-issues/phase2a-notification-fanout-tools-list-changed-20260307.md` -- Non-blocking fan-out (`try_send`), session liveness
- `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md` -- SSE priming event, keep-alive, CancellationToken
- `docs/solutions/integration-issues/downstream-https-serving-20260307.md` -- TLS config, origin validation, SSRF protection

### Internal References

- Notification architecture: `plug-core/src/notifications.rs` (full file)
- outputSchema stripping: `plug-core/src/proxy/mod.rs:1608-1627`
- Capability synthesis: `plug-core/src/proxy/mod.rs:868-893`
- Resource/prompt forwarding: `plug-core/src/proxy/mod.rs:2032-2074`
- Upstream connection: `plug-core/src/server/mod.rs:340-360`
- Protocol version header: `plug-core/src/http/server.rs:27-31`
- Auth token usage: `plug-core/src/server/mod.rs:353-356`
- Client-response stub (elicitation): `plug-core/src/http/server.rs:192-195`
- Daemon auth token pattern: `plug/src/daemon.rs:266-270`

### External References

- MCP Spec 2025-11-25: modelcontextprotocol.io/specification/2025-11-25
- MCP Authorization Spec (draft): modelcontextprotocol.io/specification/draft/basic/authorization
- rmcp crate: crates.io/crates/rmcp (v1.1.0)
- rmcp source: `~/.cargo/registry/src/index.crates.io-.../rmcp-1.1.0/`
- oauth2 crate: docs.rs/oauth2/latest/oauth2/
- keyring crate: crates.io/crates/keyring
- reqwest-eventsource: crates.io/crates/reqwest-eventsource
- MCP client availability matrix: mcp-availability.com
- RFC 8707 (Resource Indicators): tools.ietf.org/html/rfc8707
- RFC 9728 (Protected Resource Metadata): tools.ietf.org/html/rfc9728
