---
status: complete
priority: p2
issue_id: "026"
tags: [code-review, reliability, protocol, issue-7]
dependencies: ["022"]
---

# Timeout drops tool call future but upstream stdio server keeps processing

## Problem Statement

When `tokio::time::timeout` fires at proxy/mod.rs:291, the future is dropped but the upstream stdio server continues processing and eventually writes a JSON-RPC response. This orphaned response sits in the protocol stream and may cause subsequent request/response mismatches. For stdio transport, this can corrupt the protocol state.

## Findings

- **Source**: performance-oracle
- **Location**: `plug-core/src/proxy/mod.rs:290-291`
- **Evidence**: `tokio::time::timeout(timeout_duration, peer.call_tool(upstream_params)).await` — when timeout fires, the future is dropped. rmcp's internal request map may leak the pending entry. The next tool call might receive the orphaned response.

## Proposed Solutions

### Option A: Mark connection for reconnection after timeout (Recommended)
After a timeout on a stdio server, schedule an async reconnection (kill process, respawn, re-init).
- **Pros**: Prevents protocol desync, clean state after timeout
- **Cons**: Reconnection is disruptive (drops all pending calls to that server)
- **Effort**: Medium

### Option B: Send cancellation notification
Send MCP `$/cancelRequest` to the upstream (if supported), let rmcp clean up.
- **Pros**: Graceful, no reconnection needed
- **Cons**: Not all MCP servers support cancellation
- **Effort**: Medium

### Option C: Remove tool call timeout (defer to client)
If tool call timeout is removed entirely (see todo 022 Option B), this problem disappears.
- **Pros**: Eliminates the root cause
- **Cons**: Depends on resolving todo 022 with Option B
- **Effort**: Small (if 022 is resolved first)

## Acceptance Criteria

- [x] After a timeout on a stdio server, subsequent calls are not corrupted
- [x] The server returns to a clean state via reconnection
- [x] The timeout path is covered by a real stdio integration test

## Work Log

### 2026-03-06 - Completed In Worktree Execution

**By:** Codex

**Actions:**
- Added stdio reconnect-on-timeout behavior in `plug-core/src/proxy/mod.rs`
- Added a real integration test using a wrapper script plus `mock-mcp-server` to prove:
  - first call times out on a slow stdio server
  - plug reconnects in the background
  - the next call succeeds against the clean replacement process

**Learnings:**
- The issue was real, but it needed a real stdio integration test rather than a unit-only proof because the risk was protocol-state corruption across requests.
- Reconnect should be backgrounded after timeout so the caller still gets a prompt timeout response instead of paying reconnect latency inline.
