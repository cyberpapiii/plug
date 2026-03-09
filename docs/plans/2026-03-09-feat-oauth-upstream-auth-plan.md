---
title: "feat: OAuth 2.1 + PKCE upstream authentication with token refresh lifecycle"
type: feat
status: active
date: 2026-03-09
---

# feat: OAuth 2.1 + PKCE upstream authentication with token refresh lifecycle

## Overview

Add OAuth 2.1 + PKCE upstream authentication to plug so it can connect to remote MCP servers that
require OAuth (e.g., Notion, Figma, Slack, GitHub MCP servers). This is the final major roadmap item
— the only feature currently marked `missing` on `main`.

The implementation leverages rmcp 1.1.0's built-in `auth` feature which provides
`AuthorizationManager`, `CredentialStore` trait, PKCE flow, token exchange, and refresh. plug's role
is to wire this into its config model, transport creation, token persistence, CLI, daemon lifecycle,
and health state machine.

## Problem Statement

plug can currently connect to upstream MCP servers via stdio, HTTP, and legacy SSE. For HTTP/SSE
servers that require authentication, it supports a static bearer token via `auth_token` in config.
This is insufficient for OAuth-protected remote servers because:

1. **No authorization flow** — plug cannot perform browser-based OAuth consent
2. **No token refresh** — static tokens expire and cannot be renewed
3. **No discovery** — plug cannot discover authorization servers from MCP server metadata
4. **No secure storage** — no credential persistence beyond plain config values

Real-world pain points documented in bug reports (`docs/bug-reports/mcp-remote-oauth-reauth-blocks-
tool-calls.md` and `docs/bug-reports/mcp-remote-headless-oauth-impossible.md`):

- mcp-remote blocks all tool calls for 30-60s during synchronous re-auth
- mcp-remote cannot complete first-time auth in headless/daemon mode
- No proactive refresh — waits for 401 failure before attempting refresh

## Proposed Solution

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────┐
│                          CLI                            │
│  plug auth login ──► browser ──► localhost callback     │
│  plug auth status                                       │
│  plug auth logout                                       │
└───────────────────────┬─────────────────────────────────┘
                        │ stores credentials
                        ▼
┌─────────────────────────────────────────────────────────┐
│               Token Storage Layer                       │
│  ┌─────────────┐    ┌──────────────────────┐            │
│  │ OS Keyring  │◄──►│ File Fallback        │            │
│  │ (primary)   │    │ ~/.config/plug/      │            │
│  │             │    │   tokens/{server}.json│            │
│  └─────────────┘    └──────────────────────┘            │
└───────────────────────┬─────────────────────────────────┘
                        │ CredentialStore trait
                        ▼
┌─────────────────────────────────────────────────────────┐
│           rmcp AuthorizationManager                     │
│  discovery ─► PKCE ─► token exchange ─► refresh         │
│  (per OAuth server)                                     │
└───────────────────────┬─────────────────────────────────┘
                        │ get_access_token()
                        ▼
┌─────────────────────────────────────────────────────────┐
│              Engine / ServerManager                      │
│  ┌──────────────┐  ┌──────────────────────┐             │
│  │ Refresh Loop │  │ Transport Creation   │             │
│  │ per server   │  │ inject bearer token  │             │
│  │ (300s window)│  │ on each reconnect    │             │
│  └──────────────┘  └──────────────────────┘             │
└─────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Delegate protocol to rmcp** — rmcp's `AuthorizationManager` handles discovery, PKCE, token
   exchange, and refresh. plug implements `CredentialStore` for persistence and wires the lifecycle.

2. **Proactive refresh, not reactive 401** — background refresh at 80% of `expires_in` prevents
   the blocking re-auth problem documented in mcp-remote bugs.

3. **AuthRequired health state** — a new `ServerHealth` variant that is distinct from `Failed`.
   AuthRequired servers are excluded from routing but preserve their config and await re-auth.

4. **CLI/daemon boundary** — only `plug auth login` opens a browser. The daemon uses stored
   credentials and background refresh only. If refresh fails, the server enters AuthRequired
   state and the user must re-login via CLI.

5. **Config opt-in** — OAuth is explicit via `auth = "oauth"` on `ServerConfig`, mutually
   exclusive with `auth_token`. Validation enforces this at config load time.

6. **Public client only** — plug is a native CLI application and therefore a public OAuth client
   per OAuth 2.1 Section 2.1. It cannot securely store a client secret. The design uses PKCE
   (which replaces the client secret for authorization code grants) and does not support
   confidential client flows. There is no `oauth_client_secret` config field.

7. **Keyring-first storage** — OS keyring (macOS Keychain, Linux Secret Service) for production
   security, with JSON file fallback for headless/CI environments. Implements rmcp's
   `CredentialStore` trait.

## Technical Approach

### Dependencies

Add to workspace `Cargo.toml`:

```toml
# In rmcp features list, add:
"auth",

# New workspace dependencies:
keyring = { version = "3", features = ["apple-native", "linux-native"] }
```

The `auth` feature on rmcp 1.1.0 brings `oauth2` 5.x and `url` transitively. No need to depend
on `oauth2` directly. The `open` crate is already a CLI dependency (used for `plug config edit`).

### Phase 1: Token Storage and Auth Module Foundation

**Goal**: Implement `CredentialStore`, build the auth module, add config fields.

#### Config Model Changes

`plug-core/src/config/mod.rs`:

```rust
// New enum for auth type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamAuthMode {
    Bearer,  // existing static token behavior (default when auth_token present)
    OAuth,   // OAuth 2.1 + PKCE
}

// New fields on ServerConfig (alongside existing auth_token):
pub struct ServerConfig {
    // ... existing fields ...
    pub auth_token: Option<SecretString>,          // existing
    pub auth: Option<UpstreamAuthMode>,            // NEW
    pub oauth_client_id: Option<String>,           // NEW — optional, for pre-registered clients
    pub oauth_scopes: Option<Vec<String>>,         // NEW — requested scopes
    pub oauth_authorization_url: Option<String>,   // NEW — explicit override (skip discovery)
    pub oauth_token_url: Option<String>,           // NEW — explicit override (skip discovery)
}
```

Config TOML example:

```toml
[servers.notion]
transport = "http"
url = "https://mcp.notion.so/mcp"
auth = "oauth"
oauth_scopes = ["mcp:read", "mcp:write"]
# oauth_client_id is optional — rmcp supports dynamic client registration
```

Validation rules:
- `auth = "oauth"` is mutually exclusive with `auth_token`
- `oauth_authorization_url` and `oauth_token_url` must both be set or both absent
- `auth = "oauth"` requires `transport` of `http` or `sse` (not `stdio`)
- OAuth fields are ignored when `auth` is absent or `"bearer"`

#### Auth Module

Create `plug-core/src/oauth.rs`:

- [ ] `CompositeCredentialStore` implementing rmcp's `CredentialStore` trait
  - Primary: OS keyring via `keyring` crate (service `"plug"`, account `"oauth:{server_name}"`)
  - Fallback: JSON file at `~/.config/plug/tokens/{server_name}.json` with 0600 permissions
  - On successful keyring write, proactively clear file copy
  - On keyring read failure, silently fall back to file
- [ ] `KeyringBackend` trait abstraction for testability (real keyring + fake for tests)
- [ ] `build_authorization_manager(server_name, config) -> Result<AuthorizationManager>` factory
  - If `oauth_authorization_url` + `oauth_token_url` set: use explicit endpoints
  - Otherwise: let rmcp discover via Protected Resource Metadata (RFC 9728) from server URL
  - Set composite credential store
  - Configure client ID if provided, otherwise use dynamic registration
- [ ] `current_access_token(server_name) -> Result<Option<String>>` — resolve token from store
- [ ] `refresh_if_due(auth_manager, server_name) -> Result<RefreshOutcome>` — proactive refresh
  - Check `token_received_at` + `expires_in` against current time
  - If within 300s refresh window: attempt refresh
  - Return `NotDue`, `Refreshed`, or `AuthorizationRequired`
- [ ] `token_needs_refresh(stored: &StoredCredentials, window_secs: u64) -> bool` — pure function

#### Config Validation

`plug-core/src/config/mod.rs` — extend `validate_config()`:

- [ ] Reject `auth = "oauth"` + `auth_token` present simultaneously
- [ ] Reject `oauth_authorization_url` without `oauth_token_url` (and vice versa)
- [ ] Reject `auth = "oauth"` on `transport = "stdio"` servers
- [ ] Support `$ENV_VAR` expansion on OAuth config fields

#### Tasks

- [ ] Enable rmcp `"auth"` feature in workspace `Cargo.toml`
- [ ] Add `keyring` dependency to `plug-core/Cargo.toml`
- [ ] Add `UpstreamAuthMode` enum and new OAuth fields to `ServerConfig`
- [ ] Implement config validation rules
- [ ] Create `plug-core/src/oauth.rs` with `CompositeCredentialStore`
- [ ] Create `build_authorization_manager()` factory
- [ ] Create `current_access_token()` and `refresh_if_due()`
- [ ] Unit tests: file store round-trip, keyring fallback, composite store behavior
- [ ] Unit tests: config validation (mutual exclusion, transport restriction)

### Phase 2: Transport Integration and Health State Machine

**Goal**: Wire OAuth tokens into transport creation, add AuthRequired health state.

#### Dynamic Token Injection

`plug-core/src/server/mod.rs` — modify `start_and_register()`:

The current flow sets auth headers statically at transport creation time:

```rust
// Current (static):
if let Some(ref token) = config.auth_token {
    transport_config = transport_config.auth_header(format!("Bearer {}", token.as_str()));
}
```

For OAuth, resolve the current access token dynamically at connection time:

```rust
// New (dynamic):
let auth_header = match config.auth.as_ref() {
    Some(UpstreamAuthMode::OAuth) => {
        match oauth::current_access_token(&config.name).await? {
            Some(token) => Some(format!("Bearer {}", token)),
            None => return Err(ServerError::AuthorizationRequired),
        }
    }
    _ => config.auth_token.as_ref().map(|t| format!("Bearer {}", t.as_str())),
};

if let Some(header) = auth_header {
    transport_config = transport_config.auth_header(header);
}
```

This works because rmcp's `StreamableHttpClientTransportConfig::auth_header` is set per-connection.
When a token refreshes, the server reconnects with the new token — no need for dynamic header
injection on live connections.

**SSE transport** — same pattern applies to `LegacySseTransportConfig::auth_token`.

#### Health State Machine

`plug-core/src/types.rs` — add `AuthRequired` variant:

```rust
pub enum ServerHealth {
    Healthy,
    Degraded(String),
    Failed(String),
    AuthRequired,  // NEW — OAuth credentials missing or refresh failed
}
```

State transitions:
- **Startup without credentials** → `AuthRequired` (skip connection attempt)
- **401 during operation** → reconnect with refresh → if refresh fails → `AuthRequired`
- **Successful `plug auth login`** → trigger reconnect → if succeeds → `Healthy`
- **AuthRequired is sticky** — does not decay on health ticks, requires explicit credential refresh

Routing impact:
- `AuthRequired` servers are filtered from tool/resource/prompt/completion routing (same as Failed)
- `AuthRequired` servers appear in `plug servers` with distinct status indicator
- Capability synthesis masks `AuthRequired` servers (same as Failed)

#### Tasks

- [ ] Add `ServerHealth::AuthRequired` variant
- [ ] Update health state transitions (no auto-recovery from AuthRequired)
- [ ] Filter `AuthRequired` from all routing queries (tools, resources, prompts, completions)
- [ ] Modify `start_and_register()` to resolve OAuth tokens dynamically
- [ ] Handle `AuthorizationRequired` error → set AuthRequired state instead of Failed
- [ ] Update `ServerStatus` serialization to expose AuthRequired distinctly
- [ ] Update capability synthesis to mask AuthRequired servers
- [ ] Integration test: OAuth server without credentials → AuthRequired state
- [ ] Integration test: OAuth server with valid credentials → Healthy + tools work

### Phase 3: Background Refresh Loop

**Goal**: Proactive token refresh before expiry, concurrent refresh serialization.

#### Refresh Architecture

In the engine startup path, spawn a refresh task per OAuth-configured server:

```rust
// Per OAuth server:
tokio::spawn(async move {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        match oauth::refresh_if_due(&auth_manager, &server_name).await {
            Ok(RefreshOutcome::Refreshed) => {
                // Reconnect with new token
                server_manager.reconnect(&server_name).await;
            }
            Ok(RefreshOutcome::AuthorizationRequired) => {
                server_manager.mark_auth_required(&server_name).await;
            }
            Ok(RefreshOutcome::NotDue) => {}
            Err(e) => {
                tracing::warn!(server = %server_name, error = %e, "token refresh check failed");
            }
        }
    }
});
```

Key properties:
- **Poll interval**: 30 seconds (checks if refresh is due, not unconditionally refreshes)
- **Refresh window**: 300 seconds (5 minutes) before `expires_at`
- **Concurrent refresh serialization**: `tokio::sync::Mutex` per server in the auth manager
  prevents thundering herd from multiple poll cycles
- **Crash safety**: Write new refresh token to credential store BEFORE using new access token.
  If crash occurs after store write but before reconnect, next startup picks up the new token.
- **Reconnect on refresh**: After successful refresh, tear down the current transport and
  reconnect with the new token. This is a clean restart, not a hot-swap.

#### Tasks

- [ ] Implement per-server refresh polling loop in engine startup
- [ ] Serialize concurrent refresh attempts with `tokio::sync::Mutex`
- [ ] Reconnect server after successful token refresh
- [ ] Handle refresh failure → AuthRequired transition
- [ ] Log refresh outcomes at appropriate levels (info for refresh, warn for failure)
- [ ] Unit test: `token_needs_refresh` pure function with various expiry scenarios
- [ ] Integration test: token refresh triggers reconnect with new credentials

### Phase 4: CLI Auth Commands

**Goal**: Interactive browser-based OAuth login, status display, logout.

#### Commands

`plug/src/commands/auth.rs`:

**`plug auth login --server <name> [--no-browser]`**
1. Load server config, verify `auth = "oauth"`
2. Build `AuthorizationManager` via `oauth::build_authorization_manager()`
3. Discover authorization server metadata (or use explicit URLs from config)
4. Verify `code_challenge_methods_supported` includes `S256` (MCP spec requirement)
5. Start localhost TCP listener on random available port for redirect callback
6. Generate PKCE code verifier + challenge (S256)
7. Build authorization URL with:
   - `response_type=code`
   - `client_id` (from config or dynamic registration)
   - `redirect_uri=http://localhost:{port}/callback`
   - `scope` (from config `oauth_scopes`)
   - `resource` (server URL, per RFC 8707)
   - `code_challenge` + `code_challenge_method=S256`
   - `state` (CSRF token)
8. Open browser (unless `--no-browser`, in which case print URL for manual copy-paste)
9. Wait for callback (with 120s timeout)
10. Extract `code` and `state` from callback query parameters
11. Verify CSRF state matches
12. Exchange code for tokens via rmcp `exchange_code_for_token()`
13. Store credentials via `CompositeCredentialStore`
14. If server is AuthRequired in running engine → trigger reconnect
15. Print success with granted scopes

**`plug auth status [--output json]`**
- List all OAuth-configured servers with:
  - Server name and URL
  - Auth status (authenticated / not authenticated / token expired)
  - Granted scopes
  - Token expiry time (if available)
  - Storage location (keyring / file)

**`plug auth logout --server <name>`**
- Clear credentials from both keyring and file store
- If server is connected → mark AuthRequired

#### CLI Flag for Non-Interactive

Add `--auth-token` flag to `plug server add` for static bearer tokens (agent-native parity fix
from PR #35 review). This is separate from OAuth but bundled in this PR for completeness.

#### Tasks

- [ ] Create `plug/src/commands/auth.rs` with login/status/logout subcommands
- [ ] Register `Auth` command variant in `plug/src/main.rs`
- [ ] Implement browser-based PKCE flow with localhost callback
- [ ] Implement `--no-browser` manual URL flow
- [ ] Implement status display (text + JSON output)
- [ ] Implement logout with credential clearing
- [ ] Add `--auth-token` flag to `plug server add` (agent-native fix)
- [ ] Integration test: full login flow with mock OAuth provider (Axum-based)
- [ ] Test: `--no-browser` flow
- [ ] Test: status output for authenticated and unauthenticated servers

### Phase 5: Doctor, Import, and Polish

**Goal**: Doctor checks, import awareness, error messages, documentation.

#### Doctor Checks

`plug-core/src/doctor.rs` — add new checks:

- [ ] `check_oauth_config`: Validate OAuth config fields are coherent
  - Warn if `auth = "oauth"` but no scopes configured
  - Error if `auth = "oauth"` + `auth_token` both present
- [ ] `check_oauth_tokens`: Check token status per OAuth server
  - Pass if valid credentials exist in store
  - Warn if credentials exist but token is expired (refresh may fix)
  - Warn if no credentials found (needs `plug auth login`)
- [ ] `check_token_file_permissions`: Verify `~/.config/plug/tokens/` files have 0600

#### Import Awareness

`plug-core/src/import.rs`:

- [ ] When importing servers, preserve any `auth = "oauth"` configuration
- [ ] When importing from clients that have OAuth-configured servers, detect and map correctly
- [ ] Export: include OAuth config fields in TOML export (but NOT tokens — only config)

#### Error Messages

- [ ] AuthRequired servers show actionable message: `"Run \`plug auth login --server {name}\` to authenticate"`
- [ ] 401 errors during operation include context: `"OAuth token expired or revoked for {server}"`
- [ ] Discovery failures include the URLs attempted

#### Tasks

- [ ] Add doctor checks for OAuth config and token status
- [ ] Update import to handle OAuth config fields
- [ ] Update export to include OAuth config fields
- [ ] Polish error messages for actionable guidance
- [ ] Update `plug servers` view to show AuthRequired status clearly
- [ ] Run full test suite: `cargo test`, `cargo clippy`, `cargo fmt`

## Edge Cases and SpecFlow Findings

The following edge cases were identified during SpecFlow analysis and must be handled:

### Token Refresh Atomicity

OAuth 2.1 recommends refresh token rotation — each refresh returns a new refresh token. The old
refresh token is invalidated. If plug crashes between receiving new tokens and persisting them,
the tokens are lost. Mitigation:

1. Write new credentials to `CompositeCredentialStore` BEFORE triggering transport reconnect
2. Use atomic file writes (write to temp file, `rename()`) for file-backed storage
3. If keyring write fails, fall back to file — never lose the new refresh token

### Hot-Reload Interaction

The config watcher (`server_config_changed()`) compares transport, URL, command, args, env,
timeout, and enabled. It does NOT compare auth settings today. When a user changes from
`auth_token` to `auth = "oauth"` (or vice versa), the reload system must detect this change and
trigger a server restart with the new auth mode.

- [ ] Add `auth`, `oauth_client_id`, `oauth_scopes` to `server_config_changed()` comparison

### Health Check vs. AuthRequired

The health check probes with `list_tools()`. If an OAuth server returns 401 because the token
expired, the health check should NOT drive the server through Degraded → Failed. It must
recognize 401 as an auth issue and transition to AuthRequired instead.

- [ ] Classify 401 errors in health check path as AuthRequired, not Failed

### tools/list_changed on AuthRequired Transition

When a server enters AuthRequired, its tools are removed from the merged catalog. This SHOULD
emit `tools/list_changed` to downstream clients (consistent with Failed server behavior).

### Redirect URI Listener Security

The localhost callback listener must:
- Bind to `127.0.0.1` only (not `0.0.0.0`)
- Accept only GET requests to the expected callback path
- Shut down immediately after receiving the callback (or on timeout)
- Return a user-friendly HTML response ("Authentication complete, you may close this tab")

### Narrower Scope Grants

If the authorization server grants a narrower scope than requested, the granted scopes should be
stored and surfaced in `plug auth status`. Tool calls that fail with "insufficient scope" errors
should produce actionable error messages suggesting re-auth with broader scopes.

### Token File Path Sanitization

OAuth tokens are stored per-server at `~/.config/plug/tokens/{server_name}.json`. Since server
names come from user config, they must be sanitized to prevent path traversal. Use the same
name-sanitization rules as the existing config system.

### Missing `expires_in`

Some authorization servers omit `expires_in` from token responses. When absent, use a
conservative default (e.g., 3600 seconds / 1 hour). Log a warning so the user knows refresh
timing may not be optimal.

## Alternative Approaches Considered

### 1. Wrap rmcp with custom OAuth layer

**Rejected.** rmcp 1.1.0 already has `AuthorizationManager` with full spec compliance. Building a
parallel implementation would duplicate effort and risk diverging from the MCP spec.

### 2. Hot-swap tokens on live connections

**Rejected.** rmcp's `StreamableHttpClientTransportConfig::auth_header` is a static `Option<String>`
set at transport creation. Hot-swapping would require either an rmcp upstream change or a custom
`StreamableHttpClient` implementation that reads from `ArcSwap`. The reconnect-on-refresh approach
is simpler and proven: the extraction branch uses it, and the brief reconnection is invisible to
downstream clients because plug buffers requests during reconnect.

### 3. OAuth in plug-core only (no CLI commands)

**Rejected.** Initial OAuth authorization requires a browser interaction. The daemon cannot open a
browser. The CLI must provide `plug auth login` for the interactive flow. Status and logout are
table-stakes UX.

### 4. File-only token storage (no keyring)

**Rejected.** OS keyring integration is standard practice for CLI tools handling OAuth tokens. File
fallback is necessary for headless environments, but keyring should be the primary store for
security. The `keyring` crate with `apple-native` and `linux-native` features provides this with
minimal dependency weight.

## System-Wide Impact

### Interaction Graph

```
plug auth login
  → builds AuthorizationManager (rmcp)
  → starts localhost TCP listener (tokio)
  → opens browser (open crate)
  → receives callback → exchanges code (rmcp → upstream auth server)
  → stores credentials (CompositeCredentialStore → keyring/file)
  → triggers engine reconnect (ServerManager)
  → transport creation with new token (StreamableHttpClientTransport)
  → initialize + list_tools (rmcp → upstream MCP server)
  → tools available to downstream clients

refresh loop (per server)
  → checks token_needs_refresh (pure function)
  → calls refresh_token (rmcp AuthorizationManager → upstream auth server)
  → stores new credentials (CompositeCredentialStore)
  → reconnects server (ServerManager → transport teardown + new transport)
  → downstream clients see brief tool unavailability then recovery
```

### Error & Failure Propagation

| Error | Source | Handling |
|-------|--------|----------|
| Discovery failure | rmcp → auth server | Mark AuthRequired, log URLs attempted |
| PKCE verification missing | Metadata check | Refuse to proceed, tell user |
| Browser not opened | `open` crate | Fall back to print URL (like `--no-browser`) |
| Callback timeout | localhost listener | Error message, suggest `--no-browser` |
| Token exchange failure | rmcp → auth server | Error message with server response |
| Keyring unavailable | `keyring` crate | Silent fallback to file store |
| File write failure | Token storage | Error, suggest checking permissions |
| Refresh failure (network) | rmcp → auth server | Retry on next poll cycle (30s) |
| Refresh failure (revoked) | rmcp → auth server | Mark AuthRequired |
| 401 mid-session | Upstream MCP server | Attempt refresh → reconnect or AuthRequired |

### State Lifecycle Risks

- **Crash between token store write and reconnect**: Safe — next startup reads new token from store
- **Crash during refresh token write**: Partial write risk mitigated by atomic file operations
  (write to temp file, rename)
- **Stale file after keyring update**: CompositeCredentialStore clears file copy after keyring write
- **Multiple plug instances sharing credentials**: File locking prevents concurrent corruption;
  keyring is process-safe via OS APIs
- **Config changes from bearer to OAuth**: Requires restart; live reconfiguration not in scope

### API Surface Parity

| Interface | OAuth Support Needed |
|-----------|---------------------|
| `plug connect` (stdio downstream) | Upstream OAuth transparent — tools just work |
| `plug serve` (HTTP downstream) | Upstream OAuth transparent — tools just work |
| Daemon IPC | AuthRequired state surfaced in status; cannot trigger login |
| `plug servers --output json` | Must include auth status (bearer / oauth / auth-required) |
| `plug server add` | `--auth-token` flag for static tokens (agent-native fix) |
| `plug doctor --output json` | OAuth config + token status checks |
| `plug import` | Preserve OAuth config fields from source |

### Integration Test Scenarios

1. **Full OAuth flow with mock provider**: Start Axum-based fake OAuth server, configure plug
   server with `auth = "oauth"`, run `plug auth login` flow, verify tools are accessible
2. **Proactive refresh**: Set up token with 10s expiry, verify refresh fires before expiry,
   verify reconnect happens transparently
3. **Refresh failure → AuthRequired**: Mock refresh endpoint returning 400, verify server
   transitions to AuthRequired, verify tools are filtered from routing
4. **Daemon cold start without credentials**: Start engine with OAuth server but no stored
   credentials, verify server is AuthRequired, verify other servers still work
5. **Re-login after AuthRequired**: After server enters AuthRequired, simulate successful
   login, verify server recovers to Healthy

## Acceptance Criteria

### Functional Requirements

- [ ] `plug auth login --server <name>` completes browser-based OAuth flow and stores credentials
- [ ] `plug auth login --server <name> --no-browser` works with manual URL copy-paste
- [ ] `plug auth status` shows per-server auth status (text + JSON output)
- [ ] `plug auth logout --server <name>` clears credentials from all stores
- [ ] OAuth servers with valid credentials connect and route tools transparently
- [ ] OAuth servers without credentials enter AuthRequired state (not Failed)
- [ ] Background refresh proactively renews tokens before expiry
- [ ] Refresh failure transitions server to AuthRequired
- [ ] Re-login after AuthRequired recovers server to Healthy
- [ ] `plug doctor` validates OAuth config and token status
- [ ] `plug server add --auth-token <TOKEN>` works for static bearer tokens (agent-native fix)

### Non-Functional Requirements

- [ ] Tokens stored in OS keyring when available, file fallback with 0600 permissions
- [ ] No token values in logs (SecretString wrapping from moment of receipt)
- [ ] PKCE S256 mandatory (refuse to proceed without `code_challenge_methods_supported`)
- [ ] RFC 8707 resource parameter included in all authorization and token requests
- [ ] No blocking on tool-call path (refresh is background, not synchronous)

### Quality Gates

- [ ] All existing tests pass (`cargo test`)
- [ ] Clippy clean (`cargo clippy --all-targets --all-features -- -D warnings`)
- [ ] Format clean (`cargo fmt --check`)
- [ ] Integration test with mock OAuth provider passes
- [ ] Token refresh integration test passes
- [ ] AuthRequired state machine test passes

## Dependencies & Prerequisites

| Dependency | Version | Purpose | Risk |
|-----------|---------|---------|------|
| rmcp `"auth"` feature | 1.1.0 | AuthorizationManager, CredentialStore, PKCE | Verified available |
| `keyring` | 3.x | OS keychain credential storage | apple-native + linux-native features |
| `open` | 5.x | Browser launch (already in CLI deps) | Already present |
| `oauth2` | 5.x | Transitive via rmcp `auth` feature | Not directly depended on |

No upstream rmcp changes needed. The `auth_header` field on `StreamableHttpClientTransportConfig`
and the reconnect-on-refresh approach avoid any SDK modifications.

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| rmcp `AuthorizationManager` API doesn't match MCP spec exactly | Low | High | Verified via Context7 docs — full spec coverage including RFC 8707, PKCE S256, discovery |
| Keyring unavailable on headless Linux | Medium | Medium | CompositeCredentialStore falls back to file automatically |
| Browser launch fails in WSL/SSH | Medium | Low | `--no-browser` flag with manual URL flow |
| Token refresh race between CLI and daemon | Low | Medium | Shared credential store with file locking; keyring is process-safe |
| Redirect URI port conflict | Low | Low | Use port 0 (OS assigns available port) |
| Clock skew causes premature/late refresh | Low | Medium | Use server-provided `expires_in` relative to local receipt time, not absolute |
| Config migration from `auth_token` to `auth = "oauth"` | Medium | Low | Validation rejects both simultaneously; clear error message guides user |

## Extraction Source Reference

The `fix/subscription-rebind-confidence` branch contains a complete, tested implementation
(~1,050 lines of new Rust code) that covers this plan. Assessment: **high quality, extractable**.

Key files on that branch (exists off-main, reference only):
- `plug-core/src/oauth.rs` — CompositeCredentialStore, refresh logic, auth manager factory
- `plug/src/commands/auth.rs` — login/status/logout CLI commands
- Config changes across `plug-core/src/config/mod.rs`
- Engine integration in `plug-core/src/engine.rs`
- Health state changes in `plug-core/src/types.rs`

The extraction branch code should be used as a reference implementation, not merged whole-cloth.
It was developed alongside other features on a long-lived branch and requires isolation.

## Institutional Learnings Applied

- **SecretString Display leaks** (todos/025): Wrap OAuth tokens in `SecretString` at moment of
  receipt. Use `.as_str()` for actual header values, never `Display` or `format!`.
- **TOCTOU in file creation** (downstream auth plan): Use `create_new(true)` for initial token
  file creation to prevent symlink attacks. Write to temp file + rename for atomic updates.
- **mcp-remote blocking re-auth** (bug report): Proactive background refresh prevents tool-call
  blocking. Never trigger re-auth synchronously on the request path.
- **mcp-remote headless failure** (bug report): Daemon cannot open browser. `plug auth login`
  is the only interactive path. `--no-browser` enables manual flow for restricted environments.

## Sources & References

### Internal References

- Current auth module: `plug-core/src/auth.rs` (downstream bearer tokens only)
- Config model: `plug-core/src/config/mod.rs:108` (`ServerConfig` struct)
- Transport auth injection: `plug-core/src/server/mod.rs:503-508` (HTTP), `:595-598` (SSE)
- Health state machine: `plug-core/src/types.rs` (`ServerHealth` enum)
- Downstream auth plan: `docs/plans/2026-03-07-feat-downstream-http-bearer-auth-plan.md`
- Roadmap plan (Phase B2): `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md:287-373`
- Bug reports: `docs/bug-reports/mcp-remote-oauth-reauth-blocks-tool-calls.md`,
  `docs/bug-reports/mcp-remote-headless-oauth-impossible.md`

### External References

- MCP spec authorization: `https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization`
- rmcp `AuthorizationManager` API: `rmcp::transport::auth::AuthorizationManager`
- rmcp `CredentialStore` trait: `rmcp::transport::auth::CredentialStore`
- RFC 8707 (Resource Indicators): `https://www.rfc-editor.org/rfc/rfc8707.html`
- RFC 9728 (Protected Resource Metadata): `https://www.rfc-editor.org/rfc/rfc9728.html`
- RFC 8414 (OAuth 2.0 Authorization Server Metadata): `https://www.rfc-editor.org/rfc/rfc8414.html`

### Related Work

- PR #35 — Legacy SSE upstream transport (includes auth token injection pattern)
- PR #34 — Elicitation + sampling forwarding
- Extraction branch: `fix/subscription-rebind-confidence` (OAuth implementation exists off-main)
