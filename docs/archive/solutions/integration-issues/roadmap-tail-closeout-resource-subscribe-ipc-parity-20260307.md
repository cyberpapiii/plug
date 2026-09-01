---
title: Resource subscribe parity and cleanup closeout
date: 2026-03-07
category: integration-issues
module: plug/ipc
problem_type: integration_issue
summary: Finished the roadmap tail by adding `resources/subscribe` and `resources/unsubscribe`
  with targeted `notifications/resources/updated`, including daemon-backed `plug connect`
  parity via interleaved IPC notifications and reconnect subscription restore, plus
  dead dependency cleanup.
tags:
- ipc
- integration-issues
components:
- plug-core/src/proxy/subscriptions.rs
- plug-core/src/proxy/mod.rs
- plug-core/src/http/server.rs
- plug-core/src/session/stateful.rs
- plug-core/src/ipc.rs
- plug/src/daemon/mod.rs
- plug/src/daemon/notify.rs
- plug/src/daemon/mcp_dispatch.rs
- plug/src/ipc_proxy.rs
- plug/src/runtime.rs
related:
- docs/brainstorms/2026-03-07-roadmap-tail-closeout-brainstorm.md
- docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md
- docs/solutions/architecture-patterns/resource-subscription-transitions-and-owner-reconciliation.md
- docs/solutions/integration-issues/phase2c-resources-prompts-pagination-20260307.md
- docs/solutions/integration-issues/downstream-https-serving-20260307.md
---

# Resource Subscribe Parity And Cleanup Closeout

## Problem

After the Phase 1-3 roadmap work, `plug` still had one meaningful protocol gap: resources could be listed and read, but `resources/subscribe` was still missing.

The easy implementation path would have been:
- add subscribe/unsubscribe to the shared router
- fan out `notifications/resources/updated` to direct stdio and HTTP only
- leave daemon-backed `plug connect` without parity

That was technically smaller, but it would have created a product split: the same feature would work for `plug serve` and direct stdio while silently degrading through the daemon-backed path.

At the same time, the repo still carried dead TUI dependencies (`ratatui`, `crossterm`, `color-eyre`) even though no live code used them.

## Constraints

- Keep upstream session sharing intact.
- Preserve the existing request/response behavior for daemon IPC.
- Deliver updates only to the downstream targets that actually subscribed.
- Avoid reopening the broader router/runtime hot-reload redesign.

## Solution

### 1. Subscription bookkeeping lives in `SubscriptionRegistry`

Current concurrency and ownership rules are documented in [Resource subscription transitions and owner reconciliation](../architecture-patterns/resource-subscription-transitions-and-owner-reconciliation.md).

Live state is URI-keyed (`entries: DashMap`) with per-entry downstream `members`, generation, `Pending`/`Active`/`Draining`, and confirmed `owner_server_id`. There is no separate reverse index of target → URIs; membership lives on the URI entry.

Downstream targets include daemon-backed sessions:

- `NotificationTarget::Stdio`
- `NotificationTarget::Http`
- `NotificationTarget::Ipc`

### 2. Keep direct stdio and HTTP thin

Direct stdio and HTTP do not own subscription state.

They only:
- forward `resources/subscribe`
- forward `resources/unsubscribe`
- deliver targeted resource-update notifications using the same internal notification bus already used for tool-list changes, progress, and cancellation

That keeps the registry as the only place that knows about reference counting and upstream transition rules.

### 3. Daemon-backed parity uses the control channel, not a second attach socket

Daemon-backed `plug connect` needed real push delivery, not capability masking.

Current shape:

- daemon IPC protocol version is `3` (`IPC_PROTOCOL_VERSION`)
- push notifications are typed `IpcResponse` variants interleaved on the proxy connection (`ResourceUpdatedNotification`, list-changed, progress, cancelled, logging, auth-state, …)
- `plug/src/daemon/notify.rs` fans those control notifications to the owning IPC writer
- the proxy read loop peeks `"envelope"` when discriminating `DaemonToProxyMessage` vs plain `IpcResponse`

There is no separate `AttachNotifications` / `McpNotification` attach stream. Identity for targeted IPC delivery uses `NotificationTarget::Ipc` with the session id as `client_id`.

### 4. Replay subscriptions after daemon session replacement

Daemon reconnects replace the logical session ID.

Without extra work, that would silently drop all resource subscriptions because the daemon cleans up subscriptions when the old session disappears.

`IpcProxyHandler` keeps a local `ReplayState.subscriptions` set and restores it after reconnect with one `IpcRequest::RestoreResourceSubscriptions` round-trip (daemon applies the set with bounded concurrency) before continuing the retried request.

### 5. Clean up HTTP expiry correctly

The original HTTP teardown only handled:
- explicit `DELETE /mcp`
- opportunistic cleanup when a later notification hit a dead session

That still leaked subscriptions for naturally expired sessions.

[`plug-core/src/session/stateful.rs`](../../../plug-core/src/session/stateful.rs) supports `with_expiry_notifier`, and the HTTP runtime registers router cleanup once when it constructs the session store. Explicit delete, validation-time expiry, and background cleanup expiry converge on the same subscription teardown path.

### 6. Remove dead TUI dependencies

The workspace manifest no longer carries:
- `ratatui`
- `crossterm`
- `color-eyre`

## Verification

These all passed on the finished branch:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check licenses
cargo test
```

Additional focused coverage now proves:

- direct stdio resource subscriptions reference-count correctly and fan out updates
- targeted resource updates reach HTTP SSE clients
- daemon-backed reconnect continuity still works after the IPC changes

## Prevention

1. Do not accept transport splits casually. If a feature is user-visible, verify whether daemon-backed `plug connect`, direct stdio, and HTTP all need the same behavior.
2. If a session-based transport reconnects by replacing session identity, replay any stateful protocol surface tied to that identity.
3. Session cleanup should not rely on “future notifications might eventually notice.” Add a real teardown hook.
4. When adding notification delivery to an existing IPC protocol, bind delivery to the owning session identity, not an unauthenticated attach path.
5. Remove dead dependencies once a planned product surface is no longer in live code. Otherwise docs and reviews will keep reasoning about ghosts.
