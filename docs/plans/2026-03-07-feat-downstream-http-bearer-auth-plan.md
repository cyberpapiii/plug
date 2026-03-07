---
title: "feat: Downstream HTTP Bearer Token Authentication"
type: feat
status: complete (2 test items deferred to integration phase)
date: 2026-03-07
parent: docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md
---

# feat: Downstream HTTP Bearer Token Authentication

## Overview

Add bearer token authentication to plug's downstream HTTP server. Required before any remote access deployment (phone clients, remote AI assistants). Backward-compatible: localhost works without auth.

## Problem Statement / Motivation

plug's downstream HTTP server (`plug serve`) has NO authentication. The only protection is origin validation middleware that rejects non-localhost origins. Anyone who can reach the port can create MCP sessions and invoke any tool on any upstream server. This is acceptable for localhost-only use but **critical** for the remote phone access use case that Stream B enables.

## Proposed Solution

### High-Level Approach

1. **Generate a persistent bearer token** on first non-loopback serve, store at `~/.config/plug/http_auth_token_{port}` with 0600 permissions
2. **Add axum auth middleware** in `build_router()` that validates `Authorization: Bearer <token>` on `/mcp` routes
3. **Resolve origin validation conflict**: when bearer auth is active, bypass origin validation for authenticated requests (remote clients need this)
4. **Add `plug doctor` check**: CRITICAL if non-loopback without auth
5. **Surface token via `plug status --show-token`** (masked by default for security)

### Key Design Decisions

**Token persistence (not ephemeral)**: Reuse existing token if file exists with correct permissions. Generate only on first run. Rationale: phone clients configure the token once; ephemeral tokens would break on every restart.

**Origin validation interaction**: When bearer auth is active, authenticated requests bypass origin validation entirely. Unauthenticated requests to non-loopback servers get 401. This resolves the critical conflict where the existing `validate_origin` middleware would reject all remote clients regardless of token validity.

**Discovery endpoint**: Return minimal server card (endpoint URL + protocol version only) when unauthenticated on non-loopback. Full card when authenticated. Prevents information leakage of server names, tool counts, transport types.

**Token file path includes port**: `http_auth_token_{port}` to support multiple `plug serve` instances on different ports without token file conflicts.

**No config override initially**: Auth is automatic based on bind address (loopback = no auth, non-loopback = auth required). Simpler, fewer footguns. Add `http.auth` config field later if needed.

## Technical Considerations

### Architecture

- Auth middleware sits as the **outermost layer** on `/mcp` routes in `build_router()`
- Middleware reads `Option<Arc<str>>` from `HttpState` — `None` means no auth, `Some(token)` means validate
- Token generation/verification reuses daemon patterns but extracted to shared location in `plug-core`
- **Critical gotcha (from learnings)**: `SecretString` Display returns `[REDACTED]` — must use `.as_str()` for actual comparison

### Middleware Logic

```
Request arrives at /mcp:
  if auth_token is None (loopback server):
    → proceed to origin validation → handler (backward compatible)
  if auth_token is Some:
    if Authorization header present and valid:
      → SKIP origin validation → handler (remote client OK)
    if Authorization header present but invalid:
      → 401 with WWW-Authenticate: Bearer
    if Authorization header missing:
      → 401 with WWW-Authenticate: Bearer
```

### 401 Response Format

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32001,
    "message": "authentication required"
  },
  "id": null
}
```

With `WWW-Authenticate: Bearer` header per RFC 6750.

## System-Wide Impact

- **Interaction graph**: `HTTP request → auth middleware (new) → origin validation (conditional) → session handler → tool router`. Auth middleware is the new outermost gate.
- **Error propagation**: Auth failures return 401 immediately, never reach session or tool routing. No retry concerns.
- **State lifecycle risks**: Token file persists across restarts. Risk: stale file with wrong permissions from manual editing. Mitigation: doctor check validates permissions.
- **API surface parity**: Both `plug serve` and `plug serve --daemon` HTTP servers should get auth. The `build_router()` function is shared, so this is automatic.
- **Integration test scenarios**:
  1. Non-loopback server + no auth header → 401
  2. Non-loopback server + valid bearer → 200 (full MCP flow works)
  3. Non-loopback server + invalid bearer → 401
  4. Loopback server + no auth header → 200 (backward compatible)
  5. Non-loopback server + valid bearer + SSE stream establishment → stream works

## Implementation Tasks

### Step 1: Extract auth utilities to plug-core

- [x] Create `plug-core/src/auth.rs` with `generate_auth_token()` and `verify_auth_token()` (extracted from `plug/src/daemon.rs:266-280`)
- [x] Make them `pub` functions in the new module
- [x] Update `plug/src/daemon.rs` to use `plug_core::auth::{generate_auth_token, verify_auth_token}`
- [x] Verify daemon IPC auth still works after extraction

### Step 2: Add auth token to HttpState and config

- [x] Add `pub auth_token: Option<Arc<str>>` to `HttpState` at `plug-core/src/http/server.rs:34`
- [x] Add token generation logic in `cmd_serve()` at `plug/src/runtime.rs:311`:
  - Parse bind_address, check if loopback via `http_bind_is_loopback()`
  - If non-loopback: load or generate token, pass to `HttpState`
  - If loopback: `auth_token = None`
- [x] Token file management:
  - Path: `config_dir().join(format!("http_auth_token_{}", port))`
  - If file exists with 0600 permissions: reuse
  - If file exists with wrong permissions: warn and fix to 0600
  - If file doesn't exist: generate and write with 0600

### Step 3: Implement auth middleware

- [x] Add `validate_bearer_auth` middleware function in `plug-core/src/http/server.rs`
  - Extract `Authorization` header, parse `Bearer <token>` format
  - Use `plug_core::auth::verify_auth_token()` for constant-time comparison
  - On success: set a request extension flag (`AuthStatus::Authenticated`) and proceed
  - On failure: return 401 with JSON-RPC error body + `WWW-Authenticate: Bearer` header
  - On no auth configured (loopback): pass through with `AuthStatus::NoAuthRequired`
- [x] Modify `validate_origin` to check `AuthStatus` extension:
  - If `Authenticated`: skip origin check (remote client with valid token)
  - If `NoAuthRequired`: run origin check as today (backward compatible)
  - If neither (should not happen): reject
- [x] Update `build_router()` to add auth middleware as outermost layer on `/mcp` routes

### Step 4: Auth-aware discovery endpoint

- [x] Modify `get_server_card` handler at `plug-core/src/http/server.rs`
  - If auth is configured and request is unauthenticated: return minimal card (endpoint URL + protocol version)
  - If authenticated or no auth required: return full card as today

### Step 5: `plug doctor` check

- [x] Add `check_http_auth` in `plug-core/src/doctor.rs`:
  - If `http.bind_address` is non-loopback and no auth token file exists: `CheckStatus::Fail` with message "HTTP server bound to non-loopback address without authentication"
  - If token file exists but permissions are not 0600: `CheckStatus::Warn`
  - If loopback or auth configured: `CheckStatus::Pass`
  - Fix suggestion: "Run `plug serve` to auto-generate an auth token, or set `bind_address = \"127.0.0.1\"` for local-only access"
- [x] Add to `tokio::join!()` in `run_doctor()` at line 52
- [x] Update test assertion from `checks.len() == 10` to `== 11` at line 1122

### Step 6: `plug status` token display

- [x] Extend `IpcResponse::Status` to include `http_auth: Option<HttpAuthStatus>` with fields: `enabled: bool`, `token: Option<String>` (only populated when requested)
- [x] Add `--show-token` flag to status command
- [x] In text mode: show "Auth: enabled (use `plug status --show-token` to reveal)" or "Auth: enabled | Token: <token>"
- [x] In JSON mode: include token field only when `--show-token` is passed
- [x] Never log the token value (use `[REDACTED]` in any debug output)

### Step 7: Tests

- [x] Unit test: `verify_auth_token` constant-time comparison (moved to plug-core)
- [x] Integration test: non-loopback server rejects unauthenticated requests with 401
- [x] Integration test: non-loopback server accepts valid bearer token
- [x] Integration test: non-loopback server rejects invalid bearer token with 401
- [x] Integration test: loopback server works without auth (backward compatible)
- [ ] Integration test: SSE stream establishment works with valid bearer token
- [x] Integration test: discovery endpoint returns minimal card when unauthenticated on non-loopback
- [x] Integration test: token file created with 0600 permissions
- [x] Integration test: existing token file reused across restarts
- [ ] Doctor test: non-loopback without auth token returns Fail

## Acceptance Criteria

- [x] Non-loopback HTTP server requires `Authorization: Bearer <token>` on `/mcp` requests
- [x] Localhost HTTP server works without auth (backward-compatible, no behavior change)
- [x] Authenticated remote clients bypass origin validation (remote access works)
- [x] 401 responses include `WWW-Authenticate: Bearer` header and JSON-RPC error body
- [x] Token persists across restarts (reused from file)
- [x] Token file has 0600 permissions
- [x] `plug doctor` reports CRITICAL if non-loopback without auth
- [x] `plug status --show-token` reveals the bearer token
- [x] Discovery endpoint returns minimal card for unauthenticated non-loopback requests
- [x] Existing daemon IPC auth unaffected by extraction refactor

## Success Metrics

- Can configure a phone MCP client with the bearer token and connect to plug over the network
- `plug doctor` catches misconfigured non-loopback servers before they're exposed
- Zero behavior change for existing localhost users

## Dependencies & Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Origin validation blocks remote clients even with token | **Addressed** | Critical | Auth middleware sets extension flag; origin validation checks it |
| SecretString Display sends `[REDACTED]` as auth header | Known gotcha | High | Use `.as_str()` not Display for comparison (from learnings) |
| Token file on synced filesystem (iCloud, Dropbox) | Low | Medium | Document: token file should be on local filesystem |
| Multiple serve instances share token file | Low | Medium | Port-specific filename: `http_auth_token_{port}` |

## Sources & References

### Parent Plan
- `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md` — Pre-Phase section

### Internal References
- Auth token generation: `plug/src/daemon.rs:266-280`
- HTTP router: `plug-core/src/http/server.rs:112-129`
- Origin validation: `plug-core/src/http/server.rs:135-162`
- HttpConfig: `plug-core/src/config/mod.rs:76-91`
- Loopback detection: `plug-core/src/config/mod.rs:184-186`
- Doctor checks: `plug-core/src/doctor.rs:52`
- Status command: `plug/src/views/overview.rs:123`
- Server startup: `plug/src/runtime.rs:311-333`

### Institutional Learnings Applied
- `docs/solutions/integration-issues/review-fixes-critical-http-auth-ipc-parity-20260307.md` — SecretString `.as_str()` vs Display gotcha
- `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md` — Origin validation rules, SSRF protection, error message discipline
- `docs/solutions/integration-issues/downstream-https-serving-20260307.md` — File permission validation pattern, TLS enforcement
