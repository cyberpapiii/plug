---
title: Reload topology now rebuilds health and refresh task ownership
date: 2026-03-18
category: integration-issues
status: completed
---

# Reload topology now rebuilds health and refresh task ownership

## Problem

Hot reload previously updated server processes and config snapshots, but it did
not rebuild the runtime maintenance topology to match.

That caused two classes of drift:

- newly added or changed servers could miss health checks or refresh loops
- failed reloads could remove a server from operator truth instead of leaving it
  visible as failed/auth-required

## Solution

- reload now preserves `Failed` / `AuthRequired` status for start failures
- per-server background maintenance uses generation tracking so replacement tasks
  supersede older ones safely
- reload clears generations for removed servers
- new health/refresh tasks are spawned only after the new config snapshot is
  stored, so they read the correct server set immediately
- `health_check_interval_secs` now counts as a material server-config change

## Key decision

Generation-based task ownership was chosen instead of trying to surgically
cancel individual Tokio tasks.

Why:

- the existing shutdown model already uses one shared `CancellationToken`
- generation checks let old tasks self-retire without introducing per-task
  cancellation wiring across the whole engine
- this keeps the fix local to reload/runtime maintenance rather than turning it
  into a task-orchestration rewrite

## Tests added

- failed added servers remain visible in `server_statuses()` after reload
- changing only `health_check_interval_secs` marks a server as changed in the
  reload diff
