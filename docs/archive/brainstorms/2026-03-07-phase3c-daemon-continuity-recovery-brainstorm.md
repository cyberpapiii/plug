# Brainstorm: Phase 3C Daemon Continuity and Recovery Verification

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next Phase 3 tranche is a narrow continuity/recovery verification pass for the daemon-backed
`plug connect` path.

This tranche focuses on:

- daemon continuity after daemon shutdown/restart
- proving the IPC proxy heals without requiring a new downstream client process
- proving safe proxy traffic resumes after reconnect and that the daemon session is replaced

This tranche does **not** include:

- stateless session abstraction
- remote HTTP client continuity
- upstream crash/restart recovery inside the shared router
- a full final doc/risk-register release pass

## Why This Approach

The codebase already contains daemon continuity logic:

- stable `client_id` registration
- protocol-versioned daemon handshake
- reconnectable IPC proxy session refresh
- heartbeat-driven stale-session repair

What it does **not** yet have is strong end-to-end proof that this works the way the product
promises it should.

That makes continuity verification the highest-value remaining Phase 3 runtime slice before moving
on to the mostly design/documentation-oriented work.

## Key Decisions

- **Treat continuity as a verification tranche, not a greenfield feature build.**
  The reconnect machinery already exists; the missing asset is proof.

- **Use the real daemon socket path, not a fake in-memory substitute.**
  This needs to validate the actual runtime boundary where continuity matters.

- **Add a test-only daemon runtime-path override instead of mutating process env unsafely.**
  Rust 2024 makes ad hoc env mutation the wrong pattern for this.

- **Prove continuity with a real downstream stdio client.**
  The important behavior is “the same downstream client keeps working after daemon restart,” not
  just “a low-level reconnect helper returns Ok.”

- **Keep upstream recovery out of this tranche.**
  That is a different subsystem with its own already-implemented reconnect path.

## Resolved Questions

- **Do we need to invent new continuity logic first?** No
- **Should this tranche use real daemon sockets?** Yes
- **Should this tranche include stateless design work?** No
- **Should this tranche include upstream restart recovery?** No

## Open Questions

None. The scope is narrow enough to proceed directly to planning.

## Next

Write a focused plan for:

1. isolated daemon runtime-path overrides for tests
2. one real daemon continuity end-to-end test
3. one real “recovered proxy still handles tool traffic” assertion path
