---
status: pending
priority: p3
issue_id: "054"
tags: [code-review, oauth, correctness]
dependencies: []
---

# Redirect URI alignment on token refresh

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

## Proposed Solutions

### Option A: Verify rmcp behavior and document (Recommended first step)

Check whether rmcp's `refresh_token()` sends `redirect_uri` in the token request. If it does not,
this is a non-issue and can be closed. If it does, store the original redirect URI in
`StoredCredentials` and replay it during refresh.

**Pros:** May close with no code change
**Cons:** Requires reading rmcp source
**Effort:** Small (investigation) / Medium (if fix needed)
**Risk:** Low

### Option B: Store and replay original redirect URI

Persist the redirect URI used during initial authorization in the credential store. Use it during
refresh.

**Pros:** Correct by construction
**Cons:** Schema change to stored credentials, migration needed
**Effort:** Medium
**Risk:** Low

## Acceptance Criteria

- [ ] Verified whether rmcp sends redirect_uri on refresh requests
- [ ] If sent: original redirect URI preserved and replayed during refresh
- [ ] If not sent: documented and todo closed

## Work Log

- 2026-03-09: Identified during PR #42 CE review (code-simplicity-reviewer)

## Resources

- PR #42
- `plug-core/src/oauth.rs` — `refresh_access_token()` line with `redirect_uri`
- RFC 6749 Section 6 (Refreshing an Access Token)
