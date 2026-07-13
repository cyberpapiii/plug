---
title: "Serialize resource-subscription transitions and reconcile the confirmed owner"
date: "2026-07-13"
category: architecture-patterns
module: plug-core/proxy
problem_type: architecture_pattern
component: tooling
severity: high
applies_when:
  - "A local registry represents remote state changed by asynchronous subscribe and unsubscribe calls"
  - "Multiple downstream clients share one upstream subscription"
  - "Routing can move a resource between upstream owners while requests are in flight"
  - "Caller cancellation must not abandon remote-state transitions"
tags:
  - resource-subscriptions
  - async-transitions
  - owner-reconciliation
  - concurrency
  - lifecycle
  - mcp
  - rust
---

# Serialize resource-subscription transitions and reconcile the confirmed owner

## Context

Resource subscription state spans two systems: Plug records downstream members locally, while an upstream MCP server owns the actual `resources/subscribe` state. A local `HashSet` plus first-subscriber/last-subscriber calls is not enough once subscribe, unsubscribe, disconnect cleanup, and route refresh can overlap. The local registry can look active while the remote call failed, a cancelled request can abandon a transition, or a route refresh can send an unsubscribe to the server that owns the route now instead of the server that actually accepted the subscription.

Plug therefore treats each resource URI as a state machine. [`SubscriptionRegistry`](../../../plug-core/src/proxy/subscriptions.rs) stores a generation, downstream members, a `Pending`, `Active`, or `Draining` state, and the server ID that last confirmed the upstream subscription. It also owns persistent per-URI transition locks, so every upstream subscribe, unsubscribe, prune, and rebind for one URI is ordered without holding a synchronous `DashMap` guard across an await.

This document supersedes the deleted historical learning `resource-subscribe-forwarding-lifecycle-20260307.md`. Git history preserves the original protocol-forwarding record, but its `DashMap::retain` cleanup and insert-then-rollback design no longer describe current `main`.

## Guidance

Model the registry as a **generation-checked transition coordinator**, not as a collection that happens to surround remote calls.

1. **Separate synchronous state mutation from asynchronous remote work.** Decide the entry generation, membership, and next state under the `DashMap` guard, release that guard, then await the upstream call under the URI's async transition lock. [`subscribe`, `unsubscribe`, `cleanup_target`, `prune`, and `rebind`](../../../plug-core/src/proxy/subscriptions.rs) all follow this boundary. A synchronous map guard is never held across an await.

2. **Give every logical replacement a new generation.** A transition may remove or activate an entry only if its captured generation still matches. If a new subscriber replaces a draining entry, the older drain can finish remotely but cannot delete the newer local entry. Equivalent rebinds to the same owner share the pending generation and its authoritative result instead of racing separate migrations.

3. **Detach transition ownership from request ownership.** The registry runs upstream transitions in tasks owned by the engine's task tracker, with a detached Tokio task as the teardown/test fallback. Callers await a `watch` result, but dropping the caller does not cancel the transition or release ordering early. This is what lets a cancelled first subscriber or last unsubscriber leave the registry in a coherent state rather than stranding a lock or half-completed remote operation.

4. **Record identity, resolve capability late.** An active entry stores the confirmed `owner_server_id`, never an upstream handle. A drain resolves that ID to the current live handle at drain time and prefers it over a route-cache-derived fallback. This avoids retaining retired connections and prevents an unsubscribe racing a route refresh from targeting the wrong server.

5. **Reconcile confirmed ownership against published routing.** [`classify_route_changes`](../../../plug-core/src/proxy/subscriptions.rs) compares an entry's confirmed owner with the new resource route. A vanished route becomes a prune; an ownership change becomes a rebind. [`ToolRouter::refresh_tools`](../../../plug-core/src/proxy/mod.rs) serializes classify, prune, snapshot publish, and rebind across overlapping refreshes, then runs a detached post-publish sweep to catch entries that confirmed inside the classify-to-publish window.

6. **Heal from the transition, then verify from the caller.** A successful subscribe transition invokes a post-confirm hook that checks its confirmed owner against the published route and spawns a rebind when they differ. Because the hook fires from the detached transition task, downstream cancellation cannot suppress the heal. The caller also performs a one-shot owner check and returns success only when its target is still a member of a confirmed `Active` entry on the published owner (or the documented active routeless grace case).

7. **Make migration failure explicit.** A rebind unsubscribes the confirmed old owner before subscribing the new owner. If the old unsubscribe fails, Plug skips the new subscribe to avoid a dual subscription. If the new owner is missing, lacks subscription support, or rejects the subscribe, the matching generation is removed and waiters receive an error rather than a false success.

## Why This Matters

The invariant is not merely "the map contains this client." It is: **each acknowledged downstream member is backed by a confirmed upstream subscription on the owner Plug currently publishes, or by the documented active routeless grace case, or it receives an error it can retry**. Generations prevent stale completions from overwriting newer intent, per-URI locks preserve remote call order, and recorded owner IDs connect local state to the remote system that actually accepted it.

Without all three, individually reasonable fixes reopen another race. Rollback alone does not order subscribe against a preceding unsubscribe. A lock owned by the request is still lost when the request is cancelled. Route-diff reconciliation alone misses a subscription created inside the refresh's classify-to-publish window. The registry, refresh coordinator, post-confirm heal, and final membership verification form one lifecycle protocol.

## When to Apply

Use this pattern when local state represents a shared remote lease, listener, watch, or subscription and any of these are true:

- the first local member creates remote state and the last member removes it;
- remote calls are asynchronous, cancellable, or can fail independently;
- ownership can move because of reload, failover, discovery, or route refresh;
- cleanup can race creation, or a new member can arrive while cleanup is draining;
- callers need an honest acknowledgement rather than eventual best effort.

Keep simpler insert-and-rollback logic only when operations cannot overlap, request cancellation cannot interrupt ownership, and the remote owner cannot change. Those conditions do not hold for a long-running MCP multiplexer.

## Examples

The core transition shape in [`subscriptions.rs`](../../../plug-core/src/proxy/subscriptions.rs) is:

```text
under map guard:
  choose generation and Pending/Draining state
  update membership
release map guard

in detached task, under persistent per-URI async lock:
  re-check generation
  perform upstream call
  record confirmed owner or remove only the matching generation
  publish one authoritative result to waiters
```

A new subscriber that arrives after a last-member drain has begun its upstream unsubscribe replaces the entry with a fresh pending generation. Its subscribe task waits on the same URI lock, so the remote completion order is unsubscribe then subscribe. If replacement happens before the drain's generation check, the obsolete drain performs no remote call and cannot delete the new entry.

A refresh that moves `file:///report` from server A to server B classifies the entry using its confirmed owner A, not only the old route snapshot. Rebind drains A using the recorded owner resolver, subscribes B, and records B only after confirmation. The post-publish sweep and post-confirm hook cover subscriptions that appear or complete during the refresh window.

The deterministic tests in [`subscriptions.rs`](../../../plug-core/src/proxy/subscriptions.rs) cover piggybacked success and failure, subscribe-during-drain ordering, cancellation of the initiating caller, pending-subscribe cleanup, rebind serialization, equivalent rebind failure, recorded-owner drain selection, and owner/route drift classification. They use controlled gates and call-order assertions rather than timing sleeps, so each race is exercised at the intended boundary.

## Related

- [Phase 2A Notification Infrastructure](../integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md) — downstream notification fan-out that consumes the subscription membership snapshot.
- [Phase 2C Resources, Prompts, Pagination](../integration-issues/phase2c-resources-prompts-pagination-20260307.md) — resource discovery and routing context.
- [Phase 2B Progress and Cancellation Routing](../integration-issues/phase2b-progress-cancellation-routing-20260307.md) — adjacent targeted routing and cancellation correlation patterns.
