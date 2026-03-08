---
title: Resource subscribe parity and cleanup closeout
date: 2026-03-07
category: integration-issues
components:
  - plug-core/src/proxy/mod.rs
  - plug-core/src/http/server.rs
  - plug-core/src/session/stateful.rs
  - plug-core/src/ipc.rs
  - plug/src/daemon.rs
  - plug/src/ipc_proxy.rs
  - plug/src/runtime.rs
problem_type: protocol-parity-closeout
summary: Finished the roadmap tail by adding `resources/subscribe` and `resources/unsubscribe` with targeted `notifications/resources/updated`, including daemon-backed `plug connect` parity via daemon IPC notification delivery, plus dead dependency cleanup.
related:
  - docs/brainstorms/2026-03-07-roadmap-tail-closeout-brainstorm.md
  - docs/plans/2026-03-07-feat-roadmap-tail-closeout-plan.md
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

### 1. Add subscription bookkeeping to `ToolRouter`

In [plug-core/src/proxy/mod.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug-core/src/proxy/mod.rs), `ToolRouter` now tracks:

- canonical resource URI -> downstream subscriber targets
- downstream target -> subscribed resource URIs

That lets `plug`:
- call upstream `subscribe` only on the first local subscriber
- call upstream `unsubscribe` only when the last local subscriber goes away
- route `notifications/resources/updated` only to the subscribing targets

The downstream target model now includes daemon-backed sessions too:

- `NotificationTarget::Stdio`
- `NotificationTarget::Http`
- `NotificationTarget::Ipc`

### 2. Keep direct stdio and HTTP thin

Direct stdio and HTTP do not own subscription state.

They only:
- forward `resources/subscribe`
- forward `resources/unsubscribe`
- deliver targeted resource-update notifications using the same internal notification bus already used for tool-list changes, progress, and cancellation

That keeps the router as the only place that knows about reference counting and upstream transition rules.

### 3. Add daemon-backed parity with a daemon notification channel

Daemon-backed `plug connect` needed real push delivery, not capability masking.

The fix was:

- extend [plug-core/src/ipc.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug-core/src/ipc.rs) with:
  - `AttachNotifications`
  - `McpNotification`
  - protocol version bump to `3`
- add a daemon-side notification hub in [plug/src/daemon.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug/src/daemon.rs)
- allow `IpcProxyHandler` to attach a dedicated daemon notification stream in [plug/src/runtime.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug/src/runtime.rs)
- run a self-healing notification supervisor in [plug/src/ipc_proxy.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug/src/ipc_proxy.rs) that forwards MCP notifications back to the downstream stdio client

This preserved product parity without rewriting the primary IPC request path.

### 4. Bind notification attach to the owning client

`AttachNotifications` now requires both:
- `session_id`
- `client_id`

The daemon verifies the session belongs to that client before replacing the notification sink. That prevents another IPC connection from stealing a live client’s notification stream just by knowing the session UUID.

### 5. Replay subscriptions after daemon session replacement

Daemon reconnects replace the logical session ID.

Without extra work, that would silently drop all resource subscriptions because the daemon cleans up subscriptions when the old session disappears.

`IpcProxyHandler` now keeps a local set of subscribed resource URIs and replays them onto the replacement session after reconnect, before restarting the notification attachment.

### 6. Clean up HTTP expiry correctly

The original HTTP teardown only handled:
- explicit `DELETE /mcp`
- opportunistic cleanup when a later notification hit a dead session

That still leaked subscriptions for naturally expired sessions.

[plug-core/src/session/stateful.rs](/Users/robdezendorf/.config/superpowers/worktrees/plug/feat-roadmap-tail-closeout/plug-core/src/session/stateful.rs) now supports a removal hook, and the HTTP runtime registers router cleanup once when it constructs the session store. That means:

- explicit delete
- validation-time expiry
- background cleanup expiry

all converge on the same router subscription teardown path.

### 7. Remove dead TUI dependencies

The workspace manifest no longer carries:
- `ratatui`
- `crossterm`
- `color-eyre`

The local review context and crate-stack doc were updated to match the live codebase instead of the old planned TUI surface.

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
4. When adding notification delivery to an existing IPC protocol, tie attachment to the owner identity, not just a bare session UUID.
5. Remove dead dependencies once a planned product surface is no longer in live code. Otherwise docs and reviews will keep reasoning about ghosts.
