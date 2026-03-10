---
status: pending
priority: p3
issue_id: "052"
tags: [code-review, oauth, agent-native, observability, ipc]
dependencies: []
---

# OAuth auth lifecycle observability follow-ups

## Problem Statement

After PR #50 (refresh-exchange observability), agents connected via IPC, HTTP, or stdio still
have one remaining auth lifecycle observability gap:

1. **No IPC command for manual refresh.** Agents can observe a token approaching expiry via
   `AuthStatus` polling but cannot proactively trigger a refresh — they must wait for the background
   loop or re-inject via `InjectToken`.

These are observability gaps, not correctness bugs. The refresh logic itself is sound.

## Findings

- Identified by agent-native-reviewer during PR #42 CE review (score: 7/11 agent-accessible)
- PR #45 closed the first narrowed slice: `AuthStateChanged` is now observable to non-IPC clients
  via synthetic structured logging fan-out
- PR #50 closed the second narrowed slice: successful token refresh exchange is now observable as a
  distinct signal, separate from reconnect visibility
- `IpcAuthServerInfo` does not distinguish injected vs OAuth tokens
- Transient refresh failures are invisible to all downstream clients

## Proposed Solutions

### Option A: Incremental notification additions

Defer manual refresh IPC command until there's a concrete use case, or explicitly decide it is not
warranted.

**Pros:** Minimal remaining scope
**Cons:** Manual refresh still not possible
**Effort:** Small
**Risk:** Low

### Option B: Full agent auth parity

Add `RefreshToken` IPC command and injected-vs-OAuth distinction in `IpcAuthServerInfo`.

**Pros:** Complete agent parity
**Cons:** Larger scope; manual refresh command needs design thought
**Effort:** Medium–Large
**Risk:** Medium

## Acceptance Criteria

- [ ] Decision documented on whether manual refresh IPC command is warranted

## Work Log

- 2026-03-09: Identified during PR #42 CE review (agent-native-reviewer)
- 2026-03-10: PR #45 merged; `AuthStateChanged` transport parity landed for HTTP SSE and direct stdio via logging-channel fan-out
- 2026-03-10: PR #50 merged; successful token refresh exchange is now observable as a distinct logging signal across direct and daemon-backed downstream delivery paths

## Resources

- PR #42
- PR #41 (IPC auth commands)
- PR #45
- PR #50
- `plug-core/src/notifications.rs` — `ProtocolNotification::AuthStateChanged`
- `plug-core/src/engine.rs` — refresh success signal emission point
- `plug/src/daemon.rs` — IPC notification dispatch
