---
status: pending
priority: p3
issue_id: "028"
tags: [code-review, ux, issue-7]
dependencies: []
---

# Failed server startup is logged but not surfaced to user

## Problem Statement

When a server fails to start (SSRF block, timeout, crash), the error is logged at `tracing::error` but the server simply doesn't appear in the tool list. For stdio-connected clients (the primary use case), log output may not be visible. The issue confirms: "iMessage tools may not appear in the tool list at all" — the user has no indication why.

## Findings

- **Source**: architecture-strategist
- **Location**: `plug-core/src/server/mod.rs:112-115` (error logged, server skipped)
- **Evidence**: `tracing::error!(server = %name, error = %e, "failed to start server")` — logged but not communicated to the MCP client. No diagnostic tool or startup summary provided.

## Proposed Solutions

### Option A: Add plug__startup_warnings meta-tool (Recommended)
If any servers failed to start, include a `plug__startup_warnings` tool whose description contains failure details. AI clients will see the tool and understand what's broken.
- **Pros**: Surfaces errors via existing MCP protocol, no changes to client
- **Cons**: Adds a tool that isn't really a tool
- **Effort**: Small

### Option B: Log to well-known file, surface via `plug status`
Write startup failures to a diagnostics file that `plug status` reads.
- **Pros**: Clean separation, doesn't pollute tool list
- **Cons**: User must know to run `plug status`
- **Effort**: Small

## Acceptance Criteria

- [ ] Users are informed when a configured server fails to start
- [ ] The failure reason is visible (SSRF block, timeout, crash, etc.)
- [ ] Working servers are not affected by failed server diagnostics
