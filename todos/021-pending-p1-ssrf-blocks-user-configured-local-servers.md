---
status: pending
priority: p1
issue_id: "021"
tags: [code-review, security, architecture, issue-7]
dependencies: []
---

# SSRF protection blocks user-configured local MCP servers

## Problem Statement

The `is_blocked_host()` function blocks loopback IPs (`127.0.0.1`) and private ranges, preventing users from connecting to local MCP servers they explicitly configured. Meanwhile, `localhost` (a hostname) bypasses the same check. This inconsistency breaks iMessage MCP at `http://127.0.0.1:8080` while identical config using `localhost` would work. The SSRF check protects against a threat model (server-side request forgery) that does not apply to a desktop-local tool where the user authors all config.

**Root cause of issue #7 problem 2 (iMessage).**

## Findings

- **Source**: security-sentinel, architecture-strategist, code-simplicity-reviewer (all flagged independently)
- **Location**: `plug-core/src/server/mod.rs:445-474` (`is_blocked_host`, `is_blocked_ip`)
- **Call site**: `plug-core/src/server/mod.rs:199-208`
- **Evidence**: `is_blocked_host("127.0.0.1")` returns `true`, `is_blocked_host("localhost")` returns `false` — same destination, different result. Code comment at line 198 acknowledges: "DNS-based bypasses (hostname resolving to private IP) are not covered here"
- **Additional bypasses**: `localtest.me`, `169.254.169.254.nip.io`, any hostname resolving to private IP

## Proposed Solutions

### Option A: Remove SSRF check entirely (Recommended)
All upstream URLs come from the user's own TOML config. No untrusted input exists. Delete `is_blocked_host`, `is_blocked_ip`, call site, and all SSRF tests (~65 lines).
- **Pros**: Simplest, aligns with "ruthlessly minimal" principle, fixes issue immediately
- **Cons**: No protection if plug ever accepts URLs from untrusted sources (not currently planned)
- **Effort**: Small
- **Risk**: Low

### Option B: Narrow to cloud metadata endpoints only
Keep blocking only `169.254.0.0/16` (AWS/GCP metadata) and `metadata.google.internal`. Allow all other private/loopback IPs.
- **Pros**: Protects against cloud credential theft, allows local servers
- **Cons**: Still has DNS bypass for metadata endpoints
- **Effort**: Small
- **Risk**: Low

### Option C: Add per-server `allow_private = true` config option
Keep full SSRF check but add opt-out per server.
- **Pros**: Most flexible, future-proof
- **Cons**: Adds config complexity, users must know to set it, violates minimal principle
- **Effort**: Medium
- **Risk**: Low

## Acceptance Criteria

- [ ] `http://127.0.0.1:8080` connects successfully when configured by user
- [ ] `http://192.168.x.x` and other private IPs work for user-configured servers
- [ ] Cloud metadata endpoints (169.254.169.254) are either blocked or documented as risk
- [ ] No regression in existing SSRF test assertions (or tests removed/updated)
