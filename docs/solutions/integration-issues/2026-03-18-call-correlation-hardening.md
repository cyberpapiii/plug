---
title: Call correlation hardening for reverse requests, progress, and cancellation
date: 2026-03-18
category: integration-issues
status: completed
---

# Call correlation hardening for reverse requests, progress, and cancellation

## Problem

The router previously treated several multiplexed behaviors as if one upstream server implied one downstream owner:

- reverse requests resolved ownership by `server_id`
- downstream cancel could be dropped before the upstream request id was attached
- progress lookup used the downstream token directly, so concurrent calls could collide

That was safe only in the single-active-call case.

## Solution

- reverse-request routing now prefers the concrete upstream request id from `RequestContext`
- active call records store both downstream and upstream progress tokens, plus a pending cancel slot
- upstream progress is keyed by the unique upstream token and rewritten back to the downstream token before fanout
- if a downstream cancel arrives before the upstream request id exists, the reason is stored and forwarded as soon as the id is attached

## Important decision

The implementation keeps a fallback to the old “single active call on this server” behavior when a reverse request arrives before the request-id mapping has been attached.

Why:

- existing stdio reverse-request flows can emit early enough that strict request-id-only lookup regresses working behavior
- the fallback preserves today’s single-call semantics while still making the concurrent case correct once the mapping exists

This is intentionally transitional, not the final shape. The long-term target is to remove the fallback once transport sequencing guarantees the mapping before reverse requests can arrive.

## Tests added

- upstream request lookup resolves by request id even when two clients share one server
- upstream progress notifications restore the downstream token before fanout
- pending cancel state survives request-id attachment
- upstream progress lookup stores only the rewritten upstream token
