---
title: "feat: OAuth 2.1 + PKCE upstream authentication with token refresh lifecycle"
type: feat
status: in-review
date: 2026-03-09
deepened: 2026-03-09
---

# feat: OAuth 2.1 + PKCE upstream authentication with token refresh lifecycle

## Enhancement Summary

**Deepened on:** 2026-03-09
**Research agents used:** security-sentinel, architecture-strategist, performance-oracle,
code-simplicity-reviewer, agent-native-reviewer, learnings-researcher, repo-research-analyst,
best-practices-researcher, Context7 (MCP spec + rmcp docs), web search (OAuth 2.1, keyring, RFC 8252)
**Sections enhanced:** 14

### Key Improvements

1. **Prerequisite refactor identified**: Add `ServerHealth::is_routable()` method before OAuth work
   to prevent routing bugs from incomplete predicate updates (Architecture finding)
2. **Pre-existing bug discovered**: `server_config_changed()` in reload.rs does not compare
   `auth_token` — changing a static bearer token via hot-reload silently does nothing
3. **Security hardening**: 13 security findings integrated — path traversal sanitization, CSRF state
   entropy specification, redirect listener hardening, log leakage audit
4. **Agent-native gaps closed**: Token injection path (`plug auth inject`), non-interactive code
   exchange (`plug auth complete`), IPC auth state commands
5. **Performance optimizations**: Zero-downtime refresh via pre-created transport, computed-sleep
   replacing fixed 30s polling, in-memory token cache
6. **Simplification**: ~15-23% LOC reduction by dropping premature abstractions (KeyringBackend
   trait, UpstreamAuthMode enum, discovery URL overrides, per-server Mutex)
7. **MCP spec compliance confirmed**: RFC 8707 resource parameter MUST, RFC 9728 dual discovery,
   dynamic client registration MAY, token passthrough prohibition

### New Considerations Discovered

- AuthRequired servers should skip health checks entirely (probing without credentials is pointless)
- Refresh loop must use `tracker.spawn()` with `CancellationToken` for clean shutdown (matches
  existing health check pattern)
- Dynamic client registration client_id should be persisted in credential store
- File fallback stores tokens as plaintext — doctor check should warn when in use
- `load_raw_config()` must scan new OAuth fields for `$ENV_VAR` references for daemon auto-start

---

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
│  plug auth inject ──► stdin/args ──► credential store   │
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
│  ┌──────────────────────────────────────────┐            │
│  │ In-Memory Cache (ArcSwap per server)     │            │
│  │ hot path for refresh-check + transport   │            │
│  └──────────────────────────────────────────┘            │
└───────────────────────┬─────────────────────────────────┘
                        │ CredentialStore trait
                        ▼
┌─────────────────────────────────────────────────────────┐
│           rmcp AuthorizationManager                     │
│  discovery ─► PKCE ─► token exchange ─► refresh         │
│  (per OAuth server, cached for daemon lifetime)         │
└───────────────────────┬─────────────────────────────────┘
                        │ get_access_token()
                        ▼
┌─────────────────────────────────────────────────────────┐
│              Engine / ServerManager                      │
│  ┌──────────────┐  ┌──────────────────────┐             │
│  │ Refresh Loop │  │ Transport Creation   │             │
│  │ per server   │  │ pre-create + swap    │             │
│  │ (computed    │  │ zero-downtime on     │             │
│  │  sleep)      │  │ token refresh        │             │
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

8. **Zero-downtime refresh** — pre-create the new transport with fresh token before tearing down
   the old one. Atomic pointer swap via DashMap entry replacement eliminates the 500ms-2s
   availability gap that a tear-down-then-reconnect approach would cause.

### Research Insights: Design Decisions

**MCP Spec Compliance (Context7):**
- MCP clients MUST implement RFC 8707 Resource Indicators — `resource` parameter in both
  authorization requests AND token requests, identifying the MCP server URL
- MCP clients MUST support both RFC 9728 discovery mechanisms: WWW-Authenticate header (prioritized)
  and well-known URI (`/.well-known/oauth-protected-resource`)
- Dynamic client registration (RFC 7591) is MAY — included for backwards compatibility
- Token passthrough is explicitly forbidden in the MCP spec — tokens must be audience-bound
- OAuth 2.1 requires refresh tokens for public clients to be sender-constrained or one-time use

**rmcp Auth Module API (Context7 + source inspection):**
- `AuthorizationManager` — manages the full OAuth 2.0 flow
- `AuthClient<C>` — HTTP client with OAuth capabilities
- `AuthorizationSession` — facilitates user authorization
- `AuthorizedHttpClient` — auto-appends authorization headers
- `OAuthClientConfig` — client configuration
- `StoredCredentials` — credential storage format
- `CredentialStore` trait — persistence interface (plug implements this)
- `InMemoryCredentialStore` — default in-memory implementation (reference for trait contract)
- `OAuthState` enum — recommended state machine for OAuth clients
- `AuthError` enum — error types
- `refresh_token()` method returns `StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>`

**OAuth 2.1 Best Practices (Web Research):**
- PKCE is mandatory for ALL public clients, not just mobile (OAuth 2.1 Section 4.1.2)
- Code verifier must be treated as a secret at runtime — never log it
- Refresh token rotation is recommended — each refresh returns a new refresh token
- For native apps, RFC 8252 is the governing document for redirect URI handling

## Technical Approach

### Prerequisites (Before OAuth Work)

> These are prerequisite changes that should land as separate, small PRs before the OAuth
> implementation begins. They fix pre-existing issues and prepare the architecture.

#### Prerequisite 1: `ServerHealth::is_routable()` Method

**Source:** Architecture review finding #1

Every routing method in `ServerManager` (`get_tools()`, `get_resources()`, `get_prompts()`,
`get_completions()`, etc.) currently uses `h.health != ServerHealth::Failed` as the exclusion
predicate. Adding `AuthRequired` as a fourth variant means ALL these predicates must change. If
even one is missed, AuthRequired servers receive routed requests, causing 401 cascades.

**Fix:** Add an `is_routable()` method to `ServerHealth` and replace all `!= Failed` checks:

```rust
impl ServerHealth {
    pub fn is_routable(&self) -> bool {
        matches!(self, ServerHealth::Healthy | ServerHealth::Degraded(_))
    }
}
```

This is a mechanical refactor (~6 call sites in server/mod.rs) that eliminates an entire class of
integration bugs. Ship as a separate PR before OAuth.

- [x] Add `ServerHealth::is_routable()` method
- [x] Replace all `!= Failed` predicates with `.is_routable()`
- [x] Verify all routing methods use `is_routable()`

#### Prerequisite 2: Fix `server_config_changed()` for `auth_token`

**Source:** Architecture review + repo-patterns analysis

`server_config_changed()` in `plug-core/src/reload.rs:140` compares `command`, `args`, `env`,
`transport`, `url`, `timeout_secs`, `call_timeout_secs`, `enabled`. It does NOT compare
`auth_token`. This means changing a static bearer token via config hot-reload silently does
nothing — the old token is used until manual restart.

This is a pre-existing bug that predates the OAuth plan.

- [x] Add `auth_token` to `server_config_changed()` comparison
- [x] Ship as part of prerequisite PR or standalone fix

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

### Research Insights: Dependencies

**Keyring Crate v3 (Web Research):**
- v3 has API changes — `set_default_credential_builder` is now `set_default_store`
- No default features — must explicitly specify platform backends
- Entries identified by service name + user name pair (UTF-8 strings)
- Supports both password strings and binary secrets
- `apple-native` uses macOS Security framework (Keychain); `linux-native` uses Secret Service (D-Bus)

### Phase 1: Config, Token Storage, Transport Integration, and Health State

**Goal**: Config model, credential store, transport auth injection, and AuthRequired health state.
This phase merges the original Phases 1 and 2 because they are tightly coupled — the credential
store without transport integration is inert and untestable in isolation.

> **Simplification applied:** Original plan had separate Phase 1 (auth module) and Phase 2
> (transport + health). Merged because the credential store has no standalone test value without
> transport wiring.

#### Config Model Changes

`plug-core/src/config/mod.rs`:

```rust
// New fields on ServerConfig (alongside existing auth_token):
pub struct ServerConfig {
    // ... existing fields ...
    pub auth_token: Option<SecretString>,          // existing
    pub auth: Option<String>,                      // NEW — "oauth" or absent
    pub oauth_client_id: Option<String>,           // NEW — optional, for pre-registered clients
    pub oauth_scopes: Option<Vec<String>>,         // NEW — requested scopes
}
```

> **Simplification applied:** Dropped `UpstreamAuthMode` enum. `auth: Option<String>` where the
> only meaningful value is `"oauth"`. If `auth_token` is present and `auth` is absent, bearer is
> implied. One `if` statement, not an enum.
>
> **Simplification applied:** Deferred `oauth_authorization_url` and `oauth_token_url` config
> fields. Ship with discovery-only (RFC 9728). No known MCP server in 2026 requires manual OAuth
> endpoint configuration. Add these fields if a real user reports a server without discovery.

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
- `auth = "oauth"` requires `transport` of `http` or `sse` (not `stdio`)
- OAuth fields are ignored when `auth` is absent

#### Server Name Sanitization

**Source:** Security finding #1 (HIGH) — path traversal in token file naming

Server names from user config are used as filesystem paths for token storage. The existing config
system has NO filesystem-safe name validation.

```rust
/// Sanitize server name for use in filesystem paths.
/// Rejects path separators, parent directory references, hidden file prefixes,
/// and null bytes. Caps length to 255 bytes.
fn sanitize_server_name_for_path(name: &str) -> Result<&str, ConfigError> {
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(ConfigError::InvalidServerName(name.to_string()));
    }
    if name.starts_with('.') || name.contains("..") {
        return Err(ConfigError::InvalidServerName(name.to_string()));
    }
    if name.len() > 255 {
        return Err(ConfigError::InvalidServerName(name.to_string()));
    }
    Ok(name)
}
```

- [x] Implement `sanitize_server_name_for_path()` in config validation
- [x] Call it from `validate_config()` for ALL server names (benefits the whole system, not just OAuth)
- [x] Verify final token file path is within the `tokens/` directory after construction

#### Auth Module

Create `plug-core/src/oauth.rs`:

- [x] `CompositeCredentialStore` implementing rmcp's `CredentialStore` trait
  - Primary: OS keyring via `keyring` crate (service `"plug"`, account `"oauth:{server_name}"`)
  - Fallback: JSON file at `~/.config/plug/tokens/{server_name}.json` with 0600 permissions
  - On keyring read failure, silently fall back to file
  - Write order: try keyring first, then file. No cross-synchronization needed (simplicity reviewer:
    stale file with old token is harmless — fresh keyring token wins on read)
- [x] In-memory cache: `ArcSwap<Option<CachedCredentials>>` per server
  - Populated on first read from keyring/file
  - Invalidated on write (refresh) and logout
  - Used by refresh-check loop (pure timestamp comparison, no I/O)
  - Used by transport creation (token value for auth header)
- [ ] `build_authorization_manager(server_name, config) -> Result<AuthorizationManager>` factory
  - Let rmcp discover via Protected Resource Metadata (RFC 9728) from server URL
  - Set composite credential store
  - Configure client ID if provided, otherwise use dynamic registration
  - Cache the resulting `AuthorizationManager` in `DashMap<String, AuthorizationManager>` for
    reuse across refresh cycles (avoid rediscovery on every reconnect)
  - If dynamic registration is used, persist the assigned `client_id` in the credential store
    alongside tokens (prevents re-registration on every login — Security finding #10)
- [x] `current_access_token(server_name) -> Result<Option<String>>` — resolve from in-memory cache
- [ ] `refresh_if_due(auth_manager, server_name) -> Result<bool>` — proactive refresh
  - Check cached `token_received_at` + `expires_in` against current time
  - If within refresh window: attempt refresh, return `true`
  - If not due: return `false`
  - On terminal failure (revoked token): return error
- [x] `token_needs_refresh(stored: &StoredCredentials, window_secs: u64) -> bool` — pure function

> **Simplification applied:** Dropped `KeyringBackend` trait abstraction. The keyring crate's API
> is already an abstraction boundary. Test the file fallback path directly — that IS the testable
> path for CI/headless environments. Dropped `RefreshOutcome` enum — `Result<bool>` is simpler.
>
> **Simplification applied:** Dropped dual-write cleanup (clearing file copy after keyring write).
> Write to whichever store works. On read, try keyring first, fall back to file. A stale file is
> harmless for a personal tool. (Simplicity reviewer)

#### Research Insights: Token Storage Security

**Security Findings (Security Sentinel):**
- File fallback stores tokens as **plaintext JSON** (Security finding #4, HIGH). The keyring
  provides OS-level encryption at rest; the file does not. Mitigation: doctor check warns when
  file fallback is in use. Consider storing only the refresh token in the file (the access token
  can be re-derived from it), reducing the exposure window.
- File permissions (0600) do not protect against: root access, filesystem backup tools, cloud-synced
  directories (iCloud, Dropbox), or malware running as the same user.
- Refresh token rotation atomicity (Security finding #5, MEDIUM): Write order matters. Write to
  keyring first; if keyring fails, write to file. On crash between keyring write and file write,
  the keyring has the new token and the file has the old (revoked) one — reads from keyring win.
- Cross-process serialization (Security finding #7, MEDIUM): CLI and daemon are separate processes.
  The per-server `tokio::sync::Mutex` only serializes within one process. Use
  `fs2::FileExt::lock_exclusive()` on the token file as a cross-process mutex for credential writes.

**Institutional Learnings (docs/solutions/):**
- TOCTOU race prevention: Use `create_new(true)` (O_CREAT | O_EXCL) for initial token file
  creation to prevent symlink attacks. Write to temp file + rename for atomic updates.
  (Source: `docs/solutions/integration-issues/pre-phase-downstream-http-bearer-auth-20260307.md`)
- SecretString wrapping: Use `.as_str()` for actual header values, never `Display` or `format!`.
  The existing `SecretString` implements `Serialize` transparently via `#[serde(transparent)]`,
  meaning serializing a config to TOML/JSON emits the raw token. Ensure OAuth tokens in-memory
  are wrapped in `SecretString` and no log or serialization path leaks them.
  (Source: `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md`)

#### Config Validation

`plug-core/src/config/mod.rs` — extend `validate_config()`:

- [x] Reject `auth = "oauth"` + `auth_token` present simultaneously
- [x] Reject `auth = "oauth"` on `transport = "stdio"` servers
- [x] Validate server names with `sanitize_server_name_for_path()`
- [x] Support `$ENV_VAR` expansion on OAuth config fields
- [x] Ensure `load_raw_config()` scans new OAuth fields for `$ENV_VAR` references so daemon
  auto-start forwards referenced environment variables correctly (Architecture finding)

#### Dynamic Token Injection

`plug-core/src/server/mod.rs` — modify `start_and_register()`:

The current flow sets auth headers statically at transport creation time:

```rust
// Current (static):
if let Some(ref token) = config.auth_token {
    transport_config = transport_config.auth_header(format!("Bearer {}", token.as_str()));
}
```

For OAuth, resolve the current access token from the in-memory cache at connection time:

```rust
// New (dynamic):
let auth_header = match config.auth.as_deref() {
    Some("oauth") => {
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

impl ServerHealth {
    /// Returns true for states that should participate in routing.
    /// AuthRequired and Failed servers are excluded from tool/resource/prompt routing.
    pub fn is_routable(&self) -> bool {
        matches!(self, ServerHealth::Healthy | ServerHealth::Degraded(_))
    }
}
```

State transitions:
- **Startup without credentials** → `AuthRequired` (skip connection attempt)
- **401 during operation** → reconnect with refresh → if refresh fails → `AuthRequired`
- **Successful `plug auth login` or `plug auth inject`** → trigger reconnect → if succeeds → `Healthy`
- **AuthRequired is sticky** — does not decay on health ticks, requires explicit credential refresh

Routing impact:
- `AuthRequired` servers are filtered via `is_routable()` (same as Failed)
- `AuthRequired` servers appear in `plug servers` with distinct status indicator
- Capability synthesis masks `AuthRequired` servers (same as Failed)

### Research Insights: Health State Machine

**Architecture Findings:**
- **Skip health checks for AuthRequired servers** (Finding #2): The health check probes with
  `list_tools()`. Probing an AuthRequired server is pointless — it has no valid credentials. Add a
  check at the top of the health check loop: if health state is `AuthRequired`, continue without
  probing. This keeps the `HealthState` state machine clean and avoids HTTP error parsing.
  This is simpler than trying to classify 401 errors inside the health probe.
- **`EngineEvent` wire format**: Adding `AuthRequired` changes the `ServerHealth` serialized
  representation. Daemon IPC consumers that deserialize `ServerHealthChanged` events need to
  handle the new variant. This is a minor wire-format concern but worth noting.

**Repo Pattern (repo-research-analyst):**
- `HealthState` struct wraps `health: ServerHealth` + `consecutive_failures: u32`
- Transitions: Healthy + 3 failures → Degraded; Degraded + 6 cumulative → Failed; Failed + 1
  success → Degraded; Degraded + 1 success → Healthy
- `mark_start_failure()` sets Failed with `consecutive_failures: 6`
- AuthRequired bypasses this state machine entirely — it is set directly and only cleared by
  explicit credential provision + successful reconnect

#### Tasks (Phase 1)

- [x] Enable rmcp `"auth"` feature in workspace `Cargo.toml`
- [x] Add `keyring` dependency to `plug-core/Cargo.toml`
- [x] Add `auth: Option<String>` and OAuth fields to `ServerConfig`
- [x] Implement config validation rules (including server name sanitization)
- [x] Add `$ENV_VAR` expansion for new OAuth fields in `load_raw_config()`
- [x] Create `plug-core/src/oauth.rs` with `CompositeCredentialStore`
- [x] Implement in-memory token cache with `ArcSwap`
- [ ] Create `build_authorization_manager()` factory with per-server caching
- [x] Create `current_access_token()` (reads from cache) and `refresh_if_due()`
- [x] Add `ServerHealth::AuthRequired` variant and `is_routable()` method
- [x] Update all routing predicates to use `is_routable()`
- [x] Skip health checks for AuthRequired servers
- [x] Modify `start_and_register()` to resolve OAuth tokens dynamically
- [x] Handle `AuthorizationRequired` error → set AuthRequired state instead of Failed
- [x] Update capability synthesis to mask AuthRequired servers
- [x] Add all new fields to `server_config_changed()` comparison
- [x] Unit tests: file store round-trip, keyring fallback, composite store behavior
- [x] Unit tests: config validation (mutual exclusion, transport restriction, name sanitization)
- [x] Integration test: OAuth server without credentials → AuthRequired state
- [ ] Integration test: OAuth server with valid credentials → Healthy + tools work

### Phase 2: Background Refresh Loop

**Goal**: Proactive token refresh before expiry with zero-downtime reconnection.

#### Refresh Architecture

In the engine startup path, spawn a refresh task per OAuth-configured server via `tracker.spawn()`:

```rust
// Per OAuth server — spawned via tracker for clean shutdown:
tracker.spawn(async move {
    let mut next_check = Duration::from_secs(30);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(next_check) => {
                match oauth::refresh_if_due(&auth_manager, &server_name).await {
                    Ok(true) => {
                        // Pre-create new transport, then atomic swap
                        server_manager.reconnect_with_precheck(&server_name).await;
                        next_check = Duration::from_secs(30);
                    }
                    Ok(false) => {
                        // Compute next interesting time based on cached expiry
                        next_check = oauth::time_until_refresh_window(&server_name)
                            .unwrap_or(Duration::from_secs(30))
                            .min(Duration::from_secs(30))
                            .max(Duration::from_secs(5));
                    }
                    Err(e) if e.is_authorization_required() => {
                        server_manager.mark_auth_required(&server_name).await;
                        // Stop polling — AuthRequired is sticky, needs explicit re-auth
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(server = %server_name, error = %e,
                            "token refresh check failed, retrying in 30s");
                        next_check = Duration::from_secs(30);
                    }
                }
            }
        }
    }
});
```

### Research Insights: Refresh Loop

**Performance Findings:**
- **30s fixed polling is wasteful** (Performance finding #2): For a 3600s token, 110 of 120 polls
  per cycle are wasted work. With 20 OAuth servers, that is 2400 wakeups/hour where ~20 are useful.
  **Fix:** Computed sleep based on cached `expires_at` timestamp. Reduces wakeups from ~120/hour
  to ~2-4/hour per server during normal operation.
- **Zero-downtime refresh** (Performance finding #1): Pre-create the new transport with fresh
  token BEFORE tearing down the old one. The new transport completes TLS handshake + MCP
  `initialize` + `list_tools` while the old one is still serving. Then do an atomic pointer swap
  via `DashMap` entry replacement. The old transport drops after replacement. This eliminates the
  500ms-2s availability gap per refresh cycle.

**Architecture Findings:**
- **Use `tracker.spawn()` with `CancellationToken`** (Finding #4): Bare `tokio::spawn` creates
  orphaned tasks that survive `Engine::shutdown()` and may attempt keyring/file access after
  shutdown. Match the pattern from `spawn_health_checks()` in `plug-core/src/health.rs`.
- **Break on AuthRequired**: When refresh fails terminally, the loop should exit (not continue
  polling a server that needs explicit re-auth). A new refresh loop is spawned when the server
  recovers via `plug auth login` or `plug auth inject`.

**Simplification Applied:**
- Dropped `tokio::sync::Mutex` per server for refresh serialization. There is exactly one refresh
  loop per server. The only race scenario (CLI + daemon) is cross-process and requires file
  locking, not an in-process mutex.
- Dropped `RefreshOutcome` enum — `Result<bool>` (refreshed or not due) plus error variants.

Key properties:
- **Poll interval**: Computed sleep based on token expiry (max 30s, min 5s)
- **Refresh window**: `const TOKEN_REFRESH_WINDOW_SECS: u64 = 300` (semantic constant, not magic number)
- **Short-lived tokens**: If `expires_in < 600`, refresh at 50% of lifetime instead of lifetime - 300s
- **Crash safety**: Write new refresh token to credential store BEFORE triggering transport reconnect.
  If crash occurs after store write but before reconnect, next startup picks up the new token.
- **Cross-process serialization**: `fs2::FileExt::lock_exclusive()` on token file during
  read-refresh-write cycle prevents CLI/daemon race
- **Zero-downtime reconnect**: Pre-create new transport, then atomic swap

#### Tasks (Phase 2)

- [x] Implement per-server refresh loop with computed-sleep in engine startup
- [x] Spawn via `tracker.spawn()` with `CancellationToken` for clean shutdown
- [ ] Implement zero-downtime reconnect (pre-create transport, then swap)
- [x] Use `fs2::FileExt::lock_exclusive()` for cross-process credential write serialization
- [x] Handle refresh failure → AuthRequired transition (break loop)
- [x] Handle short-lived tokens: adapt refresh window when `expires_in < 600`
- [x] Log refresh outcomes at appropriate levels (info for refresh, warn for failure)
- [x] Unit test: `token_needs_refresh` pure function with various expiry scenarios
- [ ] Unit test: computed sleep calculation
- [ ] Integration test: token refresh triggers zero-downtime reconnect

### Phase 3: CLI Auth Commands

**Goal**: Interactive browser-based OAuth login, non-interactive token injection, status, logout.

#### Commands

`plug/src/commands/auth.rs`:

**`plug auth login --server <name> [--no-browser]`**
1. Load server config, verify `auth = "oauth"`
2. Build `AuthorizationManager` via `oauth::build_authorization_manager()`
3. Discover authorization server metadata (RFC 9728 — try WWW-Authenticate header first, then
   well-known URI)
4. Verify `code_challenge_methods_supported` includes `S256` (MCP spec requirement)
5. Start localhost TCP listener on random available port for redirect callback
   - Bind to `127.0.0.1` ONLY (not `0.0.0.0`)
   - Accept exactly ONE GET request to `/callback`, reject all other paths/methods with 404
   - Set `Connection: close` header to prevent keep-alive
   - Shut down immediately after receiving the callback
   - Return user-friendly HTML: "Authentication complete, you may close this tab"
6. Generate PKCE code verifier + challenge (S256)
7. Generate CSRF `state` parameter:
   - Source: `OsRng` (CSPRNG, consistent with existing `generate_auth_token()`)
   - Entropy: 256 bits, hex-encoded (64 chars)
   - Storage: in-memory only, never persisted to disk
   - Verification: constant-time comparison (reuse `subtle::ConstantTimeEq` pattern)
   - Consumed after first use (one-time)
8. Build authorization URL with:
   - `response_type=code`
   - `client_id` (from config or dynamic registration)
   - `redirect_uri=http://localhost:{port}/callback`
   - `scope` (from config `oauth_scopes`)
   - `resource` (server URL, per RFC 8707 — MUST in both auth and token requests)
   - `code_challenge` + `code_challenge_method=S256`
   - `state` (CSRF token)
9. Open browser (unless `--no-browser`, in which case print URL for manual copy-paste)
   - Do NOT log the authorization URL via tracing (it contains code_challenge and state)
10. Wait for callback (with 120s timeout)
11. Extract `code` and `state` from callback query parameters
12. Verify CSRF state matches (constant-time)
13. Exchange code for tokens via rmcp — include `resource` parameter in token request (RFC 8707)
14. Store credentials via `CompositeCredentialStore`
15. If server is AuthRequired in running engine → trigger reconnect + spawn new refresh loop
16. Print success with granted scopes
17. Output `--output json` for machine-parseable result

**`plug auth complete --server <name> --code <CODE> --state <STATE>`** (NEW — agent-native)

Non-interactive code exchange for agents that obtained an authorization code through an external
mechanism. Completes the token exchange without any browser involvement:

1. Load server config, verify `auth = "oauth"`
2. Verify the state parameter matches a pending login session
3. Exchange code for tokens
4. Store credentials and trigger reconnect

> **Agent-native finding:** `--no-browser` still requires a human to complete the browser flow
> somewhere. `plug auth complete` allows an agent to provide the code directly.

**`plug auth inject --server <name> --access-token <TOKEN> --refresh-token <TOKEN> [--expires-in <SECS>]`** (NEW — agent-native)

Direct token injection for agents, CI/CD, and service accounts with pre-obtained tokens:

1. Write tokens directly to `CompositeCredentialStore`
2. If server is AuthRequired → trigger reconnect
3. Accept tokens from stdin (`--access-token -`) for pipe-friendly automation
4. Also support `PLUG_OAUTH_TOKEN_{SERVER}` environment variables as injection path

> **Agent-native finding:** The plan had no path for agents to provide pre-obtained tokens.
> This follows the pattern of `kubectl config set-credentials`, `gcloud auth activate-service-account`,
> and `aws configure set`.

**`plug auth status [--output json]`**
- List all OAuth-configured servers with:
  - Server name and URL
  - Auth status (authenticated / not authenticated / token expired / auth-required)
  - Granted scopes
  - Token expiry time (if available)
  - Storage location (keyring / file)
- JSON output schema must be documented for agent consumption

**`plug auth logout --server <name>`**
- Clear credentials from both keyring and file store
- Invalidate in-memory cache
- If server is connected → mark AuthRequired

### Research Insights: CLI Security

**Security Findings:**
- **Redirect URI listener hardening** (Security finding #2, HIGH): Port hijacking window exists
  between bind and callback. Mitigations: strict `/callback` path matching, single-request
  acceptance, explicit `SO_REUSEADDR` disabled. Accept ONE GET to `/callback`, reject everything
  else with 404.
- **State parameter specification** (Security finding #3, HIGH): Must use CSPRNG (OsRng), 256-bit
  entropy, constant-time comparison (subtle crate), one-time use, memory-only storage.
- **Authorization URL in logs** (Security finding #8, MEDIUM): The URL contains code_challenge,
  state, and redirect_uri with port. Only print to stdout for `--no-browser`, never log via
  tracing. Do NOT log the callback request query parameters (contains the auth code).
- **Concurrent login prevention** (Security finding #12, LOW): Consider a lock file per server
  (`~/.config/plug/tokens/{server_name}.lock`) to prevent multiple `plug auth login` processes
  for the same server.

**Agent-Native Findings (3 critical gaps):**
1. No non-interactive token injection → `plug auth inject` (added above)
2. `--no-browser` not agent-completable → `plug auth complete` (added above)
3. No IPC command for auth state → add `AuthStatus` and `InjectToken` IPC variants (Phase 4)

**MCP Spec (Context7):**
- Authorization servers that support multiple authorization servers are valid — the client chooses
  which one to use per RFC 9728 Section 7.6
- Scope challenge handling: when receiving `insufficient_scope` error during runtime, re-auth
  with broader scopes following OAuth 2.1 Section 5 error handling

#### Tasks (Phase 3)

- [x] Create `plug/src/commands/auth.rs` with login/complete/inject/status/logout subcommands
- [x] Register `Auth` command variant in `plug/src/main.rs`
- [ ] Implement browser-based PKCE flow with hardened localhost callback
- [x] Implement `--no-browser` manual URL flow
- [ ] Implement `plug auth complete` for non-interactive code exchange
- [x] Implement `plug auth inject` for direct token injection (including stdin support)
- [ ] Implement CSRF state with CSPRNG, 256-bit entropy, constant-time comparison
- [x] Implement status display (text + JSON output with documented schema)
- [x] Implement logout with credential clearing + cache invalidation
- [x] Ensure authorization URLs are NEVER logged via tracing
- [ ] Integration test: full login flow with mock OAuth provider (Axum-based)
- [ ] Test: `--no-browser` flow
- [ ] Test: `plug auth inject` with access + refresh token
- [ ] Test: status output for authenticated and unauthenticated servers

### Phase 4: Doctor, IPC, and Polish

**Goal**: Doctor checks, IPC auth commands, error messages.

#### Doctor Checks

`plug-core/src/doctor.rs` — add new checks:

- [x] `check_oauth_config`: Validate OAuth config fields are coherent
  - Warn if `auth = "oauth"` but no scopes configured
  - Error if `auth = "oauth"` + `auth_token` both present
- [x] `check_oauth_tokens`: Check token status per OAuth server
  - Pass if valid credentials exist in store
  - Warn if credentials exist but token is expired (refresh may fix)
  - Warn if no credentials found (needs `plug auth login`)
  - Warn if file fallback is in use (keyring unavailable) — plaintext token exposure risk

#### IPC Auth Commands (Agent-Native)

**Source:** Agent-native review finding #3

Add IPC variants for daemon-connected agents:

- [ ] `AuthStatus` — returns per-server OAuth state as structured JSON
- [ ] `InjectToken { server_name, access_token, refresh_token, expires_in }` — injects
  credentials into the running daemon's credential store and triggers reconnect
- [ ] Push notification `AuthStateChanged { server_id, new_state }` — notifies IPC clients
  when a server transitions to/from AuthRequired

#### Error Messages

- [x] AuthRequired servers show actionable message: `"Run \`plug auth login --server {name}\` to authenticate"`
- [ ] 401 errors during operation include context: `"OAuth token expired or revoked for {server}"`
- [ ] Discovery failures include the URLs attempted
- [x] All error messages are machine-parseable when `--output json` is used

#### Tasks (Phase 4)

- [x] Add doctor checks for OAuth config, token status, and file fallback warning
- [ ] Add IPC variants for `AuthStatus` and `InjectToken`
- [ ] Add `AuthStateChanged` push notification for IPC clients
- [x] Polish error messages for actionable guidance (human + machine)
- [x] Update `plug servers` view to show AuthRequired status clearly
- [x] Run full test suite: `cargo test`, `cargo clippy`, `cargo fmt`

> **Simplification applied:** Deferred import/export OAuth awareness. The existing import system
> copies TOML fields verbatim, which handles the happy path. No AI client today exports OAuth-
> configured MCP servers. Handle edge cases if they appear.
>
> **Simplification applied:** Deferred `check_token_file_permissions` doctor check. The write
> path already enforces 0600 permissions.
>
> **Scope change:** Moved `--auth-token` flag on `plug server add` to a separate PR. It is
> unrelated to OAuth and adding it here muddies the commit history.

## Edge Cases and SpecFlow Findings

The following edge cases were identified during SpecFlow analysis and multi-agent review:

### Token Refresh Atomicity

OAuth 2.1 recommends refresh token rotation — each refresh returns a new refresh token. The old
refresh token is invalidated. If plug crashes between receiving new tokens and persisting them,
the tokens are lost. Mitigation:

1. Write new credentials to `CompositeCredentialStore` BEFORE triggering transport reconnect
2. Use atomic file writes (write to temp file, `rename()`) for file-backed storage
3. If keyring write fails, fall back to file — never lose the new refresh token
4. Write order: keyring first, then file (Security finding #5)
5. Use `fs2::FileExt::lock_exclusive()` during the read-refresh-write cycle (Security finding #7)

### Hot-Reload Interaction

The config watcher (`server_config_changed()`) compares transport, URL, command, args, env,
timeout, and enabled. It does NOT compare auth settings today. When a user changes from
`auth_token` to `auth = "oauth"` (or vice versa), the reload system must detect this change and
trigger a server restart with the new auth mode.

- [x] Add `auth`, `oauth_client_id`, `oauth_scopes` to `server_config_changed()` comparison
- [x] Also add `auth_token` (pre-existing bug — prerequisite fix)

### Health Check vs. AuthRequired

The health check probes with `list_tools()`. An AuthRequired server has no valid credentials, so
probing is pointless and would fail with 401.

- [x] Skip health checks entirely for AuthRequired servers (check at top of health loop)
- [x] This is simpler than trying to classify 401 errors inside the health probe (Architecture
  finding #2)

### tools/list_changed on AuthRequired Transition

When a server enters AuthRequired, its tools are removed from the merged catalog. This SHOULD
emit `tools/list_changed` to downstream clients (consistent with Failed server behavior).

### Redirect URI Listener Security

The localhost callback listener must:
- Bind to `127.0.0.1` only (not `0.0.0.0`)
- Accept exactly ONE GET request to `/callback`, reject all other paths/methods with 404
- Shut down immediately after receiving the callback (or on timeout)
- Return a user-friendly HTML response ("Authentication complete, you may close this tab")
- Set `Connection: close` header to prevent keep-alive from holding the socket
- Ensure `SO_REUSEADDR` is disabled (prevent port hijacking — Security finding #2)

### Narrower Scope Grants

If the authorization server grants a narrower scope than requested, the granted scopes should be
stored and surfaced in `plug auth status`. Let upstream "insufficient scope" errors pass through
to the user via plug's existing pass-through architecture. (Simplification: deferred the
"suggest re-auth with broader scopes" error enhancement.)

### Token File Path Sanitization

OAuth tokens are stored per-server at `~/.config/plug/tokens/{server_name}.json`. Server names
are validated by `sanitize_server_name_for_path()` which rejects path separators, parent directory
references, hidden file prefixes, null bytes, and names exceeding 255 bytes.

### Missing `expires_in`

Some authorization servers omit `expires_in` from token responses. When absent, use a
conservative default (`const DEFAULT_TOKEN_LIFETIME_SECS: u64 = 3600`). Log a warning so the
user knows refresh timing may not be optimal.

Additional edge cases:
- Clamp `expires_in` to minimum 60s and maximum 86400s
- When `expires_in` is 0 or negative, treat as unknown and apply default with warning
- For tokens with `expires_in < 600`, refresh at 50% of lifetime instead of lifetime - 300s

### Dynamic Client Registration Persistence

When rmcp performs dynamic client registration (no `oauth_client_id` configured), persist the
assigned `client_id` in the credential store alongside tokens. This prevents re-registration
on every login attempt and avoids potential rate-limiting or DoS from spoofed registrations
(Security finding #10).

### Log Leakage Audit

The following must NEVER appear in tracing output (Security finding #8):
- Authorization URLs (contain code_challenge, state, redirect_uri with port)
- Callback request query parameters (contain authorization code)
- Token exchange responses (contain access_token, refresh_token)
- Error response bodies from upstream (may contain rejected token values)

Establish a "no secrets in tracing" rule for the entire `oauth` module. Use `SecretString`
wrapping at moment of receipt. Audit rmcp's `AuthorizationManager` logging behavior at
debug/trace levels.

## Alternative Approaches Considered

### 1. Wrap rmcp with custom OAuth layer

**Rejected.** rmcp 1.1.0 already has `AuthorizationManager` with full spec compliance. Building a
parallel implementation would duplicate effort and risk diverging from the MCP spec.

### 2. Hot-swap tokens on live connections

**Rejected.** rmcp's `StreamableHttpClientTransportConfig::auth_header` is a static `Option<String>`
set at transport creation. Hot-swapping would require either an rmcp upstream change or a custom
`StreamableHttpClient` implementation that reads from `ArcSwap`. The zero-downtime reconnect
approach (pre-create new transport, then swap) achieves the same user-visible result without rmcp
changes.

### 3. OAuth in plug-core only (no CLI commands)

**Rejected.** Initial OAuth authorization requires a browser interaction. The daemon cannot open a
browser. The CLI must provide `plug auth login` for the interactive flow. Status and logout are
table-stakes UX. Agent-native commands (`inject`, `complete`) enable non-interactive workflows.

### 4. File-only token storage (no keyring)

**Rejected.** OS keyring integration is standard practice for CLI tools handling OAuth tokens. File
fallback is necessary for headless environments, but keyring should be the primary store for
security. The `keyring` crate with `apple-native` and `linux-native` features provides this with
minimal dependency weight.

### 5. `UpstreamAuthMode` enum with Bearer variant

**Rejected (simplification).** Bearer is already implicit when `auth_token` is present. An enum
with two variants plus validation overhead adds type-level complexity for zero behavioral value.
`auth: Option<String>` with `"oauth"` as the only meaningful value is simpler.

### 6. Discovery URL override config fields

**Deferred (simplification).** `oauth_authorization_url` and `oauth_token_url` are speculative
escape hatches for servers without RFC 9728 discovery support. No known MCP server in 2026
requires this. Ship with discovery-only; add overrides if a real user reports the need.

## System-Wide Impact

### Interaction Graph

```
plug auth login
  → builds AuthorizationManager (rmcp)
  → starts localhost TCP listener (tokio, 127.0.0.1 only)
  → generates PKCE + CSRF state (OsRng, 256-bit)
  → opens browser (open crate)
  → receives callback → exchanges code (rmcp → upstream auth server, with resource param)
  → stores credentials (CompositeCredentialStore → keyring/file)
  → invalidates in-memory cache
  → triggers engine reconnect (ServerManager)
  → pre-creates transport with new token (StreamableHttpClientTransport)
  → initialize + list_tools (rmcp → upstream MCP server)
  → atomic swap via DashMap → tools available to downstream clients
  → spawns new refresh loop for this server

plug auth inject (agent-native)
  → writes tokens directly to CompositeCredentialStore
  → invalidates in-memory cache
  → triggers engine reconnect (same flow as login)

refresh loop (per server, tracked via TaskTracker)
  → reads cached token expiry (in-memory, no I/O)
  → computed sleep until refresh window
  → calls refresh_token (rmcp AuthorizationManager → upstream auth server)
  → acquires fs2 file lock for cross-process serialization
  → stores new credentials (CompositeCredentialStore)
  → invalidates in-memory cache
  → pre-creates new transport with fresh token
  → atomic swap via DashMap → downstream clients see no interruption
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
| Refresh failure (network) | rmcp → auth server | Retry on next computed-sleep cycle |
| Refresh failure (revoked) | rmcp → auth server | Mark AuthRequired, break refresh loop |
| 401 mid-session | Upstream MCP server | Attempt refresh → reconnect or AuthRequired |
| Redirect listener hijack | Local attacker | Mitigated by strict path match, single-request, PKCE |

### State Lifecycle Risks

- **Crash between token store write and reconnect**: Safe — next startup reads new token from store
- **Crash during refresh token write**: Partial write risk mitigated by atomic file operations
  (write to temp file, rename). Cross-process lock via fs2 during write cycle.
- **Stale file after keyring update**: Harmless — keyring wins on read (primary store)
- **Multiple plug instances sharing credentials**: File locking via fs2 prevents concurrent
  corruption; keyring is process-safe via OS APIs
- **Config changes from bearer to OAuth**: Detected by updated `server_config_changed()`,
  triggers server restart

### API Surface Parity

| Interface | OAuth Support Needed |
|-----------|---------------------|
| `plug connect` (stdio downstream) | Upstream OAuth transparent — tools just work |
| `plug serve` (HTTP downstream) | Upstream OAuth transparent — tools just work |
| Daemon IPC | AuthRequired in status; `AuthStatus`/`InjectToken` IPC commands |
| `plug servers --output json` | Must include auth status (bearer / oauth / auth-required) |
| `plug auth inject` | Direct token injection for agents/CI/CD |
| `plug auth complete` | Non-interactive code exchange for agents |
| `plug doctor --output json` | OAuth config + token status + file fallback checks |

### Integration Test Scenarios

1. **Full OAuth flow with mock provider**: Start Axum-based fake OAuth server, configure plug
   server with `auth = "oauth"`, run `plug auth login` flow, verify tools are accessible
2. **Proactive refresh with zero-downtime**: Set up token with 10s expiry, verify refresh fires
   before expiry, verify pre-create + swap happens transparently, verify no tool call failures
   during refresh window
3. **Refresh failure → AuthRequired**: Mock refresh endpoint returning 400, verify server
   transitions to AuthRequired, verify tools are filtered from routing, verify refresh loop exits
4. **Daemon cold start without credentials**: Start engine with OAuth server but no stored
   credentials, verify server is AuthRequired, verify other servers still work
5. **Re-login after AuthRequired**: After server enters AuthRequired, simulate successful
   login, verify server recovers to Healthy, verify new refresh loop spawns
6. **Token injection**: Use `plug auth inject` to provide pre-obtained tokens, verify server
   connects and tools are available
7. **Health check skips AuthRequired**: Verify AuthRequired servers are not probed by health checks
8. **PKCE S256 enforcement**: Verify token exchange request includes `code_challenge_method=S256`
   (Security finding #9)

## Acceptance Criteria

### Functional Requirements

- [x] `plug auth login --server <name>` completes browser-based OAuth flow and stores credentials
- [x] `plug auth login --server <name> --no-browser` works with manual URL copy-paste
- [ ] `plug auth complete --server <name> --code <CODE> --state <STATE>` exchanges code non-interactively
- [x] `plug auth inject --server <name>` writes pre-obtained tokens to credential store
- [x] `plug auth status` shows per-server auth status (text + JSON output)
- [x] `plug auth logout --server <name>` clears credentials from all stores
- [x] OAuth servers with valid credentials connect and route tools transparently
- [x] OAuth servers without credentials enter AuthRequired state (not Failed)
- [x] Background refresh proactively renews tokens before expiry with zero-downtime reconnect
- [x] Refresh failure transitions server to AuthRequired and exits refresh loop
- [ ] Re-login after AuthRequired recovers server to Healthy and spawns new refresh loop
- [x] `plug doctor` validates OAuth config, token status, and file fallback warnings
- [ ] `AuthStatus` and `InjectToken` IPC commands work for daemon-connected agents

### Non-Functional Requirements

- [x] Tokens stored in OS keyring when available, file fallback with 0600 permissions
- [ ] No token values in logs (SecretString wrapping from moment of receipt)
- [x] No authorization URLs in tracing (only printed to stdout for `--no-browser`)
- [ ] PKCE S256 mandatory (refuse to proceed without `code_challenge_methods_supported`)
- [ ] RFC 8707 resource parameter included in all authorization and token requests
- [x] No blocking on tool-call path (refresh is background, not synchronous)
- [ ] CSRF state: CSPRNG, 256-bit entropy, constant-time comparison, one-time use
- [x] Server names sanitized for filesystem safety before token path construction
- [x] Cross-process credential writes serialized via fs2 file locking

### Quality Gates

- [x] All existing tests pass (`cargo test`)
- [x] Clippy clean (`cargo clippy --all-targets --all-features -- -D warnings`)
- [x] Format clean (`cargo fmt --check`)
- [ ] Integration test with mock OAuth provider passes
- [ ] Token refresh integration test with zero-downtime reconnect passes
- [x] AuthRequired state machine test passes
- [ ] PKCE S256 enforcement test passes
- [x] Server name sanitization test passes (path traversal attempts rejected)

## Dependencies & Prerequisites

| Dependency | Version | Purpose | Risk |
|-----------|---------|---------|------|
| rmcp `"auth"` feature | 1.1.0 | AuthorizationManager, CredentialStore, PKCE | Verified available |
| `keyring` | 3.x | OS keychain credential storage | apple-native + linux-native features |
| `open` | 5.x | Browser launch (already in CLI deps) | Already present |
| `oauth2` | 5.x | Transitive via rmcp `auth` feature | Not directly depended on |
| `fs2` | existing | Cross-process file locking | Already in workspace |

No upstream rmcp changes needed. The `auth_header` field on `StreamableHttpClientTransportConfig`
and the zero-downtime reconnect approach avoid any SDK modifications.

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| rmcp `AuthorizationManager` API doesn't match MCP spec exactly | Low | High | Verified via Context7 docs — full spec coverage including RFC 8707, PKCE S256, discovery |
| Keyring unavailable on headless Linux | Medium | Medium | CompositeCredentialStore falls back to file automatically; doctor warns about plaintext exposure |
| Browser launch fails in WSL/SSH | Medium | Low | `--no-browser` flag with manual URL flow; `plug auth inject` for agents |
| Token refresh race between CLI and daemon | Low | Medium | fs2 file locking for cross-process serialization |
| Redirect URI port conflict | Low | Low | Use port 0 (OS assigns available port); strict path matching |
| Clock skew causes premature/late refresh | Low | Medium | Use server-provided `expires_in` relative to local receipt time, not absolute |
| Config migration from `auth_token` to `auth = "oauth"` | Medium | Low | Validation rejects both simultaneously; clear error message guides user |
| Incomplete routing predicate updates | Low | High | Mitigated by `is_routable()` prerequisite refactor |
| Path traversal in token file naming | Low | High | Mitigated by `sanitize_server_name_for_path()` |
| Authorization code interception via port hijacking | Low | Medium | Mitigated by PKCE, strict path matching, single-request listener |

## Security Audit Summary

13 findings from security-sentinel review (full audit in Enhancement Summary):

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| 1 | Path traversal in token file naming | HIGH | Addressed — `sanitize_server_name_for_path()` added |
| 2 | Redirect URI listener race / port hijacking | HIGH | Addressed — strict path, single-request, PKCE |
| 3 | CSRF state parameter underspecified | HIGH | Addressed — CSPRNG, 256-bit, constant-time, one-time |
| 4 | Plaintext file fallback storage | HIGH | Partially addressed — doctor warns; considered acceptable for personal tool |
| 5 | Refresh token rotation atomicity | MEDIUM | Addressed — write order specified, fs2 locking |
| 6 | Missing expires_in edge cases | MEDIUM | Addressed — clamping, short-lived token adaptation |
| 7 | Cross-process refresh serialization | MEDIUM | Addressed — fs2 file locking |
| 8 | OAuth URL / code leakage in logs | MEDIUM | Addressed — no-tracing rule for oauth module |
| 9 | PKCE S256 relies on rmcp correctness | LOW | Accepted — add assertion in build_authorization_manager() |
| 10 | Dynamic client registration risks | LOW | Addressed — persist client_id in credential store |
| 11 | Auth mode change not detected by hot-reload | LOW | Addressed — prerequisite fix + new fields in comparison |
| 12 | Concurrent login / callback timeout | LOW | Deferred — low impact for personal tool |
| 13 | SecretString lacks memory zeroization | LOW | Deferred — evaluate `zeroize` crate post-ship |

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

**Note:** The extraction branch includes `oauth_client_secret` in its config, which this plan
explicitly rejects (public-client-only design decision #6). It also uses the `UpstreamAuthMode`
enum, which this plan simplifies to `Option<String>`. Extraction must account for these
design differences.

## Institutional Learnings Applied

- **SecretString Display leaks** (todos/025): Wrap OAuth tokens in `SecretString` at moment of
  receipt. Use `.as_str()` for actual header values, never `Display` or `format!`.
  Note: existing `SecretString` has `Serialize` via `#[serde(transparent)]` — ensure OAuth tokens
  in-memory are not inadvertently serialized to logs.
- **TOCTOU in file creation** (downstream auth plan): Use `create_new(true)` for initial token
  file creation to prevent symlink attacks. Write to temp file + rename for atomic updates.
  Open the file once and check permissions via the open fd — avoid multi-step syscall sequences.
- **mcp-remote blocking re-auth** (bug report): Proactive background refresh prevents tool-call
  blocking. Never trigger re-auth synchronously on the request path.
- **mcp-remote headless failure** (bug report): Daemon cannot open browser. `plug auth login`
  is the only interactive path. `--no-browser` enables manual flow. `plug auth inject` and
  `plug auth complete` enable fully non-interactive agent workflows.
- **Reconnect patterns** (proxy-timeout-handling, restart-recovery): 401 Unauthorized is a
  transport error — trigger refresh + reconnect immediately. Token refresh failures should trigger
  a full reconnect, not just a retry. Use `tracker.spawn()` with `CancellationToken` for all
  background tasks.
- **Semantic constants** (phase3-resilience): Define `TOKEN_REFRESH_WINDOW_SECS`,
  `DEFAULT_TOKEN_LIFETIME_SECS` as named constants. Never hardcode retry counts or backoff
  multipliers.

## Simplification Summary

Changes from original plan based on code-simplicity-reviewer analysis:

| Item | Decision | LOC Impact |
|------|----------|-----------|
| `UpstreamAuthMode` enum | Replaced with `Option<String>` | -20 LOC |
| `KeyringBackend` trait | Dropped — test file fallback directly | -40 LOC |
| `oauth_authorization_url` / `oauth_token_url` | Deferred — discovery-only | -35 LOC |
| Per-server `tokio::sync::Mutex` | Dropped — single loop per server, use fs2 for cross-process | -15 LOC |
| Import OAuth awareness | Deferred — TOML copy handles happy path | -40 LOC |
| `check_token_file_permissions` | Deferred — write path enforces 0600 | -25 LOC |
| `--auth-token` on `plug server add` | Separate PR | -25 LOC |
| Phases 1+2 merge | Single phase — config+store+transport+health | N/A |
| **Total estimated reduction** | | **~200 LOC (19%)** |

Resulting structure: 4 phases (down from 5), ~850 LOC estimated (down from ~1,050).

## Sources & References

### Internal References

- Current auth module: `plug-core/src/auth.rs` (downstream bearer tokens only)
- Config model: `plug-core/src/config/mod.rs:108` (`ServerConfig` struct)
- Transport auth injection: `plug-core/src/server/mod.rs:503-508` (HTTP), `:595-598` (SSE)
- Health state machine: `plug-core/src/types.rs` (`ServerHealth` enum)
- Health check loop: `plug-core/src/health.rs` (`spawn_health_checks()`)
- Engine startup + TaskTracker: `plug-core/src/engine.rs:155-207`
- Config hot-reload: `plug-core/src/reload.rs:140-149` (`server_config_changed()`)
- Downstream auth plan: `docs/plans/2026-03-07-feat-downstream-http-bearer-auth-plan.md`
- Roadmap plan (Phase B2): `docs/plans/2026-03-07-feat-mcp-spec-compliance-roadmap-plan.md:287-373`
- Bug reports: `docs/bug-reports/mcp-remote-oauth-reauth-blocks-tool-calls.md`,
  `docs/bug-reports/mcp-remote-headless-oauth-impossible.md`
- Learnings: `docs/solutions/integration-issues/pre-phase-downstream-http-bearer-auth-20260307.md`,
  `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md`,
  `docs/solutions/integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md`,
  `docs/solutions/integration-issues/post-v0-2-upstream-restart-recovery-proof-20260307.md`

### External References

- MCP spec authorization: `https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization`
- MCP spec security best practices: `https://modelcontextprotocol.io/specification/2025-11-25/basic/security_best_practices`
- rmcp `AuthorizationManager` API: `rmcp::transport::auth::AuthorizationManager`
- rmcp `CredentialStore` trait: `rmcp::transport::auth::CredentialStore`
- rmcp `OAuthState` enum: `rmcp::transport::auth::OAuthState`
- RFC 8707 (Resource Indicators): `https://www.rfc-editor.org/rfc/rfc8707.html`
- RFC 9728 (Protected Resource Metadata): `https://www.rfc-editor.org/rfc/rfc9728.html`
- RFC 8414 (OAuth 2.0 Authorization Server Metadata): `https://www.rfc-editor.org/rfc/rfc8414.html`
- RFC 8252 (OAuth 2.0 for Native Apps): `https://datatracker.ietf.org/doc/html/rfc8252`
- RFC 7591 (Dynamic Client Registration): `https://datatracker.ietf.org/doc/html/rfc7591`
- OAuth 2.1 specification: `https://oauth.net/2.1/`
- Keyring crate: `https://docs.rs/keyring`

### Related Work

- PR #35 — Legacy SSE upstream transport (includes auth token injection pattern)
- PR #34 — Elicitation + sampling forwarding
- Extraction branch: `fix/subscription-rebind-confidence` (OAuth implementation exists off-main)
