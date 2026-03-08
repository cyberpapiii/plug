---
status: complete
priority: p3
issue_id: "047"
tags: [code-review, performance, ipc]
dependencies: ["044"]
---

# IPC proxy double-parses every frame (worse than documented F4)

## Problem Statement

The IPC proxy's read loop tries `serde_json::from_slice::<DaemonToProxyMessage>(&frame)` on every frame, then falls through to `IpcResponse` parse on failure. For normal frames (pings every 1s, tool responses, etc.), the first parse always fails. This doubles JSON deserialization cost on the hot path.

This was documented as deferred finding F4 in the plan, but the performance review revealed it affects the 1-second heartbeat ping — meaning it fires continuously, not just during tool calls.

Flagged by: performance-oracle (Critical), architecture-strategist.

## Findings

- `plug/src/ipc_proxy.rs:143-173`: Try DaemonToProxyMessage, fallback to IpcResponse
- Affects every IPC frame including 1-second heartbeat pings
- `DaemonToProxyMessage` uses `"envelope"` tag, `IpcResponse` uses `"type"` tag

## Proposed Solutions

### Option A: Unified envelope framing (Recommended, pair with 044)

Have daemon always wrap all responses in `DaemonToProxyMessage::Response { inner }`. Eliminates fallback parse entirely. Best paired with IPC version bump (todo 044).

- Pros: Single parse path, clean protocol, no fallback
- Cons: Requires protocol version bump (already needed)
- Effort: Small
- Risk: Low (paired with version bump)

### Option B: Lightweight discriminator check

Check for `"envelope"` key before full parse: `if frame.contains(b"\"envelope\"")` then parse as envelope, else parse as IpcResponse.

- Pros: No protocol change needed
- Cons: Fragile byte-level check
- Effort: Trivial
- Risk: Low

## Recommended Action

_To be filled during triage_

## Technical Details

- **Affected files**: `plug/src/ipc_proxy.rs`, `plug/src/daemon.rs`

## Acceptance Criteria

- [ ] Normal IPC frames (pings, responses) are parsed exactly once
- [ ] Reverse-request envelope frames still handled correctly

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review | Performance review elevated severity from deferred to p3 due to heartbeat impact |
