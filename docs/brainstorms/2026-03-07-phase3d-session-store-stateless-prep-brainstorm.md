# Brainstorm: Phase 3D Session Store Abstraction and Stateless Prep

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next Phase 3 tranche is the architectural seam for future stateless downstream support:

- a top-level `SessionStore` trait
- a concrete `StatefulSessionStore` that preserves current HTTP behavior
- design notes describing how a future stateless implementation would plug into the system

This tranche does **not** implement stateless MCP handling.

## Why This Approach

The product now has real downstream session behavior, but it is still embedded directly inside the
HTTP module as one concrete in-memory implementation. The strategic plan already called out that the
June 2026 MCP direction is stateless-first, so the next right move is not to prematurely implement
stateless transport. It is to put the seam in the right place now.

That keeps the current behavior stable while making the architecture honest about where an
alternative session strategy would integrate later.

## Key Decisions

- **Define the trait now, keep only one implementation.**
  No extra backends yet.

- **Use the existing HTTP session behavior as `StatefulSessionStore`.**
  This is a refactor, not a rewrite.

- **Thread the trait through the HTTP state boundary.**
  That proves the abstraction is real instead of dead code.

- **Keep stateless work design-only.**
  Document entry points and constraints, but do not implement request handling for it yet.

## Resolved Questions

- **Should stateless MCP be implemented now?** No
- **Should the trait live at top-level instead of under `http`?** Yes
- **Should the HTTP server depend on the trait boundary?** Yes

## Open Questions

None. The tranche is narrow enough to move directly to planning.

## Next

Write a focused plan for:

1. introducing the top-level session module and trait
2. moving current HTTP session behavior into `StatefulSessionStore`
3. switching HTTP runtime wiring to the trait boundary
4. writing stateless design notes only
