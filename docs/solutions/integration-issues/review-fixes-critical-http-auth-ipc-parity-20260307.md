---
title: "Critical review fixes: authenticated HTTP upstreams, daemon IPC parity, and active-call cleanup"
category: integration-issues
tags:
  - review-fixes
  - http-auth
  - ipc
  - resources
  - prompts
  - active-calls
  - reliability
module: plug + plug-core
date: 2026-03-07
symptom: |
  A post-Phase-3 review surfaced several still-real issues on current `main`:
  authenticated HTTP upstreams sent `Bearer [REDACTED]`, daemon-backed stdio clients still could not
  access resources/prompts like direct clients, daemon IPC capabilities were hardcoded instead of
  synthesized, and active call bookkeeping still relied on explicit cleanup only.
root_cause: |
  The SecretString display hardening fixed logging but accidentally broke an auth-header call site
  that still used `Display`. Separately, the daemon IPC path had only been completed for tools, not
  the broader MCP surface added later in the direct runtime path. Active call tracking also had no
  drop guard to clean up if the future itself disappeared before a normal response path executed.
severity: high
related:
  - docs/brainstorms/2026-03-07-review-fixes-critical-brainstorm.md
  - docs/plans/2026-03-07-fix-review-fixes-critical-plan.md
  - plug-core/src/server/mod.rs
  - plug/src/daemon.rs
  - plug/src/ipc_proxy.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/notifications.rs
---

# Critical review fixes: authenticated HTTP upstreams, daemon IPC parity, and active-call cleanup

## Problem

The Phase 2 / Phase 3 rollout was broadly sound, but the review found a few issues that were both
real and severe enough to fix immediately:

1. authenticated HTTP upstreams were sending `Bearer [REDACTED]`
2. daemon-backed stdio clients still only had tool parity, not resources/prompts parity
3. daemon IPC capability advertising was hardcoded
4. active-call bookkeeping lacked a drop guard

## Solution

### 1. Fix bearer-header construction

The HTTP upstream path now uses `token.as_str()` instead of `Display`, so auth headers use the real
secret while logs remain redacted.

### 2. Add daemon IPC capabilities and broader MCP dispatch

The daemon IPC protocol gained a capabilities request and the daemon dispatch path now proxies:

- `resources/list`
- `resources/templates/list`
- `resources/read`
- `prompts/list`
- `prompts/get`

This makes daemon-backed stdio clients materially closer to direct-mode behavior.

### 3. Cache synthesized capabilities in the IPC proxy session

`IpcProxyHandler` now seeds and refreshes its capability view from the daemon runtime instead of
hardcoding a tools-only advertisement.

### 4. Add an active-call drop guard

The tool router now has an RAII-style cleanup guard for registered active calls so leaked tracking
state is less likely when futures fail or are dropped on unusual paths.

### 5. Remove two low-risk sharp edges

- resource URI collisions now emit warnings
- protocol notification JSON conversion logs and degrades safely instead of panicking

## Non-Goals

This fix pass intentionally did **not** include:

- blanket rate limiting of `tools/call`
- `resources/subscribe`
- route-lookup micro-optimization

Those are separate decisions from the critical regressions above.
