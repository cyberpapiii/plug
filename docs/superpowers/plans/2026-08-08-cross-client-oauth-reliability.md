# Cross-Client Downstream OAuth Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make downstream OAuth client-neutral, retry-safe, and understandable while preserving PKCE, exact redirects, resource binding, and single-use authorization codes.

**Architecture:** Keep OAuth policy in `downstream_oauth`; add a bounded memory-only completed-consent cache beside pending consent state. Keep machine OAuth errors as JSON, add stable safe descriptions, and render browser authorization errors as HTML. Test protocol capability classes rather than branching on client brands.

**Tech Stack:** Rust, Tokio, Axum, Serde, RMCP, Cargo test.

## Global Constraints

- First-time authorization requires **Connect** and local **Allow**.
- Never auto-approve a new client.
- Repeated consent creates no second authorization code.
- Authorization codes remain single-use.
- No client-name or vendor-domain branches in production.
- No tokens, codes, metadata bodies, paths, or internal network details in user-facing errors.
- Streamable HTTP remains the canonical downstream remote transport; users never choose transport during OAuth.

---

### Task 1: Retry-Safe Consent Core

**Files:**
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Test: `plug-core/src/downstream_oauth/mod.rs`

**Interfaces:**
- Consumes: `DownstreamOauthState`, `PendingConsent`, `DownstreamOauthManager::decide_consent`.
- Produces: memory-only `completed_consents` state mapping one `consent_id` to one `AuthorizationRedirect` until expiry.

- [ ] **Step 1: Write failing tests**

Add tests proving two approvals return identical locations, pending authorization-code count remains `1`, a later denial returns the first approval result, and two denials return the same `access_denied` location.

```rust
let first = manager.decide_consent(&consent.consent_id, true).await.unwrap();
let repeated = manager.decide_consent(&consent.consent_id, true).await.unwrap();
assert_eq!(repeated.location, first.location);
assert_eq!(manager.state.lock().await.pending_codes.len(), 1);
```

- [ ] **Step 2: Verify RED**

Run: `cargo test -p plug-core downstream_oauth::tests::repeated_consent`

Expected: second call fails with `InvalidAuthorizationRequest`.

- [ ] **Step 3: Implement bounded completed decisions**

Add a skipped, memory-only map to `DownstreamOauthState`:

```rust
#[serde(skip)]
completed_consents: HashMap<String, CompletedConsent>,
```

Store the first redirect only after successful state persistence for approval. Check completed entries before removing a pending consent. Expire completed entries in `evict_expired`, clear them on persisted-state load, and remove client-owned entries during revocation.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p plug-core downstream_oauth::tests::repeated_consent`

Expected: all matching tests pass.

### Task 2: Actionable OAuth Errors

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Test: `plug-core/src/http/server.rs`

**Interfaces:**
- Consumes: `DownstreamOauthError`, `oauth_error_response`, `oauth_authorize`.
- Produces: stable `(error, error_description)` JSON and browser-safe HTML for authorization endpoint failures.

- [ ] **Step 1: Write failing tests**

Assert token/registration-style failures contain both fields:

```rust
assert_eq!(body["error"], "invalid_request");
assert!(body["error_description"].as_str().unwrap().contains("Try connecting again"));
```

Assert authorization failures requested as HTML return `text/html`, readable title, stable error code, and `Cache-Control: no-store`.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p plug-core http::server::tests::oauth_error`

Expected: `error_description` or HTML content assertion fails.

- [ ] **Step 3: Implement centralized safe descriptions**

Add one exhaustive mapping from `DownstreamOauthError` to public code and safe description. Reuse it for JSON, redirect parameters, and browser HTML. Do not include `Persistence(String)` contents.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p plug-core http::server::tests::oauth_error`

Expected: all matching tests pass.

### Task 3: Client-Neutral Acceptance Matrix

**Files:**
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `docs/solutions/integration-issues/multi-client-downstream-oauth-codex-5.6-sol.md`

**Interfaces:**
- Consumes: metadata validation, DCR, consent, token, authenticated `/mcp`, refresh, reload, revoke.
- Produces: regression matrix covering capability supersets and lifecycle behavior without production vendor branches.

- [ ] **Step 1: Add recorded capability fixtures**

Use literal JSON for strict DCR, baseline CIMD, extension-rich CIMD, and missing-code-flow CIMD. Expected outcomes are hand-derived literals.

- [ ] **Step 2: Extend full flow test**

Drive registration or metadata acceptance through consent, token exchange, authenticated MCP access, refresh rotation, manager restart, and revocation. Assert exact status/error behavior at each boundary.

- [ ] **Step 3: Run focused matrix**

Run: `cargo test -p plug-core downstream_oauth`

Run: `cargo test -p plug-core oauth_`

Expected: all tests pass.

### Task 4: Full Verification and Truth Docs

**Files:**
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`

**Interfaces:**
- Consumes: verified branch state.
- Produces: accurate `exists off-main` classification until merge.

- [ ] **Step 1: Run release-quality gates**

Run:

```bash
cargo fmt --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Review mutations and security boundaries**

Confirm removing completed-cache lookup breaks retry tests, changing stored redirect creates a second code or differing location, and removing descriptions breaks response tests. Confirm remote consent POST remains forbidden and authorization code replay remains `invalid_grant`.

- [ ] **Step 3: Update project truth**

Classify unmerged work as `exists off-main`. Never call it `done on main` until current `main` contains it.
