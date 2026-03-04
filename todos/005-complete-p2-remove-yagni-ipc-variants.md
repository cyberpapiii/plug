---
status: complete
priority: p2
issue_id: "005"
tags: [code-review, simplicity, architecture]
dependencies: []
---

# Remove unused IPC protocol variants (YAGNI)

## Problem Statement

The IPC protocol has 8 request variants but only 3 are actually used by CLI commands (Status, RestartServer, Shutdown). The rest are dead code: Subscribe (no CLI consumer), SetServerEnabled (always-failing stub), ServerList (redundant with Status), ClientList (always returns 0), ToolList (no CLI consumer).

## Findings

- **Source**: simplicity-review, agent-native-review
- **Estimated LOC reduction**: ~80 lines from ipc.rs + daemon.rs dispatch

## Proposed Solutions

### Option A: Collapse to 3 variants (Recommended)
Keep Status (returns everything), RestartServer, Shutdown. Remove Subscribe, SetServerEnabled, ServerList, ClientList, ToolList.
- **Pros**: ~80 LOC removed, simpler IPC contract
- **Cons**: Must re-add when features are actually needed
- **Effort**: Medium

## Acceptance Criteria

- [ ] IPC protocol has only variants with actual callers
- [ ] No `IpcResponse::Event` (Subscribe removed)
- [ ] Status response includes all server/tool/client data
- [ ] All tests still pass
