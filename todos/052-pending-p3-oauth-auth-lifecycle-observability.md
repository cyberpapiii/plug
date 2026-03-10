---
status: pending
priority: p3
issue_id: "052"
tags: [code-review, oauth, agent-native, observability, ipc]
dependencies: []
---

# OAuth auth lifecycle observability follow-ups

## Problem Statement

After PR #45 (AuthStateChanged transport parity), agents connected via IPC, HTTP, or stdio still
have two remaining auth lifecycle observability gaps:

1. **No notification on successful token refresh.** Agents see `ServerStarted` after reconnect but
   cannot distinguish "reconnected with fresh token" from "reconnected due to network issue." This
   prevents agents from logging auth lifecycle or adjusting retry behavior.

2. **No IPC command for manual refresh.** Agents can observe a token approaching expiry via
   `AuthStatus` polling but cannot proactively trigger a refresh — they must wait for the background
   loop or re-inject via `InjectToken`.

These are observability gaps, not correctness bugs. The refresh logic itself is sound.

## Findings

- Identified by agent-native-reviewer during PR #42 CE review (score: 7/11 agent-accessible)
- PR #45 closed the first narrowed slice: `AuthStateChanged` is now observable to non-IPC clients
  via synthetic structured logging fan-out
- `IpcAuthServerInfo` does not distinguish injected vs OAuth tokens
- Transient refresh failures are invisible to all downstream clients

## Proposed Solutions

### Option A: Incremental notification additions

Add a `TokenRefreshed` variant to `ProtocolNotification` (or reuse `AuthStateChanged` with a
`new_state: Healthy` payload). Defer manual refresh IPC command until there's a concrete use case.

**Pros:** Minimal scope, addresses the highest-value remaining visibility gap
**Cons:** Manual refresh still not possible
**Effort:** Small–Medium
**Risk:** Low

### Option B: Full agent auth parity

Add `TokenRefreshed` notification, `RefreshToken` IPC command, and injected-vs-OAuth distinction in
`IpcAuthServerInfo`.

**Pros:** Complete agent parity
**Cons:** Larger scope; manual refresh command needs design thought
**Effort:** Medium–Large
**Risk:** Medium

## Acceptance Criteria

- [ ] Agents can distinguish token-refresh reconnect from network reconnect
- [ ] Decision documented on whether manual refresh IPC command is warranted

## Work Log

- 2026-03-09: Identified during PR #42 CE review (agent-native-reviewer)
- 2026-03-10: PR #45 merged; `AuthStateChanged` transport parity landed for HTTP SSE and direct stdio via logging-channel fan-out

## Resources

- PR #42
- PR #41 (IPC auth commands)
- PR #45
- `plug-core/src/notifications.rs` — `ProtocolNotification::AuthStateChanged`
- `plug/src/daemon.rs` — IPC notification dispatch
