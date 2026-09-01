---
title: HTTP SSE keepalives now preserve session activity and tool inventory empty
  states are explicit
date: 2026-03-18
category: integration-issues
module: plug-core/http
problem_type: integration_issue
summary: HTTP SSE keepalives now preserve session activity and tool inventory empty
  states are explicit
tags:
- sse
- http
- session
- integration-issues
status: completed
---

# HTTP SSE keepalives now preserve session activity and tool inventory empty states are explicit

## Problem

Two small operator-facing gaps remained:

- HTTP SSE keepalive traffic did not itself count as session activity, which
  meant a healthy but otherwise idle stream could still age toward timeout
- `plug tools` text output still collapsed several different empty-runtime
  states into one generic “No live tools found” message

## Solution

- SSE keepalives now come from plug’s own stream loop using SSE comments rather
  than Axum’s opaque keepalive helper
- each keepalive heartbeat updates the HTTP session’s activity timestamp before
  emitting the comment frame
- `plug tools` now distinguishes five empty-state cases in text mode via
  `ToolInventoryEmptyState`:
  - `NoConfiguredServers`
  - `RuntimeUnavailable` (daemon unreachable)
  - `RuntimeInspectionFailed` (daemon reachable but inventory inspection failed)
  - `AllServersUnavailable`
  - `EmptyMergedSet` (configured and reachable, but the merged tool set is empty)

## Key decision

Keepalive emission moved into the stream loop instead of adding a separate
background “touch session” task.

Why:

- it ties activity updates directly to bytes actually emitted on the stream
- stream drop naturally ends the keepalive path when the client disconnects
- it avoids keeping sessions artificially alive after the response body has
  already been dropped

## Tests added

- keepalive comments trigger the heartbeat callback in the SSE stream path
- session store supports direct activity touches for HTTP keepalive use
- tool inventory empty-state classification is covered with a direct truth-table
  unit test
