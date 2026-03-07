---
title: "Downstream HTTP Bearer Token Authentication"
category: integration-issues
tags: [axum, auth, middleware, bearer-token, http, security, constant-time]
module: plug-core/http
symptom: "No authentication on downstream HTTP server — anyone who can reach the port can create MCP sessions"
root_cause: "HTTP server designed for localhost-only use, then extended to non-loopback bind addresses without adding auth"
date: 2026-03-07
pr: "#25"
branch: feat/pre-phase-http-auth
---

# Downstream HTTP Bearer Token Authentication

## Problem

plug's downstream HTTP server (`plug serve`) had no authentication. The only protection was origin validation middleware that rejected non-localhost origins, but this is insufficient for remote access (phone clients, remote AI assistants). Anyone who could reach the port could create MCP sessions and invoke any tool.

## Solution

Added bearer token authentication as an axum middleware layer, automatically required when binding to non-loopback addresses. Loopback servers remain auth-free for backward compatibility.

## Key Decisions and Rationale

### 1. Persistent tokens (not ephemeral)
Token is generated on first `plug serve` run and reused from file on subsequent starts. Rationale: phone clients configure the token once; ephemeral tokens would break on every restart.

### 2. Origin validation bypass for authenticated requests
The existing `validate_origin` middleware rejects all non-localhost origins. Remote clients with valid bearer tokens need to bypass this. Solved via `AuthStatus` request extension: auth middleware sets it, origin middleware reads it.

**Critical insight**: Without this bypass, bearer auth would be useless for remote access — the origin middleware runs *after* auth and would reject the request regardless of valid token.

### 3. Lazy token generation
Token file is created when `plug serve` runs, not at config time. This avoids side effects in read-only config parsing and means `plug doctor` correctly shows "not yet initialized" (Warn) rather than "broken" (Fail) for fresh configs.

### 4. Discovery endpoint tiering
Unauthenticated requests to `/.well-known/mcp.json` get a minimal card (name, endpoint, transport). Authenticated requests get full details (server names, tool counts). Prevents reconnaissance of server inventory.

## Gotchas and Lessons Learned

### TOCTOU race in token file handling
**Original code**: `path.exists()` → `metadata()` → `read_to_string()` — three separate syscalls with race windows between them. An attacker with config directory access could swap the file between checks.

**Fix**: Open the file once, check permissions via `file.metadata()` on the open fd, read from the same fd. Single open eliminates the race.

### Non-atomic file creation (symlink attack vector)
**Original code**: `OpenOptions::new().create(true).truncate(true)` follows symlinks. If an attacker creates a symlink at the token path before first `plug serve`, the token gets written to an arbitrary location.

**Fix**: Use `.create_new(true)` first (O_CREAT | O_EXCL — fails if file exists), fall back to truncate only if `AlreadyExists`. This prevents symlink-following on initial creation.

### Auth check duplication
The discovery endpoint lives on a separate axum router from `/mcp` routes, so it doesn't get the auth middleware layer. Initially duplicated the full bearer token parsing inline.

**Fix**: Extract `check_bearer_token(headers, expected) -> bool` helper used by both the middleware and discovery handler. Single source of truth for auth verification logic.

### Doctor check severity
Initially made missing auth token file a `Fail`. But a fresh non-loopback config legitimately won't have a token until `plug serve` runs and generates one. The doctor check was producing false positives for valid-but-uninitialized configs.

**Fix**: Downgrade to `Warn` with message "not yet generated — run `plug serve` to initialize".

### `unreachable!()` in error handling
The `HttpError::Unauthorized` variant was handled via early-return before a match statement, with `unreachable!()` in the match arm. This is fragile — if the early return is refactored away, it panics at runtime.

**Fix**: Handle `Unauthorized` in the match arm directly (with `return` for the special WWW-Authenticate header logic).

## Architecture Pattern

```
Request → [body limit] → [bearer auth middleware] → [origin validation] → handler
                              ↓ sets AuthStatus extension
                              ↓ on failure: 401 + WWW-Authenticate: Bearer + tracing::warn
```

Auth middleware is the outermost application layer (but inside body limit for DoS prevention). It sets `AuthStatus::Authenticated` or `AuthStatus::NoAuthRequired` as a request extension. Origin validation reads this extension and skips origin checks for authenticated requests.

## Files Changed

- `plug-core/src/auth.rs` — New module: token generation, verification, TOCTOU-safe file loading, atomic file creation
- `plug-core/src/http/server.rs` — Auth middleware, AuthStatus enum, check_bearer_token helper, auth-aware discovery
- `plug-core/src/http/error.rs` — HttpError::Unauthorized variant with JSON-RPC error + WWW-Authenticate header
- `plug-core/src/doctor.rs` — check_http_auth diagnostic (Warn for missing, Warn for wrong perms, Pass otherwise)
- `plug/src/daemon.rs` — Uses shared write_token_file instead of inline file-writing
- `plug/src/runtime.rs` — Loads/generates token at serve startup for non-loopback binds
- `plug/src/views/overview.rs` — Surfaces auth status in `plug status`, `--show-token` flag
- `plug/src/main.rs` — `--show-token` CLI arg

## Review Process

5-agent parallel review (security sentinel, architecture strategist, code simplicity, pattern recognition, performance oracle). Key findings addressed:
- 2 security issues (TOCTOU, symlink) → fixed
- 1 observability gap (no auth failure logging) → fixed
- 3 code quality items (unreachable, duplication, daemon consolidation) → fixed
