---
title: Legacy SSE pre-initialize notifications are replayed and health checks use lighter liveness probes
date: 2026-03-18
category: integration-issues
status: completed
---

# Legacy SSE pre-initialize notifications are replayed and health checks use lighter liveness probes

## Problem

Two reliability gaps were still open after the earlier SSE hardening work:

- legacy SSE workers dropped notifications that arrived before the initialize
  response, which meant early logging or list-changed signals could disappear
  during session startup
- health checks still called `list_all_tools()`, which forced full tool-surface
  enumeration on every probe even when we only needed a quick liveness answer

## Solution

- the legacy SSE worker now buffers non-response messages received before the
  initialize response and replays them to the handler immediately after startup
  completes
- initialize matching now treats both success responses and error responses as
  terminal request completions
- health checks now use a single `list_tools(None)` request as the liveness
  probe instead of enumerating every paginated tool result

## Key decision

The health probe was reduced to one `list_tools` page instead of introducing a
new custom ping path.

Why:

- `list_tools` is already universal across MCP servers in the current stack
- changing from `list_all_tools` to one page materially reduces control-plane
  amplification without requiring protocol changes or server-specific behavior
- this keeps the tranche focused on reliability and load-shedding rather than a
  larger health-check redesign

## Tests added

- legacy SSE clients now preserve and deliver a logging notification that
  arrives before the initialize response
- the existing endpoint-timeout regression coverage still passes after the
  worker replay change
- full workspace tests pass with the lighter `list_tools(None)` health probe
