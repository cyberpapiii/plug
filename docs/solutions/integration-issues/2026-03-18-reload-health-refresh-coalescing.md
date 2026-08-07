---
title: Reload startup is now batched and health refreshes are coalesced
date: 2026-03-18
category: integration-issues
module: plug-core/reload
problem_type: integration_issue
summary: Two related control-plane costs were still higher than they needed to be
tags:
- reload
- health
- integration-issues
status: completed
---

# Reload startup is now batched and health refreshes are coalesced

## Problem

Two related control-plane costs were still higher than they needed to be:

- health transitions called `refresh_tools().await` directly, so flap bursts
  could trigger repeated full merged-surface rebuilds
- proactive recovery tasks could be spawned repeatedly for the same failed
  server while an existing recovery attempt was still running
- reload startup processed changed and added servers one by one, so reload
  latency scaled linearly with the slowest touched upstreams

## Solution

- health transitions now go through the router’s debounced
  `schedule_tool_list_changed_refresh()` path instead of doing eager refreshes
- the engine now keeps a per-server recovery-task claim flag so health loops
  only launch one proactive recovery task at a time for a given server
- reload now separates “stop changed servers” from “start changed and added
  servers”, then starts the touched servers in a bounded concurrent batch
- reload still records every start/restart failure and preserves `Failed` /
  `AuthRequired` visibility before doing the final config swap
- after the swap, catalog refresh (`refresh_tools`) runs only when the upstream
  server set changed (`added` / `changed` / `removed`). Tool-shaping fields
  (prefix, filters, priority/disabled lists, lazy-tools policy, etc.) are
  restart-required and do not update live `RouterConfig`, so refreshing on
  those alone would rebuild routes with stale shaping

## Key decisions

- concurrency is bounded rather than unbounded
- changed servers are still stopped before replacement startup begins
- config swap remains single-shot after the batch so operator truth and
  background task ownership stay stable
- catalog rebuild is gated on server-set dirtiness, not every reload

This keeps the semantics conservative while removing the worst serialized
control-plane work.

## Tests added

- bounded reload startup helper proves concurrency is capped and actually
  exercised
- recovery-task claims are deduplicated until the active recovery releases
- existing reload failure visibility coverage still passes
- existing router debounce coverage now protects the health-triggered refresh
  path too
- full workspace tests pass after the change

## Related

- [Backoff reset requires sustained recovery](../design-patterns/backoff-reset-requires-sustained-recovery.md) — a companion
  health-loop-recovery principle from the later active-supervision work: a
  restart/backoff governor's reset must be gated on *sustained* recovery, because
  the restart action itself transiently restores the healthy signal. Same domain
  (don't let a transient health signal re-trigger recovery work), different
  primitive (backoff-reset timing here vs. debounce + recovery-task dedup above).
