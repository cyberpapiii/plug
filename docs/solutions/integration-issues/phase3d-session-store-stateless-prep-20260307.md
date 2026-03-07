---
title: "Phase 3D session-store abstraction keeps HTTP behavior stable while creating the stateless seam"
category: integration-issues
tags:
  - session-store
  - stateless
  - http
  - architecture
  - refactor
  - stateful-session-store
module: plug-core
date: 2026-03-07
symptom: |
  Downstream session handling still lived directly in `plug-core/src/http/session.rs` as one
  concrete in-memory implementation. That worked for the current HTTP transport, but it left no
  honest architectural seam for the future stateless MCP direction already anticipated in the
  roadmap. The risk was that any later stateless work would have to either tunnel through HTTP-only
  assumptions or do a larger, riskier refactor under deadline pressure.
root_cause: |
  Session lifecycle, SSE sender ownership, client-type storage, and cleanup behavior were bundled as
  HTTP module details instead of being modeled as a first-class downstream session abstraction. That
  made the code truthful for one implementation but not truthful about where alternative session
  strategies would plug in.
severity: medium
related:
  - docs/brainstorms/2026-03-07-phase3d-session-store-stateless-prep-brainstorm.md
  - docs/plans/2026-03-07-feat-phase3d-session-store-stateless-prep-plan.md
  - docs/research/stateless-mcp-design-notes.md
  - plug-core/src/session/mod.rs
  - plug-core/src/session/stateful.rs
  - plug-core/src/http/server.rs
  - plug-core/src/http/session.rs
  - plug/src/runtime.rs
---

# Phase 3D session-store abstraction keeps HTTP behavior stable while creating the stateless seam

## Problem

The code already had a working stateful downstream session manager, but it was still embedded inside
the HTTP module.

That created two problems:

1. the architecture had no honest seam for a future stateless downstream mode
2. any later stateless work would have to untangle HTTP-specific assumptions and production
   behavior at the same time

The right move was not to implement stateless handling yet. It was to make the seam explicit while
proving that current HTTP behavior stayed exactly the same.

## Solution

### 1. Add a top-level `SessionStore` trait

`plug-core/src/session/mod.rs` now defines the downstream session contract:

- create / validate / remove sessions
- attach SSE senders
- store and read client type
- targeted and broadcast notification delivery
- cleanup-task spawning

This is intentionally the current behavioral surface, not a speculative “perfect future” API.

### 2. Move the current implementation into `StatefulSessionStore`

The old in-memory HTTP session manager became `StatefulSessionStore` in
`plug-core/src/session/stateful.rs`.

That preserved:

- expiry semantics
- SSE sender behavior
- pending targeted notification buffering
- cleanup behavior

The refactor was structural, not semantic.

### 3. Keep `http::session` as a compatibility shim

`plug-core/src/http/session.rs` now re-exports:

- `SessionStore`
- `StatefulSessionStore`
- the compatibility alias `SessionManager`

That kept the migration small and avoided touching unrelated callers just to satisfy the new module
layout.

### 4. Make the HTTP boundary actually depend on the trait

`HttpState.sessions` is now `Arc<dyn SessionStore>`, and runtime wiring constructs a
`StatefulSessionStore` behind that trait boundary.

That makes the abstraction real: the HTTP server is no longer coupled to the concrete store type.

### 5. Add stateless design notes without implementing stateless behavior

`docs/research/stateless-mcp-design-notes.md` records:

- likely stateless entry points
- how the trait boundary would be used
- where capability discovery replaces initialization
- the constraints around bridging stateless downstream to stateful upstream stdio servers

## Verification

The key verification requirement was “same behavior, different seam.”

This was validated by:

- focused HTTP/session tests
- stateful session-store unit tests in the new module
- full workspace verification:
  - `cargo test`
  - `cargo clippy --all-targets --all-features -- -D warnings`

## Prevention / Reuse

The main lesson is to create the seam before the second implementation arrives.

For future architecture work:

- define the trait while there is still only one implementation
- thread the abstraction through one real runtime boundary so it is not dead code
- keep compatibility shims when they substantially reduce churn during a behavior-preserving refactor

This tranche turns “stateless support later” from a vague aspiration into a real integration point.
