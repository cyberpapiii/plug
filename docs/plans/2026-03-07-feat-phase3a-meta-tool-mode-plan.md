---
title: "feat: phase 3a meta tool mode"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase3a-meta-tool-mode-brainstorm.md
---

# Phase 3A Meta-Tool Mode

## Overview

Build the first Phase 3 ecosystem-alignment tranche: an opt-in meta-tool mode that exposes a very small discovery and invocation surface instead of the full merged tool catalog, while preserving standard mode and adding tool-definition drift detection.

## Problem Statement / Motivation

Phase 2 made the MCP surface real and truthful, but it also means `plug` can now expose a very large tool catalog. Some clients benefit from direct full-tool mode, but others benefit from a discovery-first surface that avoids loading every tool schema up front.

The codebase already has the beginning of this:

- `plug__search_tools` exists
- merged tool snapshots exist
- routing by exact prefixed tool name already works

What is missing is a coherent mode that:

- returns only meta-tools when enabled
- lets clients discover and invoke real tools on demand
- detects upstream tool definition drift so the discovery layer stays trustworthy

## Proposed Solution

### Scope

This phase includes:

- add `meta_tool_mode` config
- when enabled, expose only the meta-tool set via `tools/list`
- implement the minimal meta-tool set:
  - `plug__list_servers`
  - `plug__list_tools`
  - `plug__search_tools`
  - `plug__invoke_tool`
- detect material tool definition changes across refreshes and log/signal them

This phase excludes:

- default-on meta-tool behavior
- rmcp upgrade work
- state/session abstraction work
- any change to standard-mode behavior beyond the necessary branching

### Technical Approach

1. **Config surface**
   Add `meta_tool_mode: bool` to config with default `false`.

2. **Meta-tool-only listing**
   In the shared router layer, branch `tools/list` behavior on `meta_tool_mode`:
   - `false` -> current behavior
   - `true` -> only the meta-tool set

3. **Meta-tool implementations**
   - `plug__list_servers`: returns server IDs and counts
   - `plug__list_tools`: returns routed tool names/descriptions, with optional server filtering
   - `plug__search_tools`: reuse and adapt the existing search implementation
   - `plug__invoke_tool`: routes directly to the requested prefixed tool and returns raw upstream result

4. **Definition change detection**
   Hash the exposed tool definition shape on refresh and compare against prior refreshes. If a definition changed materially, log and emit an internal event so drift is visible.

## System-Wide Impact

- **Interaction graph**
  config -> router listing mode -> meta-tool exposure -> direct routed invocation for `invoke_tool`.

- **Error propagation**
  `invoke_tool` should surface the same routed tool errors as standard mode, not wrap them in a new error model.

- **State lifecycle risks**
  Tool definition hashes must refresh atomically with the tool snapshot so drift detection does not compare against a torn state.

- **API surface parity**
  Meta-tool mode must behave the same on stdio and HTTP because it lives in the shared router layer.

- **Integration test scenarios**
  - meta-tool mode enabled -> only meta-tools are returned
  - standard mode disabled -> full tool list remains unchanged
  - `invoke_tool` calls a real prefixed tool and returns raw result
  - tool definition change logs/emits drift detection

## Acceptance Criteria

- [x] `meta_tool_mode` config exists and defaults to `false`
- [x] Standard mode continues to return the full routed tool list unchanged
- [x] Meta-tool mode returns only the meta-tool set
- [x] `plug__invoke_tool` routes to a real prefixed tool and returns raw result
- [x] Tool definition changes are detected on refresh
- [x] Focused tests cover both modes and the new meta-tool behaviors

## Dependencies & Risks

- Meta-tool mode must not accidentally break existing standard-mode filtering and routing
- `invoke_tool` must not introduce a new interpretation layer around raw results
- Definition drift detection must not compare stale or partially refreshed snapshots

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase3a-meta-tool-mode-brainstorm.md`
- `docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md`
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/config/mod.rs`
