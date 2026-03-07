---
title: "feat: Completion Pass-Through"
type: feat
status: completed
date: 2026-03-07
parent: MCP Spec Compliance Roadmap (Stream A)
---

# feat: Completion Pass-Through (Phase A3)

## Overview

Forward `completion/complete` requests from downstream clients to the correct upstream server based on the reference type (prompt name or resource URI). Advertise the `completions` capability truthfully when at least one upstream supports it.

## Problem Statement / Motivation

The MCP spec includes `completion/complete` — a utility that allows clients to request tab-completion suggestions for prompt arguments and resource URI templates. plug currently does not implement this handler, falling back to the rmcp default which returns an empty `CompleteResult`. This means downstream clients connected through plug never get completion suggestions, even when upstream servers support them.

Research ranks this P2-MEDIUM (low-moderate adoption but trivial pass-through complexity). The feature is already supported in rmcp 1.0.0 with `CompleteRequestParams`, `CompleteResult`, and `CompletionInfo` types.

## Proposed Solution

**Route `completion/complete` requests to the correct upstream server based on the `ref` field:**
- `ref/prompt` → look up prompt name in `prompt_routes`, forward to that upstream
- `ref/resource` → look up resource URI in `resource_routes`, forward to that upstream

### Architecture

```
Downstream Client ──completion/complete──→ ProxyHandler.complete()
                                                  ↓
                                          ToolRouter.complete_request()
                                                  ↓
                                          Match ref type:
                                          ├── ref/prompt  → prompt_routes lookup → upstream.complete()
                                          └── ref/resource → resource_routes lookup → upstream.complete()
                                                  ↓
                                          CompleteResult passed back unchanged
```

### Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Routing strategy | Use existing prompt_routes and resource_routes | Completion refs use the same names/URIs as prompts and resources |
| Capability advertisement | Truthful synthesis | Only advertise `completions` when at least one upstream declares it |
| IPC support | Add `completion/complete` dispatch | Same pattern as resources/read and prompts/get |
| Error handling | Return MCP error for unknown refs | Consistent with read_resource and get_prompt patterns |

## Implementation Tasks

### Step 1: Add `complete_request()` to ToolRouter

**File:** `plug-core/src/proxy/mod.rs`

- [x] Add `pub async fn complete_request(&self, params: CompleteRequestParams) -> Result<CompleteResult, McpError>`
- [x] Route based on `params.ref`: `Reference::Prompt` → prompt_routes, `Reference::Resource` → resource_routes
- [x] Forward the full `CompleteRequestParams` to the upstream server's `peer().complete()` method
- [x] Handle errors consistently with `read_resource()` and `get_prompt()` patterns

### Step 2: Override `complete()` in ProxyHandler's ServerHandler impl

**File:** `plug-core/src/proxy/mod.rs`

- [x] Add `fn complete()` override in `impl ServerHandler for ProxyHandler`
- [x] Delegate to `self.router.complete_request(request).await`

### Step 3: Add `completion/complete` to daemon IPC dispatch

**File:** `plug/src/daemon.rs`

- [x] Add `"completion/complete"` arm in `dispatch_mcp_request()`
- [x] Deserialize `CompleteRequestParams` from params JSON
- [x] Call `tool_router.complete_request(params).await`
- [x] Serialize result to IPC response (same pattern as prompts/get)

### Step 4: Truthful capability synthesis

**File:** `plug-core/src/proxy/mod.rs`

- [x] In `synthesized_capabilities()`, add: if any upstream has `completions.is_some()`, set `capabilities.completions = Some(serde_json::Map::new())`

### Step 5: Tests

**File:** `plug-core/src/proxy/mod.rs`

- [x] Add test: `synthesized_capabilities_includes_completions_when_upstream_supports_it`
- [x] Add test: `complete_request_params_serde_roundtrip` (verify IPC transport path)

### Step 6: Quality checks

- [x] `cargo check` passes
- [x] `cargo test` passes
- [x] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [x] `cargo fmt --check` passes

## System-Wide Impact

- **Interaction graph**: `complete()` on ProxyHandler → `complete_request()` on ToolRouter → route lookup → upstream `peer().complete()` → result returned unchanged
- **Error propagation**: Unknown ref → MCP error (consistent with existing patterns). Upstream error → forwarded as-is
- **State lifecycle risks**: None. Completion is stateless — no subscriptions, no bookkeeping
- **API surface parity**: All three transports (stdio, HTTP, IPC) will support completion after this change

## Acceptance Criteria

- [x] `completion/complete` with `ref/prompt` routes to correct upstream and returns suggestions
- [x] `completion/complete` with `ref/resource` routes to correct upstream and returns suggestions
- [x] Unknown prompt/resource refs return appropriate MCP error
- [x] `completions` capability advertised truthfully in `synthesized_capabilities()`
- [x] IPC proxy supports `completion/complete`
- [x] All quality checks pass

## Dependencies & Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| No upstream servers support completions | High | None | Returns empty results or errors — no breakage |
| Prompt/resource name mismatch in ref | Low | Low | Use same route lookup as get_prompt/read_resource |

## Sources & References

### Internal References

- `ToolRouter::get_prompt()`: `plug-core/src/proxy/mod.rs:994` — pattern to follow
- `ToolRouter::read_resource()`: `plug-core/src/proxy/mod.rs:965` — pattern to follow
- `synthesized_capabilities()`: `plug-core/src/proxy/mod.rs:868`
- `dispatch_mcp_request()`: `plug/src/daemon.rs:907`
- `ProxyHandler ServerHandler impl`: `plug-core/src/proxy/mod.rs:2032-2075`

### rmcp Types

- `CompleteRequestParams` — has `ref: Reference` (prompt or resource) + `argument: ArgumentInfo`
- `CompleteResult` — has `completion: CompletionInfo` (values, total, has_more)
- `Reference` — enum: `Resource(ResourceReference)` | `Prompt(PromptReference)`
- Client SDK: `peer().complete(params)` method

### Research References

- Feature adoption analysis: `docs/research/2026-03-07-mcp-feature-adoption-analysis.md` — P2-MEDIUM priority
