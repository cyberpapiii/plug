---
status: complete
priority: p1
issue_id: "064"
tags: [code-review, tasks, ipc, daemon, reconnect, correctness]
dependencies: []
---

# Preserve IPC task ownership across daemon reconnect

## Problem Statement

IPC task ownership is currently keyed to the daemon `session_id`. When the daemon proxy reconnects, that session ID is replaced for the same logical client.

As a result, tasks created before reconnect become inaccessible to the same client after reconnect, which violates the plan boundary that tasks should survive reconnect within the same runtime.

## Findings

- `task_owner_for_ipc_session` derives ownership from `session_id` in [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs).
- The reconnect path replaces the IPC session ID inside `DaemonProxySession` in [ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs).
- Daemon task routes use the transient session ID again for `tasks/list`, `tasks/get`, `tasks/result`, and `tasks/cancel` in [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs).
- Multiple review passes converged on the same issue, and it directly conflicts with the task plan’s reconnect-retention requirement.

## Proposed Solutions

### Option 1: Key task ownership by stable `client_id`

**Approach:** Thread the stable daemon `client_id` through task ownership instead of using the replaced `session_id`.

**Pros:**
- Matches logical client identity
- Aligns with reconnect expectations
- Minimal conceptual model

**Cons:**
- Requires touching IPC routing and daemon lookup paths

**Effort:** 3-5 hours

**Risk:** Medium

---

### Option 2: Introduce a reconnect-stable task owner token

**Approach:** Mint an owner token at first registration and persist it across reconnects, independent of both session ID and client ID.

**Pros:**
- Explicit ownership boundary
- Flexible for future multi-session models

**Cons:**
- More moving parts than necessary for current architecture

**Effort:** 5-8 hours

**Risk:** Medium

## Recommended Action

To be filled during triage.

## Technical Details

**Affected files:**
- [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)
- [ipc_proxy.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/ipc_proxy.rs)

**Related components:**
- Daemon proxy session lifecycle
- Task ownership model
- Reconnect continuity guarantees

## Resources

- **Branch:** `feat/core-mcp-tasks-support`
- **Plan:** [2026-03-22-003-feat-complete-core-mcp-tasks-support-plan.md](/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-22-003-feat-complete-core-mcp-tasks-support-plan.md)

## Acceptance Criteria

- [ ] Tasks created by a daemon-backed client remain visible after reconnect within the same runtime
- [ ] `tasks/list`, `tasks/get`, `tasks/result`, and `tasks/cancel` all work after reconnect
- [ ] Ownership remains isolated between distinct logical clients
- [ ] Integration tests explicitly cover daemon reconnect plus task continuity

## Work Log

### 2026-03-22 - Code review capture

**By:** Codex / ce:review

**Actions:**
- Traced task owner construction across proxy, daemon, and IPC reconnect code
- Confirmed current ownership is based on transient session identity
- Captured as a merge-blocking issue because it violates tranche acceptance criteria

**Learnings:**
- Session replacement and logical-client continuity are distinct concepts
- Ownership should be attached to the stable identity that the product promises continuity for

### 2026-03-22 - Fix implemented

**By:** Codex / ce:work

**Actions:**
- Switched daemon task ownership lookups to stable IPC `client_id` instead of transient `session_id`
- Added a daemon-backed regression test proving task access survives session replacement for the same client

**Learnings:**
- The daemon already knows the reconnect-stable client identity; the bug was just using the wrong key at the task boundary
