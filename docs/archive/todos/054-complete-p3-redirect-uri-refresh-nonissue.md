---
status: complete
priority: p3
issue_id: "054"
tags: [code-review, oauth, correctness]
dependencies: []
---

# Redirect URI alignment on token refresh is a non-issue

## Problem Statement

`refresh_access_token()` in oauth.rs hardcodes `"http://localhost:0/callback"` as the redirect URI
when configuring the OAuth client for a refresh_token exchange. If the original authorization was
performed with a different redirect URI (e.g. a specific port from the localhost listener, or a
different path), some OAuth providers will reject the refresh request with `invalid_grant` because
the redirect URI must match the one used during the original authorization.

In practice, RFC 6749 Section 6 does not require `redirect_uri` on refresh requests unless it was
included in the original authorization request. Whether this is a real problem depends on provider
behavior and whether rmcp's `refresh_token()` actually sends the redirect URI.

## Findings

- Identified by code-simplicity-reviewer during PR #42 CE review
- The original `plug auth login` flow uses `localhost:0` (OS-assigned port) — the actual port
  differs each time
- rmcp's `OAuthClientConfig.redirect_uri` is set but may not be sent during refresh_token exchange
- Needs verification: does rmcp send redirect_uri on refresh requests?

## Resolution

Verified on current `main`: rmcp's `AuthorizationManager::refresh_token()` does not send
`redirect_uri` on refresh. It builds the refresh request via
`oauth_client.exchange_refresh_token(...).request_async(...)` with no redirect URI attached.

That means the hardcoded `redirect_uri` in `plug-core/src/oauth.rs` is not part of the refresh
request and does not create an active correctness bug on current `main`. No runtime fix is needed.

## Acceptance Criteria

- [x] Verified whether rmcp sends redirect_uri on refresh requests
- [x] If not sent: documented and todo closed

## Work Log

- 2026-03-09: Identified during PR #42 CE review (code-simplicity-reviewer)
- 2026-03-10: Verified that rmcp's `refresh_token()` path does not send `redirect_uri`; closed as a non-issue with no runtime change required.

## Resources

- PR #42
- `plug-core/src/oauth.rs` — `refresh_access_token()` line with `redirect_uri`
- `rmcp` 1.0.0: `src/transport/auth.rs` — `AuthorizationManager::refresh_token()` uses `exchange_refresh_token(...).request_async(...)` without `redirect_uri`
- RFC 6749 Section 6 (Refreshing an Access Token)
