---
date: 2026-04-23
topic: lazy-tool-discovery-v2
---

# Lazy Tool Discovery V2

## Problem Frame

`plug`'s current meta-tool mode proves that a reduced discovery surface is possible, but its shape is too thin and too unnatural to serve as the modern long-term lazy-tool story. It returns a text search surface, keeps discovered tools behind a wrapper-style invocation path, and does not align well with the direction emerging in OpenCode or the deferred loading model already proven by OpenAI/Codex-style tool search.

The v2 redesign should keep the useful insight from the existing mode, but zoom out and reframe the product shape: `plug` should own a real lazy-discovery engine with a durable session working set, while presenting the best client-facing surface per client target. The goal is not to maximize one client's illusion at all costs. The goal is a strong modern cross-client design that still feels normal and low-friction inside each client.

---

## Actors

- A1. Plug operator: links clients, reviews the chosen mode, and overrides defaults when needed.
- A2. AI client: connects to `plug`, discovers tools, loads a working set, and calls tools during a session.
- A3. Plug runtime: chooses the client-facing lazy-discovery shape, manages the session working set, and routes real tool calls.

---

## Key Flows

- F1. Client setup and mode confirmation
  - **Trigger:** An operator links a new client or revisits an existing client in setup or `plug clients`.
  - **Actors:** A1, A3
  - **Steps:** `plug` detects the client target, chooses a default lazy-discovery mode for that client, shows the chosen mode in setup and client-management UI, and lets the operator confirm or override it.
  - **Outcome:** The client target has a durable configured mode with an understandable rationale and a visible override path.
  - **Covered by:** R1, R2, R10, R11

- F2. Search, load, and direct call
  - **Trigger:** An AI client needs a tool that is not currently in its active working set.
  - **Actors:** A2, A3
  - **Steps:** The client uses the lazy-discovery surface exposed by `plug`, `plug` returns machine-readable search results, `plug` loads the selected real tool definitions into the active working set, and the client calls the loaded tool using the same routed name and semantics as normal mode.
  - **Outcome:** The client reaches a direct-call state without having to stay inside a permanent wrapper contract.
  - **Covered by:** R3, R4, R5, R6, R7

- F3. Working-set evolution
  - **Trigger:** A session changes context, accumulates too many loaded tools, or explicitly resets its working set.
  - **Actors:** A2, A3
  - **Steps:** `plug` keeps loaded tools sticky across the session, evicts tools when policy requires it, and preserves a coherent client-visible tool set throughout the session.
  - **Outcome:** The working set stays small enough to protect context and large enough to avoid constant re-loading churn.
  - **Covered by:** R8, R9, R12

---

## Requirements

**Cross-client lazy-discovery model**
- R1. `plug` must define one internal lazy-discovery engine that is shared across supported clients rather than maintaining unrelated discovery implementations per client.
- R2. `plug` must choose a default lazy-discovery mode per client target using a maintained capability matrix, while allowing operators to override the chosen mode per client target.
- R3. The client-facing discovery surface must be adaptive by client target rather than forcing one identical external contract on every client.
- R4. For clients that need a `plug`-owned lazy-discovery bridge, the bridge must support at least search, load, and eviction as first-class actions rather than only text search plus wrapper invocation.

**Loaded-tool behavior**
- R5. After a tool is loaded into the active working set, it must appear to the client exactly as it would in `plug`'s normal routed-tool mode, with the same routed tool name and no alternate wrapper identity.
- R6. Loaded tools must be directly callable through the normal client tool-calling path rather than requiring a permanent `plug__invoke_tool`-style indirection.
- R7. Any lazy-discovery bridge must return machine-readable discovery results and must expose full tool definitions at load time so the client/model can call loaded tools reliably.
- R8. The active working set must be session-scoped and sticky across turns rather than re-created from scratch every turn.
- R9. The active working set must support eviction so `plug` can unload tools when context shifts or budget pressure requires it.

**Permissions, trust, and operator clarity**
- R10. The chosen mode for each client target must be visible and editable in both setup/wizard flows and the `plug clients` management surface, with config remaining the durable source of truth.
- R11. `plug` must make its automatic mode choice legible enough that an operator can verify whether the default was correct during first-time setup.
- R12. Lazy-discovery behavior must preserve normal client permission and approval semantics as much as the client allows, rather than collapsing all hidden tool usage behind one coarse-grained visible meta-tool.
- R13. For clients that need a `plug`-owned bridge, `plug` must expose a compact always-visible discovery hint or summary surface so the model understands how to discover tools without blind guesswork.

**Compatibility and transition**
- R14. V2 must supersede the current meta-tool-mode contract rather than merely polishing its text-search UX.
- R15. `plug__invoke_tool` may remain available as a fallback or debug primitive, but it must not be the primary UX for normal lazy-discovery sessions.
- R16. Standard full-tool mode must remain available for clients or operators that do not want lazy discovery.

---

## Acceptance Examples

- AE1. **Covers R2, R10, R11.** Given a freshly linked OpenCode client, when the operator runs setup or opens `plug clients`, `plug` shows the mode it chose for OpenCode, explains that choice well enough to inspect it, and lets the operator override it for the OpenCode target without affecting other client targets.

- AE2. **Covers R5, R6, R7.** Given a lazy-discovery session with a hidden Slack tool, when the client searches for Slack tools and loads the chosen one, the loaded Slack tool appears with the same routed name it would have in normal mode and is then called directly without a permanent wrapper tool.

- AE3. **Covers R8, R9.** Given a session that has already loaded several tools, when the conversation shifts to a different task and the working set exceeds policy, `plug` evicts tools coherently so the active set stays bounded without forcing the client to rediscover everything on every turn.

- AE4. **Covers R12, R15.** Given a client with per-tool approval behavior, when a loaded tool is called, the client sees that call as the real routed tool rather than only as `plug__invoke_tool`, while `plug__invoke_tool` remains available only for explicit fallback or debugging.

---

## Success Criteria

- Operators can connect a client, understand the chosen lazy-discovery mode, and correct it without reading source code or inventing a mental model from logs.
- Clients that currently suffer from eager large-tool exposure can operate with a smaller always-visible surface and a coherent session working set.
- Lazy-loaded tools feel like normal `plug` tools once loaded, which reduces prompt burden and preserves trust around names, approvals, and call semantics.
- A downstream planner can turn this document into an implementation plan without inventing the product shape, operator UX, or working-set semantics.

---

## Scope Boundaries

- This redesign is about lazy tool discovery, loading, eviction, and per-client mode selection, not a broad rewrite of unrelated routing or transport subsystems.
- V2 should reuse the useful internals of the existing meta-tool-mode work where appropriate, but it should not preserve wrapper-first UX just because it already exists.
- This work should not assume every client can support the same native illusion; adaptive client-facing behavior is acceptable and expected.
- The first version does not need to solve every possible future client or define a universal public lazy-discovery protocol for the entire MCP ecosystem.
- This redesign does not require removing standard full-tool mode.

---

## Key Decisions

- Use one internal lazy-discovery engine with adaptive client-facing surfaces rather than one identical external lazy contract for all clients.
- Prefer smart per-client defaults for average users, but make first-time inspection and manual override part of the product, not an afterthought.
- Use a session-scoped sticky working set with eviction rather than turn-only loading or no-eviction stickiness.
- Present loaded tools exactly like normal `plug` mode once loaded.
- Treat the current meta-tool-mode contract as prior art, not the final contract to preserve.

---

## Dependencies / Assumptions

- `plug` can reliably identify client targets well enough to drive smart defaults from a maintained compatibility matrix.
- Different clients will require different external lazy-discovery surfaces even if they share one internal working-set engine.
- Some client permission models may impose hard limits on how fully `plug` can preserve native approval semantics; v2 should preserve them as far as the client permits.
- The current shipped baseline already includes meta-tool mode and client-compatibility documentation, which can serve as input to planning rather than greenfield invention.

---

## Outstanding Questions

### Deferred to Planning

- [Affects R2, R3][Needs research] Which concrete client capability buckets should drive the initial smart-default matrix, and how should each bucket map to an external lazy-discovery surface?
- [Affects R4, R7, R13][Technical] What is the smallest bridge surface that still supports reliable search, load, and eviction for non-native clients?
- [Affects R8, R9][Technical] What eviction policy should the first version implement, and what session state must be tracked to make it coherent?
- [Affects R12][Needs research] Which client permission and approval behaviors can be preserved fully, partially, or not at all across the first supported clients?

---

## Next Steps

-> /ce-plan for structured implementation planning
