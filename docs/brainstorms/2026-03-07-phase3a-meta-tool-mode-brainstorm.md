# Brainstorm: Phase 3A Meta-Tool Mode and Tool-Surface Hardening

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The first Phase 3 tranche after Phase 2 is complete: an opt-in meta-tool mode that collapses the visible tool surface to a small discovery interface, plus the remaining tool-definition hardening needed to make that mode safe and trustworthy.

This phase focuses on:

- opt-in meta-tool mode
- a minimal meta-tool set for discovery and invocation
- tool definition change detection / hardening
- keeping standard mode fully intact when meta-tool mode is disabled

It does **not** include the later Phase 3 items yet:

- rmcp upgrade work
- session trait abstraction / stateless prep
- broad new integration-test programs beyond what is needed for meta-tool mode

## Why This Approach

By the end of Phase 2, the core protocol surface is real:

- notifications work
- progress/cancellation work
- resources and prompts work
- capabilities are synthesized truthfully

That means the next real differentiation layer is ecosystem alignment and token discipline. The strategic docs already identified meta-tool mode as the right next product move, but it should stay opt-in and minimal.

There is already a foundation in the codebase:

- `plug__search_tools` exists
- tool filtering and snapshot-based routing already exist
- the shared router can now expose a reduced or alternate surface without transport-specific hacks

So the next clean step is not another protocol feature. It is making the visible tool surface intentionally small for clients that benefit from discovery-first interaction.

## Key Decisions

- **Keep meta-tool mode opt-in.**
  Standard mode remains the default and must keep its current behavior.

- **Build on the existing `plug__search_tools` path.**
  Do not invent an unrelated mechanism when a discovery primitive already exists.

- **Return a very small meta-tool set.**
  Prefer 3-4 tools, not a broad “management API.”

- **Keep `invoke_tool` transparent.**
  It should route to the exact prefixed tool and return the raw result, not a wrapped interpretation layer.

- **Detect tool definition drift.**
  If meta-tool mode is going to push clients toward on-demand tool loading, `plug` should also detect when upstream tool definitions change materially.

- **Do not degrade standard mode.**
  Tool filtering, routing, and direct tool exposure in standard mode must remain unchanged.

## Resolved Questions

- **Should meta-tool mode replace standard mode?** No
- **Should meta-tool mode be enabled by default?** No
- **Should `plug__search_tools` be reused?** Yes
- **Should tool invocation in meta-tool mode wrap or reinterpret results?** No, pass through raw

## Open Questions

- What exact meta-tool set is the smallest useful one:
  - `list_servers`
  - `list_tools`
  - `search_tools`
  - `invoke_tool`
- Should `list_tools` support per-server filtering only, or also free-text filtering?
- How should tool definition change detection be surfaced: log only, event bus, or both?

## Next

Write a focused plan for:

1. config surface for `meta_tool_mode`
2. the minimal meta-tool set and routing behavior
3. tool definition change detection/hardening

Everything else stays deferred until this mode works cleanly in both stdio and HTTP.
