---
status: complete
priority: p1
issue_id: "022"
tags: [code-review, performance, architecture, issue-7]
dependencies: []
---

# Single timeout_secs used for both startup and tool calls

## Problem Statement

The `timeout_secs` config field (default 30s) controls both server startup initialization AND individual tool call execution. Startup is bounded (spawn + handshake + tools/list). Tool calls are unbounded — Slack `conversations_unreads` makes 130+ API calls and needs ~2 minutes. Users cannot configure appropriate timeouts for both scenarios simultaneously. A 30s tool call timeout kills legitimate long-running tools.

**Root cause of issue #7 problem 1 (Slack timeout).**

## Findings

- **Source**: performance-oracle, architecture-strategist, security-sentinel, code-simplicity-reviewer (all flagged)
- **Location**:
  - Definition: `plug-core/src/config/mod.rs:110` (single `timeout_secs` field, default 30s)
  - Startup usage: `plug-core/src/server/mod.rs:134`
  - Tool call usage: `plug-core/src/proxy/mod.rs:266`
- **Evidence**: Both locations use `Duration::from_secs(config.timeout_secs)` — identical timeout for fundamentally different operations

## Proposed Solutions

### Option A: Split into two config fields (Recommended)
Add `startup_timeout_secs` (default 30s) and `call_timeout_secs` (default 300s). Keep `timeout_secs` as deprecated alias for `startup_timeout_secs`.
- **Pros**: Precise control, backward compatible, clear semantics
- **Cons**: Adds one config field
- **Effort**: Small
- **Risk**: Low

### Option B: Remove tool call timeout entirely
Keep `timeout_secs` for startup only. Remove `tokio::time::timeout` from `call_tool`. Let the MCP client (Claude Code, Cursor) handle call timeouts — plug acts as clean pass-through.
- **Pros**: Simplest, aligns with "clean pass-through" principle, no config to get wrong
- **Cons**: No safety net for hung upstream servers (though circuit breaker provides some protection)
- **Effort**: Small
- **Risk**: Medium (hung calls could accumulate)

### Option C: Large default tool call timeout
Split timeout but default `call_timeout_secs` to 600s (10 min) as a safety net.
- **Pros**: Balanced safety and usability
- **Cons**: Still an arbitrary limit
- **Effort**: Small
- **Risk**: Low

## Acceptance Criteria

- [x] Server startup timeout is independent of tool call timeout
- [x] `call_timeout_secs` exists for long-running tool calls
- [x] Fast startup detection still uses `timeout_secs`
- [x] Existing `timeout_secs` config remains supported

## Work Log

### 2026-03-06 - Closed As Already Resolved

**By:** Codex

**Actions:**
- Verified in `plug-core/src/config/mod.rs` that `ServerConfig` already has both `timeout_secs` and `call_timeout_secs`
- Verified startup path in `plug-core/src/server/mod.rs` still uses `timeout_secs`
- Verified tool-call path in `plug-core/src/proxy/mod.rs` uses `call_timeout_secs`
- Confirmed with repo tests and code inspection that this todo no longer represents missing work

**Learnings:**
- This issue was valid at review time but became stale after the timeout split landed. Backlog cleanup is part of keeping compound execution honest.
