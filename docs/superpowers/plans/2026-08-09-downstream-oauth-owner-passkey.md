# Downstream OAuth Owner Passkey Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Plug's public-HTTPS-to-localhost consent bridge with durable, owner-verified, same-origin HTTPS authorization that works across hosted and local MCP clients.

**Architecture:** Extend the issuer-owned downstream OAuth state with persisted authorization transactions and WebAuthn passkey state. Public consent routes remain on the configured HTTPS origin; local operator routes bootstrap and administer owner credentials without exposing the operator token. Every approval verifies a server-stored WebAuthn challenge, then atomically persists challenge consumption, consent completion, and one authorization code.

**Tech Stack:** Rust 1.88, Axum 0.8, `webauthn-rs` 0.5.3 with server-side state serialization, serde JSON, existing atomic owner-only state files, embedded JavaScript, Playwright 1.62.1, Chromium, WebKit.

## Global Constraints

- Follow `docs/superpowers/specs/2026-08-09-downstream-oauth-owner-passkey-design.md` exactly.
- Use test-first red, green, refactor cycles. No production behavior before a failing test.
- No named-client production branches.
- No public route may accept the local operator token as owner proof.
- No public HTTPS page may navigate or submit to `http://127.0.0.1`.
- Exact HTTPS origin and RP ID come only from validated `http.public_base_url`.
- WebAuthn registration and authentication ceremony state stays server-side in the owner-only issuer file.
- Preserve DCR, CIMD, preregistration, PKCE S256, exact redirects, RFC 8707 resource binding, refresh rotation, revocation, and legacy MCP protocol behavior.
- Keep binary below the existing 10 MiB release gate.
- Update current-truth docs only after implementation and verification.

---

## File Map

- `Cargo.toml`: pin WebAuthn dependency.
- `plug-core/Cargo.toml`: enable WebAuthn server dependency.
- `plug-core/src/downstream_oauth/mod.rs`: issuer-state version 3, durable transactions, atomic authorization transitions, owner API delegation.
- `plug-core/src/downstream_oauth/owner.rs`: WebAuthn configuration, owner credentials, enrollment bootstraps, registration/authentication ceremonies, verification.
- `plug-core/src/http/oauth_ui.rs`: consent/enrollment HTML, first-party JavaScript, security headers.
- `plug-core/src/http/mod.rs`: register `oauth_ui` module.
- `plug-core/src/http/server.rs`: public OAuth UI/API routes; remove loopback decision route.
- `plug/src/runtime.rs`: local operator owner routes.
- `plug/src/main.rs`: `plug auth owner` command model.
- `plug/src/commands/auth.rs`: enrollment/list/removal client flow.
- `plug-core/src/doctor.rs`: blocking owner-enrollment diagnostic.
- `install.sh`: post-install enrollment check.
- `package.json`, `package-lock.json`, `playwright.config.mjs`: pinned browser harness.
- `tests/e2e/downstream-oauth.spec.mjs`: process-level OAuth and MCP acceptance.
- `.github/workflows/ci.yml`: browser job.
- `.gitignore`: Playwright artifacts.
- `docs/PROJECT-STATE-SNAPSHOT.md`, `docs/PLAN.md`, release notes, setup/troubleshooting docs: post-verification truth.

---

### Task 1: WebAuthn owner domain and issuer-state migration

**Files:**
- Modify: `Cargo.toml`
- Modify: `plug-core/Cargo.toml`
- Create: `plug-core/src/downstream_oauth/owner.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`

**Interfaces:**
- Produces: `OwnerSecurity::new(public_base_url: &str) -> Result<Self, DownstreamOauthError>`
- Produces: serializable `OwnerCredential`, `OwnerBootstrap`, `OwnerRegistrationCeremony`, `OwnerAuthenticationCeremony`
- Produces: `DownstreamOauthState` version 3 with durable OAuth transactions and owner state
- Consumes later: Tasks 2–5 use owner records and ceremony methods

- [ ] **Step 1: Add failing state-migration tests**

In `plug-core/src/downstream_oauth/mod.rs`, add tests proving version 2 loads into version 3 without losing clients/tokens, expired short-lived records are evicted, and pending consent/code survives manager restart.

```rust
#[tokio::test]
async fn v2_state_migrates_without_losing_grants() {
    let fixture = serde_json::json!({
        "version": 2,
        "clients": {},
        "access_tokens": {},
        "refresh_tokens": {},
        "revoked_client_ids": []
    });
    let path = write_state_fixture(fixture);
    let manager = DownstreamOauthManager::new_with_state_path(test_config(), path).unwrap();
    assert_eq!(manager.persisted_state_version_for_tests().await, 3);
}

#[tokio::test]
async fn pending_authorization_survives_restart() {
    let path = temp_state_path();
    let manager = registered_manager(&path).await;
    let consent = begin_valid_authorization(&manager).await;
    drop(manager);
    let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path).unwrap();
    assert!(restarted.pending_consent_exists_for_tests(&consent.consent_id).await);
}
```

- [ ] **Step 2: Run migration tests; confirm RED**

Run:

```bash
cargo test -p plug-core downstream_oauth::tests::v2_state_migrates_without_losing_grants -- --exact
cargo test -p plug-core downstream_oauth::tests::pending_authorization_survives_restart -- --exact
```

Expected: version assertion or missing helper failure; restart test reports missing consent.

- [ ] **Step 3: Pin WebAuthn dependency**

Add workspace dependency:

```toml
webauthn-rs = { version = "=0.5.3", default-features = false, features = ["danger-allow-state-serialisation", "resident-key-support"] }
```

Add `webauthn-rs.workspace = true` to `plug-core`. Serialization is permitted only because ceremony state is stored server-side in the owner-only issuer file, never in client cookies or hidden form data.

- [ ] **Step 4: Implement owner types and state version 3**

Create `owner.rs` with bounded serializable records:

```rust
use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::{Passkey, PasskeyAuthentication, PasskeyRegistration, Webauthn, WebauthnBuilder};

pub const MAX_OWNER_CREDENTIALS: usize = 5;
pub const MAX_OWNER_CHALLENGES: usize = 10;
pub const OWNER_CEREMONY_LIFETIME_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerCredential {
    pub id: String,
    pub label: String,
    pub passkey: Passkey,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerBootstrap {
    pub secret_hash: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerRegistrationCeremony {
    pub id: String,
    pub bootstrap_hash: String,
    pub state: PasskeyRegistration,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerAuthenticationCeremony {
    pub id: String,
    pub consent_id: String,
    pub state: PasskeyAuthentication,
    pub expires_at: u64,
}

pub struct OwnerSecurity {
    pub webauthn: Webauthn,
    pub rp_id: String,
    pub origin: url::Url,
}
```

`DownstreamOauthManager` stores `Arc<OwnerSecurity>`. `PendingConsent` gains a random `csrf_token`; `ConsentRequest` gains the exact `redirect_uri`, client source/hostname, resource, `csrf_token`, and expiry needed by the UI. Add explicit `DownstreamOauthError` variants: `OwnerNotEnrolled`, `InvalidOwnerBootstrap`, `OwnerChallengeExpired`, `InvalidOwnerAssertion`, `OwnerCredentialLimit`, and `OwnerCredentialNotFound`.

`OwnerSecurity::new` parses `public_base_url`, rejects non-HTTPS origins, paths, credentials, query, fragment, missing host, and IP-host RP IDs, then builds:

```rust
let webauthn = WebauthnBuilder::new(rp_id, &origin)?
    .rp_name("Plug")
    .build()?;
```

Change `STATE_VERSION` to `3`. Remove `#[serde(skip)]` from pending consents, completed consents, and pending codes. Add defaults for owner maps. Implement explicit v2 migration rather than rejecting the old version. Startup calls one bounded expiry sweep; it does not clear live transactions.

- [ ] **Step 5: Persist every authorization start**

Change `begin_authorization` to clone state, insert pending consent, persist, then publish. Persistence failure must not return a consent ID.

- [ ] **Step 6: Run focused tests; confirm GREEN**

```bash
cargo test -p plug-core downstream_oauth -- --test-threads=1
cargo check -p plug-core
```

Expected: all downstream OAuth tests pass; version 2 fixture migrates; restart retains pending consent.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock plug-core/Cargo.toml plug-core/src/downstream_oauth/mod.rs plug-core/src/downstream_oauth/owner.rs
git commit -m "feat(oauth): persist owner and authorization state"
```

---

### Task 2: Enrollment and approval ceremonies

**Files:**
- Modify: `plug-core/src/downstream_oauth/owner.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`

**Interfaces:**
- Produces: `create_owner_bootstrap() -> Result<String, DownstreamOauthError>`
- Produces: `start_owner_registration(bootstrap: &str) -> Result<OwnerRegistrationChallenge, DownstreamOauthError>`
- Produces: `finish_owner_registration(ceremony_id: &str, credential: RegisterPublicKeyCredential) -> Result<OwnerCredentialSummary, DownstreamOauthError>`
- Produces: `start_owner_approval(consent_id: &str) -> Result<OwnerApprovalChallenge, DownstreamOauthError>`
- Produces: `finish_owner_approval(ceremony_id: &str, credential: PublicKeyCredential) -> Result<AuthorizationRedirect, DownstreamOauthError>`
- Produces: `deny_consent(consent_id: &str, csrf_token: &str) -> Result<AuthorizationRedirect, DownstreamOauthError>`

- [ ] **Step 1: Add failing ceremony tests**

Cover bootstrap expiry/replay, registration persistence, wrong RP/origin, missing user verification, challenge replay, consent substitution, restart between challenge and finish, atomic write failure, and approval replay.

```rust
#[tokio::test]
async fn owner_bootstrap_is_single_use() {
    let manager = manager_with_state_path(temp_state_path());
    let bootstrap = manager.create_owner_bootstrap().await.unwrap();
    manager.start_owner_registration(&bootstrap).await.unwrap();
    assert_eq!(
        manager.start_owner_registration(&bootstrap).await.unwrap_err(),
        DownstreamOauthError::InvalidOwnerBootstrap
    );
}

#[tokio::test]
async fn approval_challenge_is_bound_to_original_consent() {
    let manager = enrolled_manager().await;
    let first = begin_valid_authorization(&manager).await;
    let second = begin_valid_authorization(&manager).await;
    let challenge = manager.start_owner_approval(&first.consent_id).await.unwrap();
    let assertion = sign_challenge_with_test_passkey(&challenge).await;
    let redirect = manager
        .finish_owner_approval(&challenge.ceremony_id, assertion)
        .await
        .unwrap();
    assert!(redirect.location.contains("state=first-state"));
    assert!(manager.pending_consent_exists_for_tests(&second.consent_id).await);
}
```

- [ ] **Step 2: Run focused tests; confirm RED**

```bash
cargo test -p plug-core downstream_oauth::tests::owner_bootstrap_is_single_use -- --exact
cargo test -p plug-core downstream_oauth::tests::approval_challenge_is_bound_to_original_consent -- --exact
```

Expected: ceremony APIs missing.

- [ ] **Step 3: Implement registration ceremony**

Use `start_passkey_registration` with one stable issuer-bound owner UUID derived from SHA-256 of the issuer, existing credential IDs as exclusions, username `plug-owner`, display name `Plug owner`. Persist the returned `PasskeyRegistration` before returning the browser challenge. `finish_passkey_registration` consumes ceremony state once, rejects duplicate credential IDs, inserts credential, and persists before returning success.

- [ ] **Step 4: Implement approval ceremony**

Use all active owner passkeys with `start_passkey_authentication`. Store the returned `PasskeyAuthentication` beside the exact `consent_id`. `finish_passkey_authentication` validates the browser result. Require matching credential ID; update the stored passkey using `Passkey::update_credential`; reject a meaningful nonzero counter regression; consume ceremony state.

- [ ] **Step 5: Atomically commit approval**

Under the existing manager mutex, validate ceremony and pending consent from one state snapshot. Clone state, consume ceremony, remove pending consent, insert one pending code, insert completed approval, update owner credential, persist, publish. Never persist an approval challenge separately from its consent transition.

Denial consumes the pending consent using a constant-time CSRF-token comparison and persists a completed `access_denied` result. It creates no owner ceremony or authorization code.

- [ ] **Step 6: Add owner administration methods**

```rust
pub async fn list_owner_credentials(&self) -> Vec<OwnerCredentialSummary>;
pub async fn remove_owner_credential(&self, credential_id: &str) -> Result<bool, DownstreamOauthError>;
pub async fn owner_enrolled(&self) -> bool;
```

Removing a credential also removes ceremonies tied to it. Removing the final credential is allowed only through the authenticated local operator API; the method itself remains explicit and auditable.

- [ ] **Step 7: Run owner and downstream OAuth suites; confirm GREEN**

```bash
cargo test -p plug-core downstream_oauth -- --test-threads=1
```

Expected: ceremony, restart, replay, expiry, and persistence-failure tests pass.

- [ ] **Step 8: Commit**

```bash
git add plug-core/src/downstream_oauth/owner.rs plug-core/src/downstream_oauth/mod.rs
git commit -m "feat(oauth): verify owner approval with passkeys"
```

---

### Task 3: Same-origin public consent and enrollment UI

**Files:**
- Create: `plug-core/src/http/oauth_ui.rs`
- Modify: `plug-core/src/http/mod.rs`
- Modify: `plug-core/src/http/server.rs`

**Interfaces:**
- Consumes: Task 2 ceremony APIs
- Produces: public routes and browser JSON contracts
- Produces: `apply_oauth_html_security_headers(response: &mut Response)`

- [ ] **Step 1: Add failing public-route tests**

Add router tests for exact Origin, absent Origin on GET, wrong Origin on POST, owner missing, enrollment, approval, denial, restart, security headers, error redaction, and absence of localhost actions.

```rust
#[tokio::test]
async fn consent_page_never_references_loopback_approval() {
    let response = oauth_authorize_response(enrolled_test_state()).await;
    let body = response_body(response).await;
    assert!(!body.contains("127.0.0.1"));
    assert!(!body.contains("localhost"));
    assert!(body.contains("/oauth/consent/challenge"));
}

#[tokio::test]
async fn public_approval_requires_exact_origin_and_owner_assertion() {
    let response = post_owner_decision_without_origin_or_assertion().await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run route tests; confirm RED**

```bash
cargo test -p plug-core http::server::tests::consent_page_never_references_loopback_approval -- --exact
cargo test -p plug-core http::server::tests::public_approval_requires_exact_origin_and_owner_assertion -- --exact
```

Expected: old page contains loopback URL; new routes missing.

- [ ] **Step 3: Implement HTML and security headers**

Move consent rendering from `server.rs` into `oauth_ui.rs`. Render client name, verified CIMD domain or dynamic-client warning, exact callback hostname and expandable URI, resource name/URL, literal scopes plus plain descriptions, and expiry.

Apply exactly:

```text
Cache-Control: no-store
Content-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
```

- [ ] **Step 4: Add embedded first-party JavaScript**

Serve `/oauth/assets/consent.js` and `/oauth/assets/enroll.js` from `include_str!` constants with `Content-Type: application/javascript; charset=utf-8` and immutable cache metadata keyed by Plug release version.

Implement base64url conversion without third-party runtime code:

```javascript
function decodeBase64url(value) {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((value.length + 3) % 4);
  return Uint8Array.from(atob(base64), character => character.charCodeAt(0));
}

function encodeBase64url(value) {
  const bytes = new Uint8Array(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
```

Convert creation/request challenge fields to `ArrayBuffer`; convert returned `rawId`, `clientDataJSON`, `attestationObject`, `authenticatorData`, `signature`, and optional `userHandle` to base64url JSON matching `webauthn-rs` protocol types.

- [ ] **Step 5: Add public handlers**

Register:

```text
GET  /oauth/authorize
GET  /oauth/assets/consent.js
POST /oauth/consent/challenge
POST /oauth/consent/decision
GET  /oauth/owner/enroll
GET  /oauth/assets/enroll.js
POST /oauth/owner/enroll/challenge
POST /oauth/owner/enroll/complete
```

POST handlers require exact `Origin == manager.base_url()` and exact Host. They use typed JSON rejections capped at 64 KiB. Enrollment challenge consumes bootstrap secret; approval finish returns JSON `{ "redirect_uri": "..." }`; browser assigns `window.location` only after success.

- [ ] **Step 6: Remove loopback approval route and helpers**

Delete route `POST /_plug/oauth/authorize`, handler `oauth_authorize_decision`, `local_consent_endpoint`, and `local_approval_request_allowed`. Keep loopback validation only for registered OAuth client callback URIs.

- [ ] **Step 7: Run focused HTTP suite; confirm GREEN**

```bash
cargo test -p plug-core http::server::tests::oauth -- --test-threads=1
cargo test -p plug-core downstream_oauth -- --test-threads=1
```

Expected: no consent HTML contains `127.0.0.1`; wrong-origin public POST is forbidden; full in-process OAuth lifecycle passes.

- [ ] **Step 8: Commit**

```bash
git add plug-core/src/http/mod.rs plug-core/src/http/oauth_ui.rs plug-core/src/http/server.rs
git commit -m "feat(oauth): serve owner-verified HTTPS consent"
```

---

### Task 4: Local operator API and `plug auth owner` CLI

**Files:**
- Modify: `plug/src/runtime.rs`
- Modify: `plug/src/main.rs`
- Modify: `plug/src/commands/auth.rs`

**Interfaces:**
- Consumes: Task 2 bootstrap/list/remove APIs
- Produces: operator routes and CLI commands

- [ ] **Step 1: Add failing runtime and CLI tests**

Cover missing/wrong operator token, forwarded headers, non-loopback Host, successful bootstrap, list, remove confirmation, JSON output, and browser-open failure fallback.

```rust
#[tokio::test]
async fn owner_bootstrap_rejects_public_forwarded_request() {
    let response = operator_router_request(
        Method::POST,
        "/_plug/oauth/owner/bootstrap",
        [("x-plug-operator-token", test_token()), ("cf-connecting-ip", "203.0.113.4")],
    ).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run tests; confirm RED**

```bash
cargo test -p plug-mcp owner_bootstrap_rejects_public_forwarded_request -- --exact
cargo test -p plug-mcp auth_owner -- --test-threads=1
```

Expected: routes and command variants missing.

- [ ] **Step 3: Implement local operator routes**

Add:

```text
POST   /_plug/oauth/owner/bootstrap
GET    /_plug/oauth/owner/credentials
DELETE /_plug/oauth/owner/credentials/{credential_id}
```

Require constant-time operator-token verification, exact loopback Host, and no `Forwarded`, `X-Forwarded-For`, or `CF-Connecting-IP`. Bootstrap returns `{ "enrollment_url": "https://.../oauth/owner/enroll#bootstrap=..." }`. Never log or include bootstrap secret in tracing fields.

- [ ] **Step 4: Add CLI command model**

```rust
#[derive(Subcommand)]
pub(crate) enum OwnerCommands {
    Enroll { #[arg(long)] no_browser: bool },
    List,
    Remove { credential_id: String, #[arg(long)] yes: bool },
}
```

Add `AuthCommands::Owner { command: OwnerCommands }` and dispatcher handling.

- [ ] **Step 5: Implement CLI operations**

Reuse the existing local operator HTTP client construction from downstream client administration by extracting one helper. `enroll` prints the URL when `--no-browser`; otherwise opens it with the existing browser-opening dependency and still prints the URL on failure. `list --output json` never serializes passkey public keys or ceremony state. `remove` requires confirmation unless `--yes`.

- [ ] **Step 6: Run CLI/runtime tests; confirm GREEN**

```bash
cargo test -p plug-mcp auth_owner -- --test-threads=1
cargo test -p plug-mcp runtime::tests::operator -- --test-threads=1
```

- [ ] **Step 7: Commit**

```bash
git add plug/src/runtime.rs plug/src/main.rs plug/src/commands/auth.rs
git commit -m "feat(oauth): add owner passkey administration"
```

---

### Task 5: Setup, doctor, migration, and user-readable failure UX

**Files:**
- Modify: `plug-core/src/doctor.rs`
- Modify: `plug/src/views/overview.rs`
- Modify: `install.sh`
- Modify: downstream OAuth error mappings in `plug-core/src/downstream_oauth/mod.rs` and `plug-core/src/http/server.rs`

**Interfaces:**
- Consumes: `owner_enrolled()` and CLI enrollment
- Produces: actionable setup status and post-install enrollment

- [ ] **Step 1: Add failing doctor/error tests**

```rust
#[test]
fn oauth_without_owner_credential_is_blocking() {
    let report = doctor_report(oauth_config(), empty_owner_state());
    assert!(report.errors.iter().any(|message| {
        message == "Downstream OAuth owner credential missing. Run `plug auth owner enroll`."
    }));
}
```

Also test corrupt owner state, unsupported browser, expired consent, stale assertion retry, and persistence-error redaction.

- [ ] **Step 2: Run tests; confirm RED**

```bash
cargo test -p plug-core doctor::tests::oauth_without_owner_credential_is_blocking -- --exact
```

- [ ] **Step 3: Add doctor and overview state**

`plug doctor` reports missing credential as a blocking setup error only when downstream OAuth is enabled. Overview reports `Owner approval: enrolled (N credentials)` or `Owner approval: setup required`; it never reads or prints credential material.

- [ ] **Step 4: Add safe browser error mapping**

Map owner failures to stable public messages:

```text
OwnerNotEnrolled: "Finish Plug owner setup on the Mac running Plug."
OwnerChallengeExpired: "Approval expired. Select Allow again."
InvalidOwnerAssertion: "Passkey verification failed. No access was granted."
Persistence(_): "Plug could not save this authorization. No access was granted."
```

Return machine-readable codes to first-party JavaScript while retaining OAuth redirects only when client and redirect were already validated.

- [ ] **Step 5: Update installer**

After signed binary installation and daemon health verification, run `plug auth owner list --output json`. When OAuth is enabled and list is empty, run `plug auth owner enroll`. Do not rotate, remove, or silently replace existing credentials. Failure prints exact recovery command and exits nonzero only when OAuth was requested by current config.

- [ ] **Step 6: Run focused tests and shell checks; confirm GREEN**

```bash
cargo test -p plug-core doctor -- --test-threads=1
cargo test -p plug-mcp auth_owner -- --test-threads=1
bash -n install.sh
```

- [ ] **Step 7: Commit**

```bash
git add plug-core/src/doctor.rs plug-core/src/downstream_oauth/mod.rs plug-core/src/http/server.rs plug/src/views/overview.rs install.sh
git commit -m "fix(oauth): guide owner enrollment and recovery"
```

---

### Task 6: Real-browser, real-process OAuth acceptance

**Files:**
- Create: `package.json`
- Create: `package-lock.json`
- Create: `playwright.config.mjs`
- Create: `tests/e2e/downstream-oauth.spec.mjs`
- Modify: `.gitignore`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Tasks 1–5 complete product flow
- Produces: `npm run test:e2e`

- [ ] **Step 1: Create failing browser specification**

Pin:

```json
{
  "private": true,
  "scripts": { "test:e2e": "playwright test" },
  "devDependencies": { "@playwright/test": "1.62.1" }
}
```

Configure Chromium and WebKit, one worker, 120-second timeout, trace on failure. Build a fixture that allocates a free loopback port, creates isolated HOME/XDG directories, writes OAuth config, spawns real `target/debug/plug serve`, and stops the child in `afterEach`.

- [ ] **Step 2: Run Playwright; confirm RED**

```bash
npm ci
npx playwright install chromium webkit
cargo build -p plug-mcp -p plug-test-harness --bins
npm run test:e2e
```

Expected: owner enrollment/consent flow fails until browser JSON contracts and runtime behavior are complete; any unexpected pass means the test is not exercising passkey authorization.

- [ ] **Step 3: Implement Chromium full ceremony**

Use Chromium DevTools Protocol `WebAuthn.enable` and `WebAuthn.addVirtualAuthenticator` with resident-key and user-verification support. Test:

1. Local operator bootstrap
2. Public HTTPS enrollment
3. DCR and PKCE S256 authorization
4. Consent details
5. Daemon stop/restart with page open
6. Allow with virtual passkey
7. Hosted callback capture
8. Token exchange and authorization-code replay `invalid_grant`
9. Bearer `initialize`, `notifications/initialized`, `tools/list`
10. Refresh rotation/replay
11. Client revocation and HTTP 401 challenge

Proxy `https://plug.test` to the isolated Plug listener inside Playwright without changing browser-visible origin. Capture `https://client.test/callback` using route fulfillment.

- [ ] **Step 4: Implement Chromium/WebKit shared cases**

Both engines cover consent rendering, security headers, denial, wrong-origin API rejection, expiry, missing-owner page, restart survival, JavaScript-disabled failure, and absence of localhost navigation. WebKit full platform-passkey ceremony remains manual when its automation API cannot install a virtual authenticator.

- [ ] **Step 5: Add CI browser job**

After `check`:

```yaml
browser-oauth:
  name: Browser OAuth
  needs: check
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - uses: actions/setup-node@v4
      with:
        node-version: "24"
        cache: npm
    - run: cargo build -p plug-mcp -p plug-test-harness --bins
    - run: npm ci
    - run: npx playwright install --with-deps chromium webkit
    - run: npm run test:e2e
```

- [ ] **Step 6: Run browser suite; confirm GREEN**

```bash
npm run test:e2e
```

Expected: Chromium full journey passes; Chromium and WebKit shared cases pass; no child process survives.

- [ ] **Step 7: Commit**

```bash
git add package.json package-lock.json playwright.config.mjs tests/e2e/downstream-oauth.spec.mjs .gitignore .github/workflows/ci.yml
git commit -m "test(oauth): prove passkey consent in real browsers"
```

---

### Task 7: Security review, full verification, and release truth

**Files:**
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`
- Modify: `docs/OPERATOR-GUIDE.md`
- Modify: `docs/CRATE-STACK.md`
- Create: `docs/RELEASE-NOTES-2026-08-09-OAUTH-OWNER-PASSKEY.md`

**Interfaces:**
- Consumes: all implementation and test tasks
- Produces: reviewed release candidate and honest documentation

- [ ] **Step 1: Run adversarial review before docs**

Review exact diff for: unauthenticated approval, operator-token exposure, bootstrap logging, CSRF, wrong-origin acceptance, RP-ID confusion, ceremony replay, consent substitution, write-before-publish violations, path traversal, state migration loss, unbounded maps, code/token leakage, client-specific branching, and binary-size regression. Resolve every P0/P1/P2 finding with a new failing test before code.

- [ ] **Step 2: Run full fresh verification**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
cargo check --workspace
cargo +1.88.0 check --workspace
cargo deny check licenses bans sources advisories
npm ci
npm run test:e2e
cargo build --release -p plug-mcp
test "$(stat -f%z target/release/plug)" -le 10485760
git diff --check
```

On Linux, use `stat --format=%s target/release/plug` for the size gate.

- [ ] **Step 3: Update truth docs**

Only after Step 2 passes, mark public HTTPS owner-passkey consent and durable authorization transactions as `exists off-main`. Keep live vendor certification pending until each named client passes. Record WebKit automated ceremony limitation accurately.

- [ ] **Step 4: Commit release truth**

```bash
git add docs/PROJECT-STATE-SNAPSHOT.md docs/PLAN.md docs/OPERATOR-GUIDE.md docs/CRATE-STACK.md docs/RELEASE-NOTES-2026-08-09-OAUTH-OWNER-PASSKEY.md
git commit -m "docs(oauth): record owner-verified consent release"
```

---

### Task 8: Publish, merge, install, and live client certification

**Files:**
- No source changes unless a live failure first receives a regression test

**Interfaces:**
- Consumes: verified branch
- Produces: merged `main`, signed local installation, live certification record

- [ ] **Step 1: Push branch and open PR**

```bash
git push -u origin codex/oauth-owner-passkey
gh pr create --base main --head codex/oauth-owner-passkey --title "feat(oauth): add owner-verified HTTPS consent" --body-file /tmp/plug-oauth-owner-passkey-pr.md
```

PR body must summarize user journey, trust boundaries, migration, tests, WebKit limitation, and manual certification gates in normal prose.

- [ ] **Step 2: Wait for exact-head CI and review**

Require every CI job green on current head. Resolve review feedback with TDD. Re-run full local verification after any head change.

- [ ] **Step 3: Merge and verify main**

Merge only after exact-head gates. Pull `main`; verify merged commit contains code and truth docs. Create a post-merge truth commit changing the feature from `exists off-main` to `done on main`, then push `main` and re-run the exact-head documentation checks.

- [ ] **Step 4: Install signed build**

Use repository's supported signed installation path. Verify signature, installed binary hash/version, daemon executable path, daemon restart, owner enrollment, and 12 configured upstreams. Do not treat `plug status` alone as client proof.

- [ ] **Step 5: Live client matrix**

For each current stable client, create a separate temporary entry named `Plug OAuth RC`; do not delete or overwrite the user's existing connector:

```text
Claude: Connect, passkey Allow, hosted callback, tools/list, tool call
ChatGPT: Connect, passkey Allow, hosted callback, tools/list, tool call
Codex: Authenticate, passkey Allow, loopback callback, tools/list, tool call
Cursor: Connect, passkey Allow, callback, tools/list, tool call
OpenCode: first protected use, passkey Allow, callback, tools/list, tool call
```

Also verify daemon restart with consent open, denial, expiration, retry, refresh, revocation, and reauthorization. Capture exact client version and result. Never mark an untested or failed client certified.

- [ ] **Step 6: Cleanup**

Remove temporary browser artifacts and stopped test processes. Delete merged remote branch if repository convention permits. Preserve logs needed for failed certification. Confirm clean `main` and healthy installed runtime.
