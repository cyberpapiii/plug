---
title: Reload startup is now batched and health refreshes are coalesced
date: 2026-03-18
category: integration-issues
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
  `AuthRequired` visibility before doing the final config swap and refresh

## Key decisions

- concurrency is bounded rather than unbounded
- changed servers are still stopped before replacement startup begins
- config swap and downstream refresh remain single-shot after the batch
  completes so operator truth and background task ownership stay stable

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
