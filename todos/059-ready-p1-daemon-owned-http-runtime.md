---
status: done
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

Promote the shared service to the primary runtime authority for both downstream stdio and
downstream HTTP, including foreground `plug serve` usage, so the product stops maintaining a second
standalone HTTP runtime path.

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
- `plug serve` clearly means the same shared service in the foreground

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
- Defined the initial daemon-owned HTTP architecture step and its verification boundaries.

**Learnings:**
- The parity program solved visibility.
- This program is about simplifying runtime truth, not re-solving operator inventory.

### 2026-03-17 - Daemon became the primary HTTP runtime authority

**By:** Codex

**Actions:**
- Reused the shared HTTP runtime builder in daemon mode so the background service now owns
  downstream HTTP/HTTPS alongside IPC.
- Extended daemon `ListLiveSessions` to include HTTP session snapshots directly and report
  `transport_complete` when it owns both transports.
- Updated runtime inventory fetches to trust transport-complete daemon responses directly instead
  of re-querying standalone HTTP and risking double-counting.
- Clarified command/docs semantics so:
  - `plug start` / `plug serve --daemon` mean the shared background service
  - standalone `plug serve` remains an explicit foreground/fallback path

**Verification:**
- `cargo test -p plug live_sessions -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- The safest architecture shift was to make daemon mode authoritative first without deleting the
  standalone HTTP path.
- This keeps one-runtime truth for normal background-service usage while preserving a lower-risk
  escape hatch for explicit standalone serving.
