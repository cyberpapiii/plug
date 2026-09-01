---
status: complete
priority: p3
issue_id: "052"
tags: [code-review, oauth, agent-native, observability, ipc]
dependencies: []
---

# OAuth manual refresh IPC decision

## Problem Statement

After PR #50 (refresh-exchange observability), the remaining question was whether agents also need
an IPC command to trigger token refresh manually.

This is not a correctness bug. The background refresh logic is already sound.

## Findings

- Identified by agent-native-reviewer during PR #42 CE review (score: 7/11 agent-accessible)
- PR #45 closed `AuthStateChanged` transport parity
- PR #50 closed the distinct successful refresh-exchange observability gap
- On current `main`, agents already have `AuthStatus` polling, background refresh, and `InjectToken`
  for explicit credential replacement and reconnect
- No concrete blocked user or agent workflow was identified that requires a manual refresh command

## Decision

Manual refresh IPC is not warranted on current `main`.

Evidence:
- `AuthStatus` already lets agents observe token timing and auth state
- the engine already performs background refresh and reconnect automatically
- `InjectToken` already provides the explicit “act now” path when an agent or user has replacement
  credentials
- a manual refresh IPC command would add protocol and daemon complexity without solving a clearly
  blocked workflow

## Acceptance Criteria

- [x] Decision documented on whether manual refresh IPC command is warranted

## Work Log

- 2026-03-09: Identified during PR #42 CE review (agent-native-reviewer)
- 2026-03-10: PR #45 merged; `AuthStateChanged` transport parity landed for HTTP SSE and direct stdio via logging-channel fan-out
- 2026-03-10: PR #50 merged; successful token refresh exchange is now observable as a distinct logging signal across direct and daemon-backed downstream delivery paths
- 2026-03-10: Decision pass on current `main` concluded manual refresh IPC is not warranted; closed as resolved-by-decision with no runtime change.

## Resources

- PR #42
- PR #41 (IPC auth commands)
- PR #45
- PR #50
- `plug-core/src/notifications.rs` — `ProtocolNotification::AuthStateChanged`
- `plug-core/src/engine.rs` — refresh success signal emission point
- `plug/src/daemon.rs` — IPC notification dispatch
- `plug-core/src/ipc.rs` — existing `AuthStatus` and `InjectToken` IPC surface
