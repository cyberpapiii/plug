# Brainstorm: Phase 2C Resources, Prompts, Pagination, Capability Synthesis

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next Phase 2 implementation tranche after progress/cancellation routing: make resources and prompts truthful and usable, add pagination for large tool lists, and synthesize downstream server capabilities from actual upstream support.

This tranche focuses on:

- resources forwarding (`list`, `read`, `templates/list`)
- prompts forwarding (`list`, `get`)
- truthful merged capabilities for tools/resources/prompts
- cursor-based pagination for `tools/list`

It does **not** include meta-tool mode, rmcp upgrades, or broader Phase 3 work yet.

## Why This Approach

The current behavior is still materially misleading:

- stdio and HTTP both advertise resources support, but return empty lists
- prompts are not advertised, but `list_prompts` still returns an empty success path
- capabilities are not synthesized from upstream truth
- large tool sets still return one unpaged response even though the protocol already supports pagination

After Phase 2A and 2B, the request/notification substrate is strong enough. The next gap is not transport control flow anymore; it is truthful forwarding of the next major MCP feature surfaces.

## Key Decisions

- **Make capability advertisement truthful.**
  Do not keep advertising stubbed resource behavior once forwarding is added. Capabilities should reflect healthy upstream support, not defaults.

- **Route resources/prompts through `ToolRouter`’s shared pattern, not one-off transport code.**
  HTTP and stdio should use the same merged query/routing layer, with thin transport handlers on top.

- **Preserve canonical resource URIs.**
  Resource `uri` should remain the upstream URI. Any multiplexing disambiguation should happen on display name/metadata, not by rewriting the URI.

- **Prefix prompt names and resource display names where needed.**
  The user-facing routed identity must stay collision-safe, just like tools.

- **Keep subscription/update forwarding deferred unless needed to make resources truthful.**
  If resource `subscribe` cannot be made coherent in this tranche without over-scope, capability synthesis should simply omit it.

- **Add pagination only to `tools/list` now.**
  Resources/prompts already use paginated result types, but Phase 2C should not broaden into generic pagination machinery for every list endpoint unless necessary.

## Resolved Questions

- **Should this start before progress/cancellation lands?** No
- **Should capability synthesis wait for Phase 3?** No
- **Should URIs be prefixed?** No
- **Should prompts/resources be another broadcast-notification project?** Not yet

## Open Questions

- What is the smallest routing table addition needed for resources and prompts without bloating the tool snapshot?
- Should `resources/subscribe` and `resources/updated` land in this tranche or remain deferred with truthful capability omission?
- How should pagination cursors be encoded so they survive cache rebuilds without becoming brittle?

## Next

Write a focused plan for:

1. storing upstream capabilities and synthesizing truthful downstream capabilities
2. resources and prompts forwarding through the shared router layer
3. tool-list pagination on top of the existing snapshot model

Everything else stays deferred until these core feature surfaces are real.
