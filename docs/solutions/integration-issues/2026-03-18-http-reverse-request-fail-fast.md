---
title: HTTP reverse requests now fail fast when no live SSE consumer exists
date: 2026-03-18
category: integration-issues
status: completed
---

# HTTP reverse requests now fail fast when no live SSE consumer exists

## Problem

Reverse requests to downstream HTTP sessions still allocated timeout-backed
pending state even when there was no live SSE consumer to receive them.

That meant:

- `pending_client_requests` entries were created before delivery viability was
  known
- disconnected-but-not-yet-expired HTTP sessions could silently enqueue reverse
  work for minutes
- repeated reverse requests accumulated avoidable oneshots, map entries, and
  timers instead of failing immediately

## Solution

- the session store now exposes an explicit “live SSE sender present” check
- `send_http_client_request()` gates reverse-request allocation on that liveness
  check before inserting into `pending_client_requests`
- a second liveness check runs immediately before enqueue to cover the small
  race window between request construction and send
- dead/no-sender HTTP sessions now return an immediate internal transport error
  instead of allocating timeout-backed pending state

## Key decision

The fail-fast behavior is limited to sessions with no live SSE sender, not
merely slow clients.

Why:

- slow but connected clients still have a valid delivery path
- the bug was specifically wasted state allocation for sessions that could not
  possibly answer
- this keeps the fix tight to the dead-target case without changing timeout
  behavior for legitimate in-flight reverse requests

## Tests added

- disconnected HTTP sessions fail fast without growing pending reverse-request
  state
- connected HTTP sessions still complete reverse requests normally
- session-store liveness detection is covered directly
- full workspace tests pass after the fail-fast change
