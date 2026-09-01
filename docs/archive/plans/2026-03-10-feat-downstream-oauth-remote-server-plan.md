# Downstream OAuth Remote Server Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Turn `plug` into a reliable always-on remote MCP server for a Mac mini, with local stdio clients and remote HTTPS clients sharing the same upstream tool estate through a correct downstream auth model.

**Architecture:** Keep the current transport split: daemon IPC for local stdio clients, Streamable HTTP for remote clients. Replace the current bind-address-driven downstream auth behavior with explicit downstream auth modes, make downstream OAuth the primary internet-facing mode, and keep bearer auth as a compatibility mode for non-OAuth remote clients. Deploy the public edge behind a stable HTTPS hostname and treat quick tunnels as debugging-only.

**Tech Stack:** Rust, rmcp 1.1.x, Axum, axum-server/rustls, existing `plug-core::auth` primitives, existing IPC daemon/runtime code, Cloudflare named tunnel or equivalent reverse proxy.

---

## Scope

This plan intentionally does **not** redesign away from Streamable HTTP. That is the current MCP remote transport. The root fix is to make `plug serve` a correct remote HTTP server with explicit auth semantics and stable deployment.

This plan also intentionally separates:

- product changes required in `plug`
- deployment choices for the Mac mini
- client-specific validation for Claude and other remote MCP consumers

## Non-Goals

- ACME/Let's Encrypt automation inside `plug`
- replacing Cloudflare with a custom ingress stack
- changing upstream auth architecture
- introducing a new remote transport beyond Streamable HTTP

## Root Problem

Current downstream auth is derived from bind address:

- loopback bind: no downstream auth
- non-loopback bind: downstream bearer auth

That is insufficient for the real deployment shape:

- `plug` may remain loopback-bound on the Mac mini
- the machine may still be internet-reachable via a tunnel or reverse proxy
- remote clients may require OAuth rather than static bearer tokens

So the auth model is attached to the wrong signal.

## Target Product Shape

At the end of this work:

- `plug serve --daemon` is the stable local service for stdio clients
- `plug serve` is the stable downstream HTTP service for remote clients
- `plug serve` can require auth even when loopback-bound
- downstream auth mode is operator-configurable
- downstream OAuth is the primary auth flow for internet-facing deployments
- downstream bearer auth remains available for generic non-OAuth remote clients
- remote clients connect to `https://<stable-host>/mcp`
- named/stable tunnel or reverse proxy is the supported deployment path

## Design Decisions To Lock First

### Decision 1: Add explicit downstream auth mode

Add a downstream config mode that is independent of bind address:

- `http.auth_mode = "none" | "bearer" | "oauth"`

Rules:

- `none` is only valid for local/dev use
- `bearer` is valid for generic remote clients and scripted clients
- `oauth` is the preferred mode for Claude remote connectors and other browser-like clients
- bind address no longer determines auth mode by itself

### Decision 2: Keep HTTPS at the edge, not inside auth logic

TLS remains a serving/deployment concern:

- direct TLS in `plug` remains supported
- tunnel/proxy HTTPS remains supported
- downstream auth must work in either deployment

### Decision 3: Add explicit public origin and public base URL support

Remote deployments need config that reflects the real external URL:

- `http.public_base_url = "https://plug.example.com"`
- `http.allowed_origins = [...]`

This avoids inferring public URLs from local bind state and makes OAuth redirect generation correct.

### Decision 4: Treat named tunnels as production, quick tunnels as debugging-only

Quick tunnels are not acceptable for the Mac mini production target because the hostname is ephemeral. The documented supported deployment should use:

- Cloudflare named tunnel with stable hostname, or
- equivalent stable reverse proxy with HTTPS

## Phase 0: Pre-Implementation Design Validation

### Task 1: Write the downstream auth-mode ADR

**Files:**
- Create: `docs/decisions/2026-03-10-downstream-auth-mode.md`
- Reference: `docs/DECISIONS.md`
- Reference: `docs/plans/2026-03-07-feat-downstream-http-bearer-auth-plan.md`
- Reference: `docs/solutions/integration-issues/pre-phase-downstream-http-bearer-auth-20260307.md`

**Step 1: Write the ADR**

Document:

- why bind-address-derived auth is insufficient
- why Streamable HTTP remains the correct transport
- why downstream OAuth is the primary remote mode
- why bearer remains as compatibility mode

**Step 2: Review against current code**

Check:

- `plug-core/src/config/mod.rs`
- `plug/src/runtime.rs`
- `plug-core/src/http/server.rs`

Expected result: ADR matches current architecture boundaries and does not require transport rewrites.

**Step 3: Commit**

```bash
git add docs/decisions/2026-03-10-downstream-auth-mode.md
git commit -m "docs: add downstream auth mode ADR"
```

## Phase 1: Config Model Refactor

### Task 2: Add explicit downstream auth config

**Files:**
- Modify: `plug-core/src/config/mod.rs`
- Test: `plug-core/src/config/mod.rs`
- Reference: `plug-core/src/doctor.rs`

**Step 1: Write failing config validation tests**

Add tests for:

- `auth_mode = "bearer"` on loopback is valid
- `auth_mode = "oauth"` on loopback is valid
- `auth_mode = "none"` on non-loopback is rejected unless explicitly marked dev-only
- `public_base_url` is required when `auth_mode = "oauth"`
- unknown auth mode fails validation

**Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p plug-core validate_downstream_auth_mode -- --nocapture
```

Expected: failing tests for missing fields and new enum parsing.

**Step 3: Implement minimal config model**

Add to `HttpConfig`:

```rust
pub auth_mode: DownstreamAuthMode,
pub public_base_url: Option<String>,
pub oauth_client_id: Option<String>,
pub oauth_client_secret: Option<crate::types::SecretString>,
pub oauth_scopes: Option<Vec<String>>,
```

Add enum:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownstreamAuthMode {
    #[default]
    None,
    Bearer,
    Oauth,
}
```

**Step 4: Add config validation**

Validate:

- `oauth` requires `public_base_url`
- `oauth` requires coherent client registration settings
- `bearer` and `oauth` are both legal on loopback
- non-loopback still requires TLS or secure deployment signaling

**Step 5: Run tests to verify pass**

Run:

```bash
cargo test -p plug-core validate_downstream_auth_mode -- --nocapture
```

**Step 6: Commit**

```bash
git add plug-core/src/config/mod.rs
git commit -m "feat(config): add explicit downstream auth mode"
```

### Task 3: Update defaults and migration behavior

**Files:**
- Modify: `plug-core/src/config/mod.rs`
- Modify: `plug/src/views/overview.rs`
- Test: `plug-core/src/config/mod.rs`

**Step 1: Write migration/compatibility tests**

Cover:

- existing configs without `http.auth_mode` still load
- legacy behavior maps to safe defaults
- overview/status output reflects explicit auth mode

**Step 2: Implement backward-compatible defaults**

Default recommendation:

- default auth mode remains effectively local-safe
- if `bind_address` is loopback and no auth config exists, treat as `none`
- if `bind_address` is non-loopback and no auth config exists, map to `bearer` during compatibility window

**Step 3: Update status surface**

Show:

- `Auth Mode: none|bearer|oauth`
- `Public Base URL: ...` when configured

**Step 4: Verify**

Run:

```bash
cargo test -p plug-core config_defaults -- --nocapture
cargo test -p plug overview -- --nocapture
```

**Step 5: Commit**

```bash
git add plug-core/src/config/mod.rs plug/src/views/overview.rs
git commit -m "feat(config): surface downstream auth mode in runtime views"
```

## Phase 2: Decouple Downstream Bearer Auth From Bind Address

### Task 4: Refactor bearer token loading to auth-mode-driven behavior

**Files:**
- Modify: `plug/src/runtime.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/doctor.rs`
- Test: `plug-core/src/http/server.rs`
- Test: `plug-core/src/doctor.rs`

**Step 1: Write failing tests**

Cover:

- loopback + `auth_mode = bearer` requires bearer token
- loopback + `auth_mode = none` stays unauthenticated
- non-loopback + `auth_mode = bearer` still works
- doctor warns/fails based on auth mode, not bind address alone

**Step 2: Implement runtime token behavior**

Change `cmd_serve()` so bearer token setup is driven by:

```rust
match config.http.auth_mode {
    DownstreamAuthMode::Bearer => { /* load or generate token */ }
    _ => None
}
```

Do not special-case loopback here.

**Step 3: Implement doctor updates**

Doctor should report:

- `none`: local-only/no auth
- `bearer`: token state and permissions
- `oauth`: OAuth config coherence and metadata readiness

**Step 4: Verify**

Run:

```bash
cargo test -p plug-core auth_valid_token_bypasses_origin_check -- --nocapture
cargo test -p plug-core check_http_auth -- --nocapture
```

**Step 5: Commit**

```bash
git add plug/src/runtime.rs plug-core/src/http/server.rs plug-core/src/doctor.rs
git commit -m "refactor(http): drive downstream bearer auth by auth mode"
```

## Phase 3: Downstream OAuth Foundation

### Task 5: Define downstream OAuth architecture seam

**Files:**
- Create: `plug-core/src/downstream_oauth/mod.rs`
- Modify: `plug-core/src/lib.rs`
- Create: `docs/research/downstream-oauth-design-notes.md`

**Step 1: Write the seam doc**

Define responsibilities:

- metadata endpoint exposure
- authorization request generation
- callback/exchange handling
- token/session issuance
- identity mapping between OAuth subject and MCP session

**Step 2: Create minimal module skeleton**

Add placeholder types:

```rust
pub struct DownstreamOauthConfig { /* parsed config */ }
pub struct DownstreamOauthManager { /* state + helpers */ }
pub enum DownstreamAuthChallenge { Redirect(String), Unauthorized }
```

**Step 3: Verify compile**

Run:

```bash
cargo check
```

**Step 4: Commit**

```bash
git add plug-core/src/downstream_oauth/mod.rs plug-core/src/lib.rs docs/research/downstream-oauth-design-notes.md
git commit -m "feat(oauth): add downstream OAuth architecture seam"
```

### Task 6: Add discovery and metadata endpoints for downstream OAuth

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug/src/runtime.rs`
- Test: `plug-core/src/http/server.rs`

**Step 1: Write failing HTTP tests**

Add tests for:

- `/.well-known/oauth-authorization-server`
- `/.well-known/openid-configuration` only if needed by chosen design
- advertised endpoints reflect `http.public_base_url`

**Step 2: Implement metadata handlers**

Expose metadata only when `auth_mode = oauth`.

Minimum fields should include:

- authorization endpoint
- token endpoint
- supported grant type / PKCE expectations
- supported auth methods for client registration if applicable

**Step 3: Verify**

Run:

```bash
cargo test -p plug-core downstream_oauth_metadata -- --nocapture
```

**Step 4: Commit**

```bash
git add plug-core/src/http/server.rs plug/src/runtime.rs
git commit -m "feat(http): add downstream OAuth metadata endpoints"
```

### Task 7: Implement downstream OAuth authorization flow

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Create: `plug-core/src/downstream_oauth/state.rs`
- Create: `plug-core/src/downstream_oauth/tokens.rs`
- Test: `plug-core/src/http/server.rs`

**Step 1: Write failing end-to-end auth tests**

Cover:

- unauthenticated request receives OAuth challenge/redirect path
- authorization request stores state securely
- callback validates state
- token exchange issues authenticated downstream session

**Step 2: Implement authorization start**

Add auth start endpoint and state storage. Reuse existing security patterns from upstream OAuth where sensible, but do not force the same module boundaries if the lifecycle is materially different.

**Step 3: Implement callback/exchange**

Issue authenticated server-side identity/session after successful OAuth completion.

Important: MCP session creation and OAuth auth state should be linked but not conflated.

**Step 4: Verify**

Run:

```bash
cargo test -p plug-core downstream_oauth_flow -- --nocapture
```

**Step 5: Commit**

```bash
git add plug-core/src/http/server.rs plug-core/src/downstream_oauth/state.rs plug-core/src/downstream_oauth/tokens.rs
git commit -m "feat(oauth): implement downstream OAuth authorization flow"
```

## Phase 4: HTTP Request Authentication Unification

### Task 8: Unify downstream request auth handling across none/bearer/oauth

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Test: `plug-core/src/http/server.rs`

**Step 1: Write failing matrix tests**

Matrix:

- `none` + no auth → pass
- `bearer` + valid header → pass
- `bearer` + invalid header → 401
- `oauth` + unauthenticated browser-style request → challenge
- `oauth` + authenticated session/cookie/token → pass

**Step 2: Implement single auth dispatcher**

Replace bind-derived logic with auth-mode-derived logic:

```rust
match state.auth_mode {
    DownstreamAuthMode::None => ...
    DownstreamAuthMode::Bearer => ...
    DownstreamAuthMode::Oauth => ...
}
```

**Step 3: Re-check origin semantics**

Origin validation rules should remain strict, but authentication success should still bypass the unsafe-localhost-only fallback path.

**Step 4: Verify**

Run:

```bash
cargo test -p plug-core http::server::tests -- --nocapture
```

**Step 5: Commit**

```bash
git add plug-core/src/http/server.rs
git commit -m "refactor(http): unify downstream auth handling across auth modes"
```

## Phase 5: Deployment-Grade Remote Server Support

### Task 9: Add explicit reverse-proxy / tunnel deployment support

**Files:**
- Modify: `plug-core/src/config/mod.rs`
- Modify: `plug-core/src/doctor.rs`
- Create: `docs/plans/deployment/mac-mini-remote-server.md`

**Step 1: Write deployment validation checks**

Doctor should warn if:

- `auth_mode = oauth` or `bearer` with no `public_base_url`
- stable remote mode configured but hostname appears ephemeral
- loopback bind + remote auth mode but no stable public URL documented

**Step 2: Document supported deployment modes**

Document exactly two supported production deployments:

- direct HTTPS bind on Mac mini
- loopback bind behind stable named tunnel / reverse proxy

Mark quick tunnels as non-production.

**Step 3: Verify**

Run:

```bash
plug doctor
plug config check
```

**Step 4: Commit**

```bash
git add plug-core/src/config/mod.rs plug-core/src/doctor.rs docs/plans/deployment/mac-mini-remote-server.md
git commit -m "docs: define supported remote deployment modes"
```

### Task 10: Add session visibility parity for remote HTTP sessions

**Files:**
- Modify: `plug/src/views/overview.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/ipc.rs` or relevant status surfaces
- Test: targeted runtime/overview tests

**Step 1: Write failing UX/state tests**

Cover:

- remote HTTP sessions appear in status views
- transport type is visible
- session auth mode / client type is visible

**Step 2: Implement HTTP session visibility**

Add operator-visible labeling:

- `transport = http`
- `auth = none|bearer|oauth`
- detected client metadata if available

**Step 3: Verify**

Run:

```bash
plug status --output json
plug clients --output json
```

**Step 4: Commit**

```bash
git add plug/src/views/overview.rs plug-core/src/http/server.rs
git commit -m "feat(ux): surface remote HTTP session details in status views"
```

## Phase 6: Real Client Verification

### Task 11: Create deterministic local verification harness

**Files:**
- Create: `scripts/verify_remote_server.sh`
- Create: `docs/testing/remote-server-smoke-test.md`

**Step 1: Write verification script**

The script should check:

- initialize on local `/mcp`
- local `Origin: https://claude.ai`
- auth-mode-specific request behavior
- full `tools/list` shape
- no pagination for current tool counts if page-size workaround remains active

**Step 2: Verify locally**

Run:

```bash
bash scripts/verify_remote_server.sh
```

Expected: all checks pass and print the active auth mode plus tool count.

**Step 3: Commit**

```bash
git add scripts/verify_remote_server.sh docs/testing/remote-server-smoke-test.md
git commit -m "test: add remote server smoke test harness"
```

### Task 12: Validate against real remote clients

**Files:**
- Modify: `docs/testing/remote-server-smoke-test.md`
- Modify: `docs/CLIENT-COMPAT.md`

**Step 1: Validate with Claude custom connector**

Use:

- stable URL `https://<stable-host>/mcp`
- `oauth` mode if implemented
- confirm tool discovery
- confirm tool execution

**Step 2: Validate with at least one generic MCP HTTP client**

Examples:

- Codex HTTP MCP
- another `mcp-remote`-compatible client

**Step 3: Record findings**

Document:

- what works
- client auth expectations
- pagination behavior
- SSE behavior if relevant

**Step 4: Commit**

```bash
git add docs/testing/remote-server-smoke-test.md docs/CLIENT-COMPAT.md
git commit -m "docs: record validated remote client compatibility"
```

## Recommended Deployment End State

For the Mac mini production deployment:

- `plug serve --daemon` managed by launchd
- `plug serve` managed by launchd
- stable named Cloudflare tunnel or equivalent proxy to `http://127.0.0.1:3282`
- public hostname fixed and durable
- remote clients configured against `https://<stable-host>/mcp`
- downstream `oauth` auth mode for Claude and browser-like remote clients
- downstream `bearer` mode retained as compatibility fallback for generic clients

## Deferred Work

Do not include these in the first tranche:

- full OpenID Connect identity surface if not required by clients
- multi-tenant user management beyond what downstream OAuth minimally requires
- tunnel automation inside `plug`
- certificate lifecycle automation inside `plug`

## Final Acceptance Criteria

- `plug` supports explicit downstream auth modes independent of bind address
- loopback-bound remote deployments can still require downstream auth
- downstream OAuth works for remote connectors that expect OAuth
- downstream bearer auth still works for generic remote clients
- local stdio clients and remote HTTP clients run concurrently without separate configuration silos
- operator can deploy on a Mac mini with a stable hostname and no quick-tunnel dependency
- status/doctor surfaces clearly describe remote deployment state

## Suggested Implementation Order

1. Phase 0
2. Phase 1
3. Phase 2
4. Phase 5 Task 9
5. Phase 3
6. Phase 4
7. Phase 5 Task 10
8. Phase 6

Plan complete and saved to `docs/plans/2026-03-10-feat-downstream-oauth-remote-server-plan.md`. Two execution options:

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

Which approach?
