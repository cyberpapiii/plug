---
title: HTTP SSE fanout now reuses pre-serialized payloads
date: 2026-03-18
category: integration-issues
status: completed
---

# HTTP SSE fanout now reuses pre-serialized payloads

## Problem

The HTTP/SSE notification path was doing extra work for every connected
session:

- notifications were carried as raw `serde_json::Value`
- the SSE stream serialized the same payload once per client stream
- broadcast fanout first collected session IDs, then did another map lookup per
  target before attempting delivery

That made noisy global notifications more CPU- and allocation-heavy than they
needed to be.

## Solution

- SSE messages are now stored as a pre-serialized shared payload type instead
  of raw JSON values
- the HTTP notification fanout path serializes a notification once when it is
  enqueued, then clones the shared payload handle during broadcast
- the SSE stream now writes the already-serialized payload directly instead of
  calling `serde_json::to_string()` for every client
- session broadcast fanout now walks the live session map once and records the
  minimal follow-up work needed for expired, delivered, queued, or disconnected
  sessions

## Key decisions

- this keeps targeted delivery semantics unchanged; the optimization is about
  representation and fanout shape, not protocol behavior
- the session store still owns slow/closed-client handling and requeue rules
- the new `SseMessage` type keeps JSON parsing available in tests so behavior
  can still be asserted without reaching through internal transport details

## Tests added

- existing targeted reverse-request delivery still passes with serialized SSE
  payloads
- existing session fanout tests still prove slow clients do not block faster
  ones
- existing SSE stream tests still prove priming, message ordering, and
  heartbeat behavior
- full workspace tests pass after the fanout change
