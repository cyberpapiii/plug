---
title: fix: Canonicalize MCP tool display titles
type: fix
status: completed
date: 2026-03-22
---

# fix: Canonicalize MCP tool display titles

## Overview

`plug` currently guarantees stable prefixed wire names for MCP tools, but it can emit conflicting display metadata. The proxy sets a canonical top-level `title`, while upstream servers and enrichment can still leave `annotations.title` pointing at a different human label. Clients that prefer different display fields then show inconsistent labels for the same merged tool surface.

## Problem Statement / Motivation

The current behavior produces mixed UI output such as `Todoist: Add Filters` beside generic labels like `List Channels` or `Get Documentation`, even when the underlying tool routing is correct. This causes avoidable confusion when users compare the same tool surface across clients.

## Proposed Solution

Add a canonical display-title normalization step in the tool proxy pipeline so every routed tool leaves `plug` with:

- a stable prefixed wire `name`
- a canonical top-level `title`
- a matching `annotations.title`

This work intentionally does not change wire names, tool grouping, or client-specific rendering logic. It narrows the fix to the highest-leverage inconsistency `plug` can control directly.

## Technical Considerations

- Preserve wire-name stability for routing and existing client permission behavior.
- Normalize display metadata after final name selection so title generation uses the final collision-safe name.
- Prefer a single canonical display label format for all merged tools in this pass: `Prefix: Humanized Name`.
- Avoid broad behavior changes to enrichment; keep enrichment as a fallback helper, then overwrite display title fields in the proxy with the canonical value.

## System-Wide Impact

- **Interaction graph**: `refresh_tools()` builds merged tools, applies optional enrichment, assigns final wire `name`, and then currently sets only top-level `title`. This change adds canonical `annotations.title` normalization in the same pipeline before tools are cached and exposed.
- **Error propagation**: Low risk. This is pure metadata shaping on already-fetched tools.
- **State lifecycle risks**: None beyond cached tool inventory refresh.
- **API surface parity**: Only tools are in scope for this plan. Resources, prompts, and templates already rely on top-level titles and are not part of this fix.
- **Integration test scenarios**: Need coverage for tools carrying upstream `annotations.title`, tools without annotation titles, and tools where final display name depends on stripped versus full name selection.

## Implementation Units

### Unit 1: Add canonical display-title normalization for tools

- Goal: Ensure every routed tool gets one canonical display label in both `tool.title` and `annotations.title`.
- Files:
  - `plug-core/src/proxy/mod.rs`
- Patterns to follow:
  - Existing final-name/title assignment in `refresh_tools()`
  - Existing metadata-preservation behavior in `strip_optional_fields()`
- Approach:
  - Introduce a helper that computes the canonical display title from prefix plus the same display source used for the final title.
  - Apply it after final name resolution.
  - Set both top-level `title` and `annotations.title` to that canonical value, overwriting upstream or enrichment-provided annotation titles for merged tools.
- Execution note: test-first
- Verification:
  - Unit tests prove canonical display title is copied into both top-level and annotation title fields.

### Unit 2: Add regression tests for conflicting upstream annotation titles

- Goal: Lock in behavior for upstream servers that ship their own generic annotation titles.
- Files:
  - `plug-core/src/proxy/mod.rs`
- Patterns to follow:
  - Existing proxy tests near `strip_optional_fields_*`
  - Existing tool naming tests in `plug-core/src/tool_naming.rs`
- Approach:
  - Add a proxy-level test using a tool with an upstream annotation title like `List Channels`.
  - Verify the routed tool exposes the canonical prefixed title in both top-level and annotation fields.
  - Add a test for a tool without upstream annotation title to confirm the same canonical result.
- Execution note: test-first
- Verification:
  - Targeted `cargo test -p plug-core` passes for the new cases.

## Acceptance Criteria

- [ ] Routed MCP tools still use stable prefixed wire names.
- [ ] Routed MCP tools expose a canonical top-level `title`.
- [ ] Routed MCP tools expose a matching `annotations.title`.
- [ ] Upstream annotation titles no longer leak conflicting generic display labels after proxy normalization.
- [ ] Targeted `plug-core` tests pass.

## Success Metrics

- Clients that respect MCP `title` or `annotations.title` display the same human label for a given routed tool.
- The merged tool surface no longer mixes prefixed and generic labels due solely to conflicting title metadata from upstreams.

## Dependencies & Risks

- Some clients will still diverge because they are name-first or synthesize display labels client-side.
- This plan intentionally does not solve workspace grouping policy, prefix casing polish, or broader doc drift.

## Sources & References

- Existing proxy title assignment: `/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs`
- Enrichment annotation title fallback: `/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/enrichment.rs`
- Slack upstream conflicting title example: `/Users/robdezendorf/Documents/GitHub/slack-mcp-server/pkg/server/server.go`
- MCP display precedence research:
  - https://modelcontextprotocol.io/specification/2025-11-25/schema
  - https://ts.sdk.modelcontextprotocol.io/documents/server.html
