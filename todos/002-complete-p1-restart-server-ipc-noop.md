---
status: complete
priority: p1
issue_id: "002"
tags: [code-review, correctness, daemon, agent-native]
dependencies: []
---

# RestartServer IPC returns Ok without actually restarting

## Problem Statement

The daemon's `dispatch_request` for `IpcRequest::RestartServer` validates the server exists and returns `IpcResponse::Ok`, but never actually restarts the server. An agent or CLI user gets a success response while nothing happens — a silent failure.

## Findings

- **Source**: simplicity-review, agent-native-review, architecture-review, security-review (all flagged)
- **Location**: `plug/src/daemon.rs:475-487`
- **Evidence**: Comment says "For now, return Ok — full restart integration requires Engine in the task"
- `ConnectionContext` holds `server_manager` and `tool_router` but not `Engine`

## Proposed Solutions

### Option A: Add Arc<Engine> to ConnectionContext (Recommended)
Pass `Arc<Engine>` into `ConnectionContext` and call `engine.restart_server()` in dispatch.
- **Pros**: Consistent with TUI which already calls `engine.restart_server()`
- **Cons**: Need to wrap Engine in Arc (it's currently owned by cmd_daemon)
- **Effort**: Medium

### Option B: Return honest error
Return `IpcResponse::Error { code: "NOT_IMPLEMENTED" }` instead of `Ok`.
- **Pros**: Immediate fix, no architectural change
- **Cons**: Feature still doesn't work
- **Effort**: Small

## Acceptance Criteria

- [ ] `RestartServer` IPC request either works or returns an error (never silent Ok)
- [ ] If implemented: server is actually restarted and events are emitted
- [ ] Same fix applies to `SetServerEnabled`
