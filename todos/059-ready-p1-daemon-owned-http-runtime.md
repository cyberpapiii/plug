---
status: ready
priority: p1
issue_id: "059"
tags: [daemon, http, runtime, architecture, sessions, ux]
dependencies: ["058"]
---

# Daemon-Owned HTTP Runtime Program

## Problem Statement

`plug` now has honest transport-aware live inventory, but it still relies on a merged view when
daemon IPC and downstream HTTP are running as separate runtime authorities. That keeps the product
truthful, but it is still more complex than necessary.

## Goal

Promote the background service to the primary runtime authority for both downstream stdio and
downstream HTTP, while preserving standalone `plug serve` as an explicit fallback/debug path.

## Task List

### Task 1: Daemon owns shared HTTP runtime

Outcome:
- daemon startup creates and serves downstream HTTP itself

Acceptance:
- daemon `ListLiveSessions` can include HTTP sessions directly
- daemon startup fails early if HTTP cannot bind

### Task 2: Runtime inventory trusts authoritative daemon responses

Outcome:
- `fetch_live_sessions(...)` stops querying standalone HTTP when daemon already has full transport
  truth

Acceptance:
- `transport_complete` daemon responses are returned directly
- old-daemon and no-daemon fallbacks still work

### Task 3: Clarify command/help semantics

Outcome:
- user-facing command model matches the new ownership model

Acceptance:
- `plug start` / `plug serve --daemon` clearly mean shared background service with IPC + HTTP
- standalone `plug serve` is documented as explicit foreground/fallback behavior

### Task 4: Truth-pass and verification

Outcome:
- docs and tests reflect the new architecture

Acceptance:
- current-truth docs updated after code lands
- focused and full test suite coverage added

## Work Log

### 2026-03-17 - Program created

**By:** Codex

**Actions:**
- Split the daemon-owned HTTP architecture step out from the completed parity program.
- Defined the lowest-risk shape: daemon owns HTTP when background service runs, standalone
  `plug serve` remains available explicitly.

**Learnings:**
- The parity program solved visibility.
- This program is about simplifying runtime truth, not re-solving operator inventory.
