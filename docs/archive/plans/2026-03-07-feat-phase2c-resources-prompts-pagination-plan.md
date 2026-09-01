---
title: "feat: phase 2c resources prompts pagination and capability synthesis"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase2c-resources-prompts-pagination-brainstorm.md
---

# Phase 2C Resources, Prompts, Pagination, Capability Synthesis

## Overview

Build the next truthful MCP surface after tools and request-scoped control flow: forward resources and prompts from upstream servers, paginate large tool lists, and synthesize downstream capabilities from actual healthy upstream support.

## Problem Statement / Motivation

Even after Phase 2A and 2B, `plug` still has major truth gaps:

- resources are advertised but stubbed
- prompts mostly return empty defaults
- downstream capabilities are not synthesized from actual upstream server support
- large `tools/list` responses still ignore pagination even though the protocol already supports cursors

That means the transport and routing core is solid, but the next protocol layer is still hollow.

## Proposed Solution

### Scope

This phase includes:

- store upstream `ServerCapabilities` on connection
- synthesize truthful downstream capabilities for tools/resources/prompts
- implement resources forwarding:
  - `resources/list`
  - `resources/read`
  - `resources/templates/list`
- implement prompts forwarding:
  - `prompts/list`
  - `prompts/get`
- implement cursor-based pagination for `tools/list`

This phase excludes:

- meta-tool mode
- rmcp upgrade work
- generic pagination across every list endpoint
- broad resource subscription/update forwarding unless it is required to keep capability synthesis truthful

### Technical Approach

1. **Upstream capability storage**
   Capture and store upstream `ServerCapabilities` during server startup so they are available to routing and initialization responses.

2. **Capability synthesis**
   Replace hardcoded capability defaults in stdio and HTTP initialization with a merged capability view derived from healthy upstream servers:
   - `tools` if any healthy upstream supports tools
   - `resources` if any healthy upstream supports resources
   - `prompts` if any healthy upstream supports prompts
   - `resources.subscribe` only if the forwarding path is actually implemented

3. **Shared resource/prompt router state**
   Extend the shared router layer to maintain merged resource and prompt snapshots plus route maps:
   - routed resource display identity -> upstream server + canonical URI
   - routed prompt name -> upstream server + canonical prompt name

4. **Resources forwarding**
   - fan out `resources/list` and merge results
   - keep canonical `uri`
   - prefix/normalize display name to avoid collisions
   - route `resources/read` via the shared resource route map
   - fan out `resources/templates/list` and merge results

5. **Prompts forwarding**
   - fan out `prompts/list` and merge results
   - prefix prompt names for collision safety
   - route `prompts/get` via the prompt route map

6. **Tool pagination**
   Add cursor-based pagination for `tools/list` on top of the existing tool snapshot:
   - deterministic ordering from the current snapshot
   - opaque cursor encoding
   - graceful restart-from-beginning behavior for invalid/stale cursors

## System-Wide Impact

- **Interaction graph**
  upstream connection -> capabilities stored -> router refresh builds tool/resource/prompt snapshots -> stdio/HTTP initialization reads synthesized capability view -> stdio/HTTP request handlers route to shared forwarding layer.

- **Error propagation**
  Missing routes or unsupported upstream capabilities should return normal MCP errors, not empty successes that look like real data.

- **State lifecycle risks**
  Resource/prompt snapshots must remain coherent with the same upstream refresh path as tools so list/read/get do not diverge.

- **API surface parity**
  Both stdio and HTTP must expose the same resource and prompt behavior because they share the same router layer.

- **Integration test scenarios**
  - two upstream servers expose resources with distinct URIs and merged list returns both
  - `resources/read` routes to the correct upstream by routed identity
  - two upstream servers expose prompts and merged `prompts/list` returns prefixed names
  - `prompts/get` routes correctly and returns upstream content
  - `tools/list` paginates with stable cursor behavior
  - initialization capabilities reflect actual healthy upstream support

## Acceptance Criteria

- [x] Upstream capabilities are stored and used for downstream capability synthesis
- [x] `resources/list`, `resources/read`, and `resources/templates/list` work through the shared router layer
- [x] `prompts/list` and `prompts/get` work through the shared router layer
- [x] `tools/list` supports cursor-based pagination
- [x] stdio and HTTP initialization responses advertise truthful merged capabilities
- [x] Focused tests cover merged forwarding, routing, and pagination

## Dependencies & Risks

- Resource and prompt routing must not drift from the same refresh/snapshot path used for tools
- Capability synthesis must stay truthful if some upstreams are down or unhealthy
- If `resources.subscribe` forwarding is deferred, capability synthesis must omit subscribe support instead of advertising a stub

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase2c-resources-prompts-pagination-brainstorm.md`
- `docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `docs/plans/2026-03-07-feat-phase2a-notification-infrastructure-plan.md`
- `docs/plans/2026-03-07-feat-phase2b-progress-cancellation-routing-plan.md`
- `docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md`
- `docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md`
- `plug-core/src/proxy/mod.rs`
- `plug-core/src/server/mod.rs`
- `plug-core/src/http/server.rs`
