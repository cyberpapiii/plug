---
title: "Phase 3A meta-tool mode with routed invocation and tool-definition drift detection"
category: integration-issues
tags:
  - meta-tools
  - routing
  - token-efficiency
  - tool-discovery
  - transport-parity
  - drift-detection
  - stdio
  - http
module: plug-core
date: 2026-03-07
symptom: |
  After Phase 2, plug exposed the full merged tool catalog to every client. That made the
  protocol surface truthful, but also meant high-cardinality tool sets were pushed eagerly to
  clients that would benefit from a discovery-first surface. The codebase had `plug__search_tools`
  but not a coherent mode that hid the full catalog, invoked hidden tools safely, or detected when
  routed tool definitions changed materially between refreshes.
root_cause: |
  The shared router treated the visible tool list and the canonical routed tool snapshot as the same
  thing. There was no alternate exposed surface for `tools/list`, no first-class meta-tool set, and
  no fingerprinting of routed tool definitions at refresh time. That made discovery-first operation
  awkward and left no signal when upstream servers changed the schema or semantics of an existing
  routed tool.
severity: medium
related:
  - docs/brainstorms/2026-03-07-phase3a-meta-tool-mode-brainstorm.md
  - docs/plans/2026-03-07-feat-phase3a-meta-tool-mode-plan.md
  - docs/solutions/integration-issues/phase2c-resources-prompts-pagination-20260307.md
  - docs/solutions/integration-issues/phase3-resilience-token-efficiency.md
  - plug-core/src/config/mod.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/server/mod.rs
  - plug-core/src/http/server.rs
---

# Phase 3A meta-tool mode with routed invocation and tool-definition drift detection

## Problem

The shared router already knew how to merge and route tools, but it still assumed every client
should see the whole tool catalog up front. That caused two product gaps:

1. no opt-in discovery-first mode for large tool sets
2. no explicit signal when an existing routed tool changed shape across refreshes

The important design constraint was that standard mode could not regress. The fix had to live in the
shared router so stdio and HTTP inherited the same behavior automatically.

## Solution

### 1. Split canonical routed tools from the exposed tool surface

`RouterSnapshot` now stores:

- the full routed tool catalog (`tools_all`)
- a dedicated meta-tool surface (`meta_tools_all`)
- fingerprints for exposed routed tool definitions

That keeps canonical routing intact while allowing `tools/list` to expose a different surface when
`meta_tool_mode` is enabled.

### 2. Add an opt-in `meta_tool_mode` config flag

`Config` and `RouterConfig` now carry `meta_tool_mode: bool`, defaulting to `false`.

When enabled:

- `tools/list` returns only the meta-tools
- standard routing still exists underneath for direct invocation
- standard mode behavior remains unchanged when disabled

### 3. Introduce a minimal meta-tool set

The meta-tool surface is intentionally small:

- `plug__list_servers`
- `plug__list_tools`
- `plug__search_tools`
- `plug__invoke_tool`

The key design choice was to keep `plug__invoke_tool` transparent. It forwards to the exact routed
tool name and returns the raw upstream result instead of inventing a wrapper protocol.

### 4. Reuse the existing router instead of adding a side channel

The implementation stays in `ToolRouter::call_tool_inner()`:

- intercept meta-tool names first
- keep all other routing logic unchanged
- reuse the same downstream context, progress-token, timeout, reconnect, and cancellation path for
  `plug__invoke_tool`

That avoided duplicating resilience behavior for a new invocation pathway.

### 5. Detect material tool-definition drift during refresh

At refresh time, the router now fingerprints the visible routed tool definition shape:

- name
- description
- title
- input schema
- annotations

When a tool with the same routed name changes fingerprint, `plug` now:

- logs the drift
- emits `EngineEvent::ToolDefinitionDriftDetected`

This is intentionally limited to same-name definition changes. Adds/removes are already covered by
tool-list refresh behavior.

## Verification

The tranche was verified at three levels:

1. router/unit coverage
   - meta-tool mode lists only the meta-tool set
   - drift detection isolates changed tools
2. stdio integration
   - downstream `tools/list` returns only meta-tools
   - `plug__invoke_tool` successfully invokes a hidden routed tool end to end
3. HTTP integration
   - session-bound `tools/list` returns the same meta-tool surface over Streamable HTTP

Final quality gate:

- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`

## Prevention / Reuse

For later Phase 3 work:

- keep alternative client surfaces in the shared router, not in per-transport forks
- treat canonical routed state and exposed client state as separate concerns
- prefer direct reuse of the normal invocation pipeline for meta-tools that call real tools
- detect drift at refresh boundaries, where snapshots are already rebuilt atomically

The main reusable lesson is that discovery-first UX does not require a second routing system. It
requires a smaller exposed surface on top of the same canonical router.
