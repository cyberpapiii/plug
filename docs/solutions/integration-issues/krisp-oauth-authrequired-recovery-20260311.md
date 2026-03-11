---
title: "Krisp OAuth AuthRequired Recovery Gap"
category: integration-issues
tags: [oauth, krisp, authrequired, runtime, recovery, pkce]
module: plug-core/oauth
symptom: "`plug auth login --server krisp` succeeds and `plug auth status` shows authenticated, but `plug status` still reports `krisp` as `Auth Required` with 0 tools."
root_cause: "The runtime startup path resolves OAuth headers from `current_access_token()` cache only. Persisted credentials on disk are not enough if the in-memory cache is empty, so the daemon can remain stuck in `AuthRequired` after a successful login."
date: 2026-03-11
branch: fix/claude-remote-protocol-version
---

# Krisp OAuth AuthRequired Recovery Gap

## Problem

Krisp was configured as an upstream HTTP MCP server with OAuth:

```toml
[servers.krisp]
transport = "http"
url = "https://mcp.krisp.ai/mcp"
auth = "oauth"
```

The user completed `plug auth login --server krisp` successfully in a normal browser session. The resulting stored credentials were valid:

- `plug auth status` reported `krisp (authenticated)`
- `~/Library/Application Support/plug/tokens/krisp.json` contained access token, refresh token, granted scopes, and expiry

But the runtime still reported:

- `plug status` → `krisp  Auth Required  0 tools`

## Solution / Findings

There were two distinct issues.

### 1. Wrong configured scopes for Krisp

The local config originally requested:

```toml
oauth_scopes = ["mcp:read", "mcp:write"]
```

Krisp's published OAuth metadata does not support those scopes. Its protected-resource metadata advertises:

- `user::me::read`
- `user::meetings:metadata::read`
- `user::meetings:notes::read`
- `user::meetings:transcripts::read`
- `user::meetings::list`
- `user::activities::list`

Updating the config to use Krisp-supported scopes fixed the OAuth authorization request itself.

### 2. Successful login does not reliably recover runtime from `AuthRequired`

Even after valid credentials existed on disk, the runtime stayed in `AuthRequired`.

The critical code path is in `plug-core/src/server/mod.rs`:

- HTTP/SSE upstream startup resolves OAuth auth headers from `crate::oauth::current_access_token(name)`
- if that returns `None`, startup immediately marks the server `AuthRequired`

`current_access_token()` reads the in-memory cache, not the persisted credential file directly. That means:

- `plug auth status` can say authenticated because it loads persisted credentials
- the runtime can still say `AuthRequired` if its cache was never hydrated from those credentials

This matches the still-open plan gap in `docs/plans/2026-03-09-feat-oauth-upstream-auth-plan.md`:

- `Re-login after AuthRequired recovers server to Healthy and spawns new refresh loop`

## Why `--no-browser` Shows `localhost:0`

`plug auth login --server krisp --no-browser` intentionally uses a placeholder redirect URI:

- `http://localhost:0/callback`

That mode is manual/headless. The browser is expected to fail to load the redirect URL, while the user copies `code` and `state` from the address bar back into the terminal. That behavior is expected and not specific to Krisp.

## Key Decisions and Rationale

### 1. Use provider-published scopes, not guessed MCP scopes

Remote MCP servers may expose OAuth resources unrelated to generic `mcp:*` scopes. Krisp publishes concrete `user::*` scopes. The local config should match provider metadata, not a guessed convention.

### 2. Separate "auth success" from "runtime recovery"

A valid token file is proof that OAuth completed successfully. If the runtime remains `AuthRequired`, treat that as a runtime recovery problem, not an authentication problem.

### 3. Manual `--no-browser` mode is working as designed

The `localhost:0` redirect is a UX quirk, not a failure in the OAuth exchange logic.

## Gotchas and Lessons Learned

### `plug auth status` and `plug status` can disagree

This is not theoretical. `plug auth status` loads persisted credentials, while the runtime startup path depends on cached tokens for auth header resolution.

### Successful OAuth does not guarantee tools become routable

For OAuth servers, there is still a recovery seam between:

1. storing credentials
2. reconnecting the runtime
3. rehydrating auth state for actual upstream routing

### Stale `krisp_state_*.json` files indicate aborted flows, not success

Those files only contain PKCE verifier / CSRF state. They are evidence that login was started, not that any token was stored.

## Recommended Follow-Up

The durable fix should make runtime startup recover from persisted credentials, not just in-memory cache. Possible approaches:

- hydrate the OAuth cache from persisted credentials before first upstream connect
- or bypass the cache-only lookup on first startup and load stored credentials directly
- or explicitly notify / reconnect the daemon after `plug auth login` succeeds

## Evidence

- `plug auth status` showed Krisp authenticated after browser login
- `plug status` still showed Krisp `Auth Required`
- `krisp.json` existed with valid token payload and granted scopes
- `plug-core/src/server/mod.rs` startup path gates OAuth startup on `current_access_token(name)`
