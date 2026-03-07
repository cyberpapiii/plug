---
title: "feat: phase 3d session store stateless prep"
type: feat
status: completed
date: 2026-03-07
origin: docs/brainstorms/2026-03-07-phase3d-session-store-stateless-prep-brainstorm.md
---

# Phase 3D Session Store Stateless Prep

## Overview

Introduce a real `SessionStore` abstraction at the top level, preserve current behavior through a
`StatefulSessionStore`, and document how future stateless downstream handling would plug in without
implementing it yet.

## Problem Statement / Motivation

Current downstream session handling is still embedded directly in `plug-core/src/http/session.rs` as
one concrete in-memory implementation. That works today, but it leaves no honest seam for the
future stateless MCP direction already called out in the roadmap.

The next architectural step is to make the seam explicit while keeping production behavior
unchanged.

## Proposed Solution

This tranche will:

- add `plug-core/src/session/mod.rs`
- define a `SessionStore` trait for downstream session operations
- move the existing HTTP session implementation into `StatefulSessionStore`
- thread the HTTP runtime through the trait boundary
- write `docs/research/stateless-mcp-design-notes.md`

This tranche will not:

- implement a stateless session store
- change downstream protocol behavior
- alter HTTP session semantics

## Technical Considerations

- Keep the refactor additive and behavior-preserving
- Minimize churn by leaving `http::session` as a compatibility shim or re-export if helpful
- Ensure the abstraction is actually used by HTTP state, not just defined
- Keep the stateless notes concrete about entry points, discovery, and routing constraints

## System-Wide Impact

- **Interaction graph**: HTTP handlers -> `HttpState.sessions` -> `SessionStore` trait -> current
  `StatefulSessionStore`.
- **Error propagation**: session validation and SSE delivery errors must remain unchanged.
- **State lifecycle risks**: this is a refactor of session ownership boundaries, so cleanup-task and
  pending-notification semantics must stay identical.
- **API surface parity**: only HTTP currently uses this path; stdio and daemon paths should remain
  untouched.
- **Integration test scenarios**:
  - existing HTTP session tests remain green through the trait boundary
  - runtime wiring still starts cleanup and serves requests correctly

## Acceptance Criteria

- [x] Add top-level `SessionStore` trait and `StatefulSessionStore`
- [x] Keep current HTTP session behavior unchanged through the new abstraction
- [x] Switch HTTP runtime/state wiring to depend on the trait boundary
- [x] Add stateless design notes documenting entry points and constraints
- [x] Full suite passes after the refactor

## Sources & References

- **Origin brainstorm:** `docs/brainstorms/2026-03-07-phase3d-session-store-stateless-prep-brainstorm.md`
- `/Users/robdezendorf/Documents/GitHub/plug/docs/plans/2026-03-06-feat-strategic-stabilize-comply-compete-plan.md`
- `plug-core/src/http/session.rs`
- `plug-core/src/http/server.rs`
- `plug/src/runtime.rs`
