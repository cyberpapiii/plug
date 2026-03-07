---
title: "Phase 2C resources, prompts, pagination, and capability synthesis"
category: integration-issues
tags:
  - resources
  - prompts
  - pagination
  - capabilities
  - routing
  - stdio
  - http
module: plug-core
date: 2026-03-07
symptom: |
  plug had solid tools, notifications, and progress/cancellation routing, but resources and prompts
  were still hollow surfaces: list endpoints returned empty defaults, read/get routing did not exist,
  initialization capabilities were not synthesized from real upstream support, and tools/list still
  ignored cursor pagination.
root_cause: |
  The shared router snapshot only modeled tools, while resources/prompts were left as transport-level
  stubs. Upstream capabilities were not retained after connect, so stdio/HTTP initialization had no
  truthful capability source. Pagination support existed in rmcp result types but was never applied
  to the merged tool snapshot.
severity: medium
related:
  - docs/brainstorms/2026-03-07-phase2c-resources-prompts-pagination-brainstorm.md
  - docs/plans/2026-03-07-feat-phase2c-resources-prompts-pagination-plan.md
  - docs/solutions/integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md
  - docs/solutions/integration-issues/phase2b-progress-cancellation-routing-20260307.md
  - plug-core/src/server/mod.rs
  - plug-core/src/proxy/mod.rs
  - plug-core/src/http/server.rs
---

# Phase 2C resources, prompts, pagination, and capability synthesis

## Problem

After Phase 2A and 2B, `plug` had a strong tool and control-flow path, but the next MCP surfaces were still not real:

- resources were advertised or stubbed but not forwarded
- prompts returned empty defaults
- initialization capabilities were not merged from upstream truth
- `tools/list` returned one unpaged response regardless of size

That left `plug` operationally solid but still partially hollow from the MCP surface-area perspective.

## Solution

### 1. Store upstream capabilities

`UpstreamServer` now keeps the upstream `ServerCapabilities` captured from the initialization handshake.

That makes capability synthesis a pure read of healthy upstream state instead of another hardcoded transport default.

### 2. Extend the shared router snapshot beyond tools

`ToolRouter` now refreshes and stores:

- merged tools
- merged resources
- merged resource templates
- merged prompts
- canonical resource URI → upstream route map
- routed prompt name → upstream route map

This keeps tools, resources, and prompts on the same snapshot boundary instead of inventing a parallel cache.

### 3. Forward resources and prompts through the shared router layer

Added shared router methods for:

- `list_resources()`
- `list_resource_templates()`
- `read_resource(uri)`
- `list_prompts()`
- `get_prompt(name, arguments)`

Routing rules:

- resource URIs stay canonical
- resource display names are prefixed for collision safety
- prompt names are prefixed for collision safety
- `resources/read` routes by canonical URI
- `prompts/get` routes by prefixed prompt name

### 4. Synthesize truthful downstream capabilities

Both stdio `get_info()` and HTTP initialize responses now read `ToolRouter::synthesized_capabilities()` instead of hardcoding capability defaults.

The synthesized view is derived from healthy upstream servers:

- tools if tools are actually present
- resources if at least one healthy upstream supports resources
- prompts if at least one healthy upstream supports prompts

### 5. Add cursor pagination for tools/list

`ToolRouter::list_tools_page_for_client(...)` now paginates the existing tool snapshot with cursor-based paging and transport handlers call that shared method instead of always returning the full list.

## Verification

Focused tests added:

- `server::tests::router_refreshes_resources_and_prompts_and_routes_reads`
- `proxy::tests::list_tools_page_for_client_uses_cursor_pagination`

Full verification passed:

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Prevention

- Keep new MCP surfaces inside the shared router snapshot instead of transport-local stubs.
- Store upstream capabilities at connect time so downstream initialization stays truthful.
- Route by canonical identity where the protocol already defines one (`uri`), and prefix only the user-facing collision surface.
- Add pagination at the shared snapshot boundary, not separately in each transport handler.
