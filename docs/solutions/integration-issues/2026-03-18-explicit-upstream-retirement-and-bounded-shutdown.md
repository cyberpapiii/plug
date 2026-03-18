---
title: Upstream replacement now retires old connections explicitly and engine shutdown stays bounded
date: 2026-03-18
category: integration-issues
status: completed
---

# Upstream replacement now retires old connections explicitly and engine shutdown stays bounded

## Problem

Reconnect and restart cutover still relied too heavily on `Drop` behavior:

- `replace_server()` swapped the routing map but did not explicitly cancel or
  close the retired upstream connection
- if some code still held an `Arc<UpstreamServer>`, `Arc::try_unwrap()` could
  not reclaim ownership and the old upstream could keep running indefinitely
- `engine.shutdown()` spent its entire timeout budget waiting for background
  tasks before beginning upstream teardown, which made mixed-fleet shutdown more
  likely to overrun bounded callers

## Solution

- server retirement is now explicit for stop, replace, and shutdown paths
- every retired upstream gets an immediate cancellation signal
- when ownership is available, plug follows cancellation with
  `close_with_timeout()` to bound graceful teardown
- replacement uses a grace period when the old upstream still has outstanding
  `Arc` holders, so active in-flight work is not cancelled immediately during
  reconnect cutover
- shutdown-all retirement now runs concurrently across the server set rather
  than serially
- engine shutdown now uses a shorter task-drain window before explicit upstream
  retirement so bounded callers do not spend their entire timeout budget before
  teardown begins

## Key decision

Retirement now uses a two-step strategy: cancel first, then gracefully close
only when plug owns the full `RunningService`.

Why:

- cancellation works even when another `Arc<UpstreamServer>` is still alive
- `close_with_timeout()` gives us bounded cleanup when we do own the client
- the replacement grace period preserves the existing zero-downtime intent for
  in-flight work while still preventing indefinite duplicate upstreams
- this avoids a much larger refactor to wrap every upstream client in a mutable
  synchronization primitive purely for teardown control

## Tests added

- replaced upstreams are marked closed even when a lingering `Arc` still exists
- the mixed-auth fleet integration test still completes shutdown within its
  caller-imposed timeout
- full workspace tests pass with the explicit retirement path in place
