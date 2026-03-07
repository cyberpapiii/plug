---
title: "Completion Pass-Through Forwarding for MCP Proxy"
category: integration-issues
tags:
  - mcp
  - completion
  - proxy
  - rmcp
  - protocol-compliance
  - routing
module: plug-core/src/proxy
symptom: "Downstream clients never receive tab-completion suggestions for prompt arguments or resource URIs through the multiplexer"
root_cause: "ProxyHandler did not override the ServerHandler::complete() method, falling back to rmcp's default which returns empty CompletionInfo"
date: 2026-03-07
pr: "#28"
phase: "A3 — MCP Spec Compliance Roadmap"
---

# Completion Pass-Through Forwarding for MCP Proxy

## Problem Statement

Downstream MCP clients connected through `plug` could never receive tab-completion suggestions. The `completion/complete` MCP method was not implemented in the proxy — it fell back to rmcp's default `ServerHandler::complete()` which returns an empty `CompleteResult`. Upstream servers that support completions were invisible to downstream clients.

## Investigation Steps

1. Research ranked `completion/complete` as P2-MEDIUM from MCP spec adoption analysis
2. Confirmed rmcp 1.0.0 has full completion support: `CompleteRequestParams`, `CompleteResult`, `CompletionInfo`
3. Verified `ProxyHandler` did not override `complete()` in its `ServerHandler` impl
4. Identified routing strategy: `CompleteRequestParams.ref` is either `ref/prompt` (use `prompt_routes`) or `ref/resource` (use `resource_routes`)
5. Checked all three transports: stdio (ProxyHandler), HTTP (same ProxyHandler), IPC (daemon dispatch) — none handled completion

## Solution

### Root Cause Analysis

The `ServerHandler` trait has a default implementation for `complete()` that returns `CompleteResult::default()` (empty suggestions). Since `ProxyHandler` didn't override it, all completion requests returned empty results regardless of upstream server capabilities.

### Working Solution

**Step 1: Add `ToolRouter::complete_request()`** (`plug-core/src/proxy/mod.rs`)

Routes based on the `ref` field:
- `ref/prompt` → look up in `prompt_routes`, get `(server_id, original_name)`, rewrite the ref to use the original name
- `ref/resource` → look up in `resource_routes`, get server_id

```rust
pub async fn complete_request(
    &self,
    mut params: CompleteRequestParams,
) -> Result<CompleteResult, McpError> {
    let snapshot = self.cache.load();
    let server_id = match &params.r#ref {
        Reference::Prompt(prompt_ref) => {
            let (sid, original_name) = snapshot.prompt_routes
                .get(&prompt_ref.name).cloned()
                .ok_or_else(|| /* InvalidRequest */)?;
            params.r#ref = Reference::for_prompt(original_name);
            sid
        }
        Reference::Resource(resource_ref) => {
            snapshot.resource_routes
                .get(&resource_ref.uri).cloned()
                .ok_or_else(|| /* InvalidRequest */)?
        }
    };
    drop(snapshot);
    // Forward to upstream
    upstream.client.peer().complete(params).await
}
```

**Step 2: Override `complete()` in ProxyHandler**

```rust
fn complete(&self, request: CompleteRequestParams, _context: ...) -> ... {
    async move { self.router.complete_request(request).await }
}
```

**Step 3: Add IPC dispatch** (`plug/src/daemon.rs`)

Added `"completion/complete"` arm that deserializes `CompleteRequestParams` from JSON params and forwards to `tool_router.complete_request()`.

**Step 4: Truthful capability synthesis**

Added `completions` to `synthesized_capabilities()` when at least one upstream advertises it.

### Key Insight — Prompt Name Translation Bug

During code review, a P1 correctness bug was found: the initial implementation forwarded the **prefixed/routed** prompt name (e.g., `servername__my-prompt`) to the upstream server instead of the **original** name (`my-prompt`). The upstream server wouldn't recognize the prefixed name and would return an error.

The fix: extract the original name from `prompt_routes` (which stores `(server_id, original_name)` tuples) and rewrite `params.ref` before forwarding. This matches the pattern used by `get_prompt()` which similarly translates names.

**Lesson:** Any new method that routes via `prompt_routes` must translate the prefixed name back to the original. This is easy to miss because `resource_routes` don't require translation (resource URIs are not prefixed).

## Prevention

- When adding new proxy methods that route via `prompt_routes`, always extract and use the original name (second tuple element)
- Follow the `get_prompt()` pattern exactly: `let (server_id, original_name) = prompt_routes.get(...)`
- Resource routes don't have this issue — URIs pass through unchanged

## Related Documentation

- `docs/solutions/integration-issues/output-schema-stripped-from-tool-definitions-20260307.md` — Phase A2
- `docs/plans/2026-03-07-feat-completion-pass-through-plan.md` — Phase A3 plan
- `docs/research/2026-03-07-mcp-feature-adoption-analysis.md` — P2-MEDIUM priority

### PRs

- PR #28 — Phase A3 completion pass-through
- PR #27 — Phase A2 structured output pass-through
- PR #26 — Phase A1 logging notification forwarding
