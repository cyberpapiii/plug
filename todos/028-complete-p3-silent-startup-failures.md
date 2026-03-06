---
status: complete
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
- **Evidence**: `tracing::error!(server = %name, error = %e, "failed to start server")` — logged but not communicated to the user. No diagnostic tool or startup summary existed.

## Proposed Solutions

### Option A: Add plug__startup_warnings meta-tool (Recommended at discovery time)
If any servers failed to start, include a `plug__startup_warnings` tool whose description contains failure details.
- **Pros**: Surfaces errors via existing MCP protocol
- **Cons**: Adds a tool that isn't really a tool
- **Effort**: Small

### Option B: Log to well-known file, surface via `plug status`
Write startup failures to diagnostics surfaced through the CLI status path.
- **Pros**: Clean separation, doesn't pollute tool list
- **Cons**: User must know to inspect status output
- **Effort:** Small

## Acceptance Criteria

- [x] Users are informed when a configured server fails to start
- [x] The failure reason is visible through the existing runtime/status surface
- [x] Working servers are not affected by failed server diagnostics

## Work Log

### 2026-03-06 - Completed During v0.1 Stabilization

**By:** Codex

**Actions:**
- Added startup-failure tracking and failed-server status surfacing in `engine.rs`, `health.rs`, and `server/mod.rs`
- Updated `plug servers` output to show failed configured servers honestly
- Added regression coverage for failed-startup visibility

**Learnings:**
- For `v0.1`, surfacing failed startups in the status/servers path was enough. A dedicated warnings meta-tool is not needed yet.
