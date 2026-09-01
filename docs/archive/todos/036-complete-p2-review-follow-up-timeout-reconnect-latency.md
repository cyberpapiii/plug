---
status: complete
priority: p2
issue_id: "036"
tags: [code-review, performance, proxy]
dependencies: []
---

# Move stdio timeout reconnect off the caller latency path

## Problem Statement

The initial stdio timeout fix awaited reconnect inline before returning the timeout, which could make the observed request latency exceed the configured tool timeout.

## Findings

- Timeout branch in `plug-core/src/proxy/mod.rs` awaited reconnect work directly
- Reconnect performs full server start, MCP init, tool discovery, and cache refresh

## Recommended Action

Schedule stdio reconnect in the background after timeout, so timeout responses stay prompt while the connection is repaired for the next call.

## Acceptance Criteria

- [x] Timed-out stdio calls return promptly without waiting for reconnect completion
- [x] Reconnect still runs for subsequent-call safety

## Work Log

### 2026-03-06 - Completed During Review Follow-up

**By:** Codex

**Actions:**
- Added shared reconnect helpers in `ToolRouter`
- Moved stdio timeout reconnect to a background task

**Learnings:**
- Repairing protocol state is important, but it should not silently extend the caller-visible timeout path.
