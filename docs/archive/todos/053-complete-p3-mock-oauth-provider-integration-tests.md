---
status: complete
priority: p3
issue_id: "053"
tags: [testing, oauth, integration]
dependencies: []
---

# Mock OAuth provider integration tests

## Problem Statement

The OAuth token refresh and login flows are tested with unit tests that exercise individual
functions (`refresh_access_token` with unreachable servers, credential store round-trips), but there
are no integration tests that stand up a mock OAuth authorization server and exercise the full
end-to-end flow: PKCE challenge → authorization code → token exchange → token refresh → credential
storage → transport reconnect.

Without this, regressions in the interaction between rmcp's `AuthorizationManager`, the
`CompositeCredentialStore`, and the engine's refresh loop can only be caught manually.

## Findings

- Tracked in `docs/PLAN.md` under "OAuth follow-up polish" since PR #36
- No mock OAuth server infrastructure exists in `plug-test-harness/` or elsewhere
- rmcp's `AuthorizationManager` expects a real HTTP endpoint for metadata discovery

## Proposed Solutions

### Option A: In-process mock OAuth server (Recommended)

Use axum to spin up a minimal OAuth 2.1 server in-process during tests. Implement the minimum
endpoints: `/.well-known/oauth-authorization-server`, `/authorize`, `/token`. Verify the full
refresh flow end-to-end.

**Pros:** Fast, deterministic, no external dependencies
**Cons:** Must keep mock aligned with OAuth 2.1 spec surface
**Effort:** Medium
**Risk:** Low

### Option B: Record/replay with a real OAuth provider

Use a record/replay HTTP library to capture real OAuth interactions and replay them in tests.

**Pros:** Tests against real protocol behavior
**Cons:** Brittle to provider changes, harder to set up
**Effort:** Medium
**Risk:** Medium

## Acceptance Criteria

- [x] Integration test exercises: metadata discovery → token exchange → credential storage
- [x] Integration test exercises: token refresh → cache reload → transport reconnect
- [x] Tests run in CI without external dependencies

## Work Log

- 2026-03-09: Formalized as tracked todo (previously only in PLAN.md)
- 2026-03-10: Blocker removed — PR #47 established HTTP OAuth integration test pattern in `integration_tests.rs`; mock OAuth provider tests are now unblocked
- 2026-03-10: PR #51 added mock OAuth provider integration coverage on `main` for metadata discovery, auth-code exchange persistence with state cleanup, token refresh persistence, and reconnect using refreshed credentials.

## Resources

- `docs/PLAN.md` — "mock OAuth provider integration tests"
- `plug-core/src/oauth.rs` — `refresh_access_token()`, `CompositeCredentialStore`
- `plug-core/src/engine.rs` — `run_refresh_loop`
