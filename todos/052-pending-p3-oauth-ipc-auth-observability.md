---
status: pending
priority: p3
issue_id: "052"
tags: [code-review, oauth, agent-native, observability, ipc]
dependencies: []
---

# OAuth / IPC auth observability follow-ups

## Problem Statement

After PR #42 (zero-downtime token refresh), agents connected via IPC, HTTP, or stdio have limited
visibility into auth lifecycle events. Three specific gaps:

1. **No notification on successful token refresh.** Agents see `ServerStarted` after reconnect but
   cannot distinguish "reconnected with fresh token" from "reconnected due to network issue." This
   prevents agents from logging auth lifecycle or adjusting retry behavior.

2. **No IPC command for manual refresh.** Agents can observe a token approaching expiry via
   `AuthStatus` polling but cannot proactively trigger a refresh — they must wait for the background
   loop or re-inject via `InjectToken`.

3. **`AuthStateChanged` not forwarded beyond IPC.** The notification is emitted as a
   `ProtocolNotification` but only IPC clients receive it. HTTP and stdio clients are blind to auth
   state transitions (e.g. Healthy → AuthRequired).

These are observability gaps, not correctness bugs. The refresh logic itself is sound.

## Findings

- Identified by agent-native-reviewer during PR #42 CE review (score: 7/11 agent-accessible)
- `AuthStateChanged` is already a `ProtocolNotification` variant, but HTTP/stdio fan-out does not
  handle it
- `IpcAuthServerInfo` does not distinguish injected vs OAuth tokens
- Transient refresh failures are invisible to all downstream clients

## Proposed Solutions

### Option A: Incremental notification additions

Add a `TokenRefreshed` variant to `ProtocolNotification` (or reuse `AuthStateChanged` with a
`new_state: Healthy` payload). Forward `AuthStateChanged` to HTTP SSE streams and stdio. Defer
manual refresh IPC command until there's a concrete use case.

**Pros:** Minimal scope, addresses the highest-value gap (visibility)
**Cons:** Manual refresh still not possible
**Effort:** Small–Medium
**Risk:** Low

### Option B: Full agent auth parity

Add `TokenRefreshed` notification, `RefreshToken` IPC command, `AuthStateChanged` forwarding to all
transports, and injected-vs-OAuth distinction in `IpcAuthServerInfo`.

**Pros:** Complete agent parity
**Cons:** Larger scope; manual refresh command needs design thought
**Effort:** Medium–Large
**Risk:** Medium

## Acceptance Criteria

- [ ] Agents can distinguish token-refresh reconnect from network reconnect
- [ ] `AuthStateChanged` reaches HTTP and stdio clients (not just IPC)
- [ ] Decision documented on whether manual refresh IPC command is warranted

## Work Log

- 2026-03-09: Identified during PR #42 CE review (agent-native-reviewer)

## Resources

- PR #42
- PR #41 (IPC auth commands)
- `plug-core/src/notifications.rs` — `ProtocolNotification::AuthStateChanged`
- `plug/src/daemon.rs` — IPC notification dispatch
