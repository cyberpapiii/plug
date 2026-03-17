---
status: pending
priority: p2
issue_id: "056"
tags: [http, ux, sessions, parity, observability]
dependencies: []
---

# Problem Statement

The current plug terminal/menu UX does not appear to surface active downstream HTTP sessions with parity to local stdio clients. During remote Claude Desktop/Mobile troubleshooting, active HTTP connector usage was not clearly visible or distinguishable in the menu/session system, which made diagnosis materially more confusing.

# Findings

- Remote Claude HTTP traffic was successfully reaching `plug serve`, but the user could not confirm that from the normal menu/session UX.
- The troubleshooting path had to rely on log inspection instead of first-class in-product session visibility.
- The user specifically observed that the menu system did not show an active Claude Desktop HTTP session and did not clearly differentiate transports or clients.
- This may be one of two issues:
  - HTTP sessions are not included in the menu/session inventory
  - HTTP sessions are included internally but not exposed or labeled clearly in the UX

# Proposed Solutions

## Option 1: Surface HTTP sessions in existing session views

Add HTTP-backed downstream sessions to the same inventory used by the terminal/menu system and label each session with:

- transport: `http` or `stdio`
- client identity when known
- session ID
- connected timestamp / activity timestamp

### Pros

- Minimal conceptual change
- Gives immediate parity with existing stdio visibility
- Helps debugging without teaching a new UI model

### Cons

- Depends on how session data is currently modeled
- May expose partial/messy client identity if metadata quality is inconsistent

## Option 2: Add a transport-aware session diagnostics view

Keep current menus intact but add a dedicated diagnostics/session view that merges:

- downstream stdio clients
- downstream HTTP sessions
- upstream server health/state

### Pros

- Cleaner operator-focused debugging surface
- Easier to design for parity explicitly

### Cons

- Larger scope than a direct parity fix
- More product/UI work

# Recommended Action

Investigate the current session/menu plumbing first and answer one question clearly: are downstream HTTP sessions missing from the underlying inventory, or just not exposed/labeled in the UI? Then implement the smallest change that gives operators transport-aware session visibility with clear client labeling.

# Acceptance Criteria

- [x] Investigation confirms whether downstream HTTP sessions are currently tracked by the menu/session subsystem
- [ ] The UX can show active HTTP sessions alongside stdio sessions, or a dedicated diagnostics view exists with equivalent visibility
- [ ] Session transport is explicitly labeled
- [ ] Claude Desktop/Mobile HTTP sessions can be distinguished from local stdio clients during troubleshooting
- [ ] A regression or smoke-test procedure exists for verifying remote-session visibility

# Work Log

### 2026-03-10 - Incident follow-up capture

**By:** Codex

**Actions:**
- Recorded the user-observed UX parity gap after stabilizing the Claude remote HTTP connector path
- Captured the need for investigation rather than assuming whether the issue is missing tracking vs missing presentation

**Learnings:**
- Remote HTTP support is materially harder to operate if logs are the only trustworthy source of session truth
- Session visibility parity is part of feature completeness, not optional polish

### 2026-03-17 - Inventory-path investigation and explicit UX caveat

**By:** Codex

**Actions:**
- Traced the live-client path end to end:
  - `plug-core` HTTP sessions are tracked only inside `SessionStore` / `StatefulSessionStore`
  - daemon `ListClients` returns only IPC proxy clients from `ClientRegistry`
  - `plug clients` builds its live inventory from that daemon IPC list only
- Confirmed the parity problem is therefore an underlying inventory/model gap, not just hidden UI
  data.
- Added an explicit note to `plug clients` so the command now states that its live inventory is
  daemon-proxy-only and does not yet include downstream HTTP sessions.

**Evidence:**
- `plug-core/src/http/server.rs` creates HTTP sessions and records only `client_type`
- `plug-core/src/session/mod.rs` has no list/snapshot API
- `plug/src/daemon.rs` `ListClients` returns `ctx.client_registry.list()` only
- `plug/src/runtime.rs` `fetch_live_clients()` consumes only that daemon IPC response

**Learnings:**
- The shortest honest fix is to surface the limitation explicitly now, then add a unified transport-
  aware session snapshot model later.
- Full parity will require shared session snapshot types plus merged daemon/HTTP inventory, not
  just another label in the existing client view.
