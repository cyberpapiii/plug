# Downstream OAuth Owner Passkey Design

**Status:** Approved design

**Date:** 2026-08-09

**Goal:** Make downstream MCP OAuth authorization a durable, standards-based public HTTPS flow that works with hosted and local MCP clients while preserving Plug's local-owner trust boundary.

## Problem

Plug implements OAuth discovery, Dynamic Client Registration, Client ID Metadata Documents, PKCE S256, exact redirect validation, resource-bound tokens, refresh rotation, and client revocation. The remaining authorization journey is not portable: the public HTTPS consent page submits its decision to `http://127.0.0.1:<port>`. That address names the browser's device, not necessarily the Mac running Plug. Browser handling also differs across engines.

The loopback POST is currently Plug's only resource-owner check. Replacing it with an unauthenticated public POST would allow anyone who can start an authorization request to approve that request. The replacement therefore needs both public HTTPS transport and explicit owner verification.

Pending consent, completed decisions, and authorization codes are also memory-only. A daemon restart during authorization invalidates the journey even though clients, access tokens, and refresh tokens survive.

## Product Contract

Normal authorization after one-time owner enrollment is:

1. User selects Connect in any MCP client.
2. Client performs MCP and OAuth metadata discovery.
3. Client opens Plug's public HTTPS authorization page.
4. Page shows the client identity, client domain, exact redirect destination, Plug resource, requested permissions, and five-minute expiry.
5. User selects Allow.
6. Browser performs a passkey assertion with user verification.
7. Plug redirects to the client's exact registered callback.
8. Client exchanges the code with PKCE and resumes MCP automatically.

No client-specific branches, copied codes, manually supplied client secrets, localhost approval page, second Connect action, or Plug restart are part of the normal journey.

Connector installation remains client-owned. Plug cannot make every client's server-addition UI identical. Plug guarantees one Connect and one Allow after the server URL has been installed or selected.

## Trust Model

Four identities remain separate:

- **OAuth client:** Established by preregistration, CIMD, or DCR. It owns redirect URIs, PKCE material, grants, and tokens.
- **Plug owner:** Established by a WebAuthn credential enrolled through Plug's local operator control plane. Only this identity can approve a grant.
- **Tunnel or reverse proxy:** Delivers HTTPS traffic. Headers, source IP, and tunnel reachability never prove owner identity.
- **MCP principal:** Issuer, client ID, resource, scopes, and lifecycle generation attached to authenticated MCP work.

The local OS account remains Plug's recovery trust root. A user with local operator access may enroll, list, or remove owner credentials. The browser never receives Plug's reusable operator token.

## Owner Enrollment

Add local commands:

```text
plug auth owner enroll
plug auth owner list
plug auth owner remove <credential-id>
```

Enrollment requires the existing local operator credential and loopback control route. It creates a random 256-bit, single-use bootstrap grant with a five-minute lifetime, then opens:

```text
https://<public-origin>/oauth/owner/enroll#bootstrap=<secret>
```

The secret stays in the URL fragment and is never sent in an HTTP request, referrer, or server log. First-party enrollment JavaScript reads the fragment, clears it from browser history, exchanges it for a WebAuthn creation challenge, and calls `navigator.credentials.create()`.

Plug stores only public credential material, credential metadata, and signature-counter state. Private keys remain in the platform passkey provider. Enrollment requires user verification. Maximum five active owner credentials per issuer.

Removing the final credential requires explicit local confirmation. OAuth authorization then fails closed until another credential is enrolled. Existing client grants remain revocable but no new grant can be approved.

## Authorization Flow

`GET /oauth/authorize` keeps existing OAuth validation. After client, redirect URI, resource, scope, state, and PKCE validation, Plug atomically persists an immutable pending transaction and renders the HTTPS consent page.

The consent page contains an opaque transaction ID only. Security-relevant fields are always reloaded from Plug's persisted transaction; hidden form values never become authority.

Selecting Allow performs:

1. `POST /oauth/consent/challenge` with the transaction ID.
2. Plug creates and persists a single-use WebAuthn challenge bound to the issuer, transaction ID, client ID, exact redirect URI, resource, scopes, PKCE challenge, approval decision, and expiry.
3. Browser calls `navigator.credentials.get()` with `userVerification: "required"`.
4. `POST /oauth/consent/decision` submits the assertion.
5. Plug validates exact HTTPS origin, RP ID, challenge, credential ID, user-presence flag, user-verification flag, signature, expiry, and replay state.
6. Plug atomically consumes the owner challenge, records approval, creates one authorization code, and persists the complete transition before responding.
7. Browser navigates to the exact registered callback.

Selecting Deny requires the same-origin transaction CSRF token but not a passkey assertion because denial cannot create a grant. Denial is persisted, single-use, and redirects with `error=access_denied` plus the original state.

Repeated submission returns the first completed result and never creates a second code. Conflicting later decisions return the original result. Expired, unknown, or revoked transactions return a stable, user-readable page and never expose internal persistence errors.

## WebAuthn Rules

- RP ID is the exact hostname from `http.public_base_url`.
- Expected origin is the exact HTTPS origin from `http.public_base_url`.
- User presence and user verification are required.
- Attestation preference is `none`; Plug does not build a hardware-vendor trust list.
- Resident/discoverable credentials are preferred for passkey UX but not required when the platform cannot provide them.
- Challenges use at least 256 bits of randomness and expire after five minutes.
- A challenge is bound to one transaction and the literal `approve` decision.
- Credential signature counters are checked when meaningful. A lower nonzero counter fails closed and is surfaced as a credential-security error.
- Credential changes, corruption, or verification failures never fall back to unauthenticated approval.

## Persistence

Advance issuer state from version 2 to version 3. Persist, with existing owner-only permissions and atomic write pattern:

- Registered OAuth clients
- Pending consents
- Completed consent decisions
- Pending authorization codes
- Access tokens
- Refresh tokens
- Revoked client IDs
- Owner public credentials
- Owner-enrollment bootstraps
- Owner assertion challenges

Every short-lived record has an explicit creation time and expiry. Startup evicts expired records instead of clearing all authorization state. Limits remain bounded: 200 pending or completed consents globally, five per OAuth client, one active enrollment bootstrap, ten active owner challenges, and five owner credentials.

State-changing paths retain clone, validate, persist, publish ordering. Approval, authorization-code creation, and owner-challenge consumption occur in one persisted issuer-state transition. Authorization-code exchange removes the code and publishes tokens in one persisted transition. Persistence failure returns an error before any redirect, code, token, or replay result becomes visible.

## HTTP Surface

Public routes:

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

Local operator routes:

```text
POST   /_plug/oauth/owner/bootstrap
GET    /_plug/oauth/owner/credentials
DELETE /_plug/oauth/owner/credentials/:credential-id
```

Public decision and enrollment routes require exact Host and Origin matching the configured public origin where a browser sends Origin. They never accept the operator token as public authorization. Local routes require the existing operator token, loopback Host, and no forwarding headers.

Consent and enrollment responses set:

```text
Cache-Control: no-store
Content-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
```

JavaScript is served as immutable first-party static content embedded in the Plug binary. No third-party scripts, fonts, images, analytics, or network calls are allowed.

The current `POST /_plug/oauth/authorize` loopback approval route is removed after migration. No public HTTPS page submits to HTTP localhost.

## Configuration and Migration

OAuth mode requires a stable HTTPS `public_base_url`. Owner passkey verification is mandatory for public consent; no insecure configuration switch disables it.

On upgrade, existing OAuth client registrations and grants migrate to issuer state version 3. No owner credential is invented. `plug doctor` reports a blocking setup action until enrollment completes:

```text
Downstream OAuth owner credential missing. Run `plug auth owner enroll`.
```

The signed local installer runs the owner-enrollment check after starting the daemon and opens enrollment when needed. It never rotates or removes existing owner credentials automatically.

## Consent UX

Consent page uses ordinary language and one primary action:

- Heading: `Allow <client> to use Plug?`
- Identity: client name plus verified client-ID hostname or `Unverified dynamically registered client`
- Destination: exact redirect hostname with expandable full URI
- Resource: human-readable Plug resource name and canonical MCP URL
- Permissions: requested scopes translated into concrete capabilities while retaining literal scope values
- Expiry: `This request expires in 5 minutes.`
- Primary action: `Allow with Touch ID or passkey`
- Secondary action: `Deny`

Localhost client callbacks receive the MCP-required warning and visibly show the callback hostname and port. Metadata fetch failures never receive trusted branding.

If owner enrollment is missing, the page explains the local command needed and does not display an Allow button. If a WebAuthn challenge expires while the user is present, the page obtains one fresh challenge automatically. It never restarts the OAuth transaction or asks the MCP client to Connect again unless the five-minute authorization transaction itself expired.

## Compatibility

Retain capability-based behavior, never named-client branches:

1. Preregistered client credentials when configured by the client platform
2. CIMD when advertised and valid
3. RFC 7591 DCR fallback
4. Manual client credentials as last-resort operator setup

Retain RFC 9728 protected-resource metadata, RFC 8414 and OIDC discovery compatibility, `resource` on authorization and token requests, PKCE S256, exact redirects, authorization-code single use, public-client refresh rotation, token audience checks, and bearer challenges.

Hosted Claude and ChatGPT callbacks remain public HTTPS. Local native callbacks may use registered loopback HTTP with PKCE; that exception applies only to the client's callback, never Plug's consent transport.

## Error Handling

- Invalid client or untrusted metadata: local HTTPS error page; never redirect to an unvalidated URI.
- Valid client with safe redirect and denied/expired request: OAuth error redirect with original state.
- Missing owner credential: actionable setup page; no grant.
- WebAuthn unavailable: actionable browser/platform message; no insecure fallback.
- Stale WebAuthn challenge: one transparent challenge retry.
- Restart: persisted transaction and challenge continue until expiry.
- Persistence failure: fail closed; redact filesystem detail from browser.
- Authorization-code replay: `invalid_grant`.
- Refresh-token replay: `invalid_grant` and existing rotation policy.
- Revoked client token: HTTP 401 with protected-resource challenge.

## Testing

All production behavior follows test-first red, green, refactor cycles.

Rust unit and HTTP tests cover:

- State version 2 to 3 migration
- Pending consent, completion, code, bootstrap, and challenge restart survival
- Expiry and bounded eviction
- Atomic write failure before publication
- Exact origin and RP-ID validation
- Missing user presence or verification
- Wrong credential, signature, challenge, transaction, decision, resource, redirect, scope, or PKCE binding
- Challenge replay and approval replay
- Conflicting decisions
- Signature-counter regression
- Enrollment bootstrap expiry and replay
- Final-credential removal
- Denial without credential issuance
- DCR, CIMD, and preregistered client lifecycle parity
- Token and refresh replay
- Revocation and MCP principal cleanup

Process-level Playwright tests use the real Plug binary and mock MCP upstream. Chromium uses a virtual WebAuthn authenticator for the full enrollment, Connect, Allow, callback, token, `initialize`, and `tools/list` journey. Chromium and WebKit both cover consent rendering, denial, public-origin routing, restart survival, expiration, and error UX. WebKit's full platform-passkey ceremony remains a signed-build manual gate if CI lacks a virtual authenticator API.

CI adds a dedicated browser job with pinned Playwright and browser versions, serial execution, isolated HOME and XDG paths, captured traces on failure, and guaranteed child-process cleanup.

## Live Release Gate

After automated tests, install a signed build from the exact release commit. Verify:

- Owner enrollment and Touch ID in the default macOS browser
- Fresh Claude connector: one Connect, one Allow, automatic callback, tools available
- ChatGPT custom MCP hosted callback
- Codex local callback
- Cursor current stable build
- OpenCode current stable build
- Daemon restart while consent page is open
- Denial, expiry, retry, token refresh, revocation, and reauthorization
- No request to `http://127.0.0.1` from public consent pages

Cursor and OpenCode cannot be certified from documentation alone. A failed live client remains explicitly uncertified; automated protocol-class coverage does not replace the vendor-client gate.

## Documentation and Release Truth

Update `docs/PROJECT-STATE-SNAPSHOT.md`, `docs/PLAN.md`, release notes, connector setup documentation, and troubleshooting only after merged code exists on `main`. Claims use repository truth labels. `Any client` means the protocol capability matrix passes; named clients are listed only when their current stable builds pass live certification.

## Non-Goals

- Building a Plug cloud account service
- Trusting Cloudflare headers, source IP, consent-ID possession, or operator-token exposure as owner authentication
- Adding client-specific production branches
- Replacing OAuth with a custom device protocol
- Reworking upstream-provider OAuth
- Expanding method-family scope semantics in the legacy MCP era; that separately documented policy remains outside this authorization-journey change
