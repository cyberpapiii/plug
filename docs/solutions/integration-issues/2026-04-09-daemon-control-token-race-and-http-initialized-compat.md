---
title: "Daemon control-token race and HTTP initialized-notification compatibility"
date: 2026-04-09
category: integration-issues
status: implemented
tags: [daemon, ipc, oauth, http, krisp, todoist, supabase, compatibility]
---

# Daemon control-token race and HTTP initialized-notification compatibility

## Problem

Two separate issues combined into one misleading operator experience:

1. The daemon control path could become desynchronized from its on-disk control files.
2. Some remote HTTP MCP servers could successfully answer `initialize` and later reject
   `notifications/initialized`, causing startup to fail even though the connection was otherwise usable.

In practice this showed up as:

- `plug reload`, `plug stop`, and `plug auth inject` failing with `AUTH_FAILED: invalid auth token`
- `plug.pid` becoming empty while a daemon process was still running
- servers such as Todoist and Krisp oscillating between `Auth Required`, `Failed`, and `Healthy`
- Supabase still genuinely failing due to an invalid upstream OAuth token

## Root Causes

### 1. Daemon startup could overwrite control files before it actually owned the daemon

In `plug/src/daemon.rs`, daemon startup:

- wrote `plug.token` before it had exclusive daemon ownership
- opened `plug.pid` with truncation before acquiring the PID-file lock

That meant a losing concurrent startup attempt could:

- overwrite the control token used by admin IPC commands
- blank out the PID file
- fail to become the daemon
- leave the original daemon alive with an older in-memory auth token

This explains the observed state where:

- read-only runtime queries still worked
- admin IPC commands failed with `AUTH_FAILED: invalid auth token`
- doctor reported an invalid PID file

### 2. HTTP upstream startup treated `notifications/initialized` failure as fatal

For some remote HTTP servers, the MCP startup sequence looked like:

1. `initialize` succeeds
2. follow-up `notifications/initialized` is rejected
3. normal RPCs such as `tools/list` still work

The upstream HTTP worker in rmcp treats send-time failure for `notifications/initialized` as fatal.
For those servers, plug never reached the real validation step (`tools/list`) and incorrectly treated
the startup as a broken auth/runtime state.

This was reproduced in a dedicated integration fixture and matched live behavior seen with Krisp/Todoist
at different points during investigation.

## Fixes

### Daemon control path hardening

Startup ordering in `plug/src/daemon.rs` was changed so that:

- daemon ownership is claimed via PID-file locking before writing a fresh control token
- PID contents are only truncated after the lock is successfully acquired

This prevents a failed concurrent startup from corrupting `plug.pid` or rotating `plug.token`
out from under a running daemon.

Regression coverage:

- `daemon::tests::acquire_pid_lock_does_not_truncate_existing_file_on_failed_relock`

### General HTTP compatibility for initialized-notification failure

Plug now wraps the upstream HTTP client path with a compatibility-aware client that:

- preserves normal behavior for all ordinary requests
- treats `notifications/initialized` failure as compatibility noise for HTTP upstream startup
- still requires the connection to prove usefulness via later calls such as `tools/list`

This is intentionally general rather than server-specific:

- it is keyed to the MCP handshake boundary (`notifications/initialized`)
- it applies to HTTP upstreams that otherwise establish a working session
- it does not special-case Krisp, Todoist, or any specific hostname

Regression coverage:

- `test_oauth_server_can_start_when_initialized_notification_is_rejected`
- `test_oauth_stateless_http_server_with_valid_credentials_starts_healthy`
- `test_oauth_startup_failure_with_valid_credentials_is_not_auth_required`

## Live Findings

### Krisp

Krisp was not just a stale-token problem.

Key findings:

- a probe using the wrong protocol shape produced misleading auth failures
- the actual plug startup path (`2025-11-25` initialize) could succeed
- Krisp then failed specifically on `notifications/initialized`
- after the compatibility fix, Krisp came up healthy in the live daemon

### Todoist

Todoist’s saved token was valid the whole time.

Direct upstream probing showed:

- `initialize` succeeded
- `notifications/initialized` could succeed directly
- `tools/list` succeeded

The stale/broken daemon control path was a major reason Todoist could remain stuck in a misleading
runtime state. After the daemon control fix and clean restart, Todoist came up healthy.

### Supabase

Supabase remained a real auth failure throughout.

Direct upstream probing consistently returned:

- `401 Unauthorized`
- `Invalid oauth access token`

So Supabase is the remaining operator action item: refresh the upstream login.

## Learnings

### 1. Not every OAuth startup failure is an auth failure

For HTTP upstreams, a startup failure can mean:

- genuine invalid credentials
- protocol-version mismatch in the probe
- handshake incompatibility after successful initialize
- ordinary runtime failure after credentials were already accepted

Treating all OAuth startup failures as `AuthRequired` creates misleading recovery advice.

### 2. Control-plane races can look like auth bugs

When daemon control files drift out of sync with the live daemon, the operator sees:

- broken reload/restart commands
- stale runtime state
- contradictory status surfaces

That can be mistaken for remote OAuth instability when the actual problem is local daemon ownership.

### 3. `notifications/initialized` is not a reliable compatibility boundary for HTTP startup

Some HTTP servers treat it as effectively optional or reject it while still being usable. A robust
multiplexer should validate usefulness with real follow-up capability calls instead of assuming
initialized-notification success is always required to continue.

## Verification

Local verification that passed for this branch:

- `cargo test -p plug acquire_pid_lock_does_not_truncate_existing_file_on_failed_relock -- --nocapture`
- `cargo test -p plug-core oauth -- --nocapture`

Live verification after reinstall + daemon restart:

- `plug reload` succeeded again
- `plug.pid` contained a real PID instead of being blank
- `plug status` showed `krisp` healthy
- `plug status` showed `todoist` healthy
- `plug status` continued to show `supabase` failed

## Scope Note

This note records branch-local implementation and investigation results from 2026-04-09.
It should not be treated as proof of `done on main` unless the code is merged and the current-truth
docs are updated accordingly.
