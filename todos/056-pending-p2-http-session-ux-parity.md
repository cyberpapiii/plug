---
status: done
priority: p2
issue_id: "056"
tags: [http, ux, sessions, parity, observability]
dependencies: []
---

# Problem Statement

The current plug terminal/menu UX did not originally surface active downstream HTTP sessions with parity to local stdio clients. During remote Claude Desktop/Mobile troubleshooting, active HTTP connector usage was not clearly visible or distinguishable in the menu/session system, which made diagnosis materially more confusing.

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

# Resolution

Investigation confirmed that downstream HTTP sessions were missing from the original live
daemon/session inventory, not merely hidden in the UI. That work is now implemented on `main`
through an explicit aggregation model:

1. keep downstream HTTP serving and daemon IPC ownership separate for now
2. add an HTTP-session snapshot API plus a higher-level aggregator that merges standalone `serve`
   session state with daemon IPC client state
3. expose explicit inventory scope/availability semantics instead of implying a single unified
   runtime authority

Future architecture work is still possible if the product wants one daemon-owned runtime authority,
but the operator UX parity gap described by this issue is now closed on `main`.

# Acceptance Criteria

- [x] Investigation confirms whether downstream HTTP sessions are currently tracked by the menu/session subsystem
- [x] The UX can show active HTTP sessions alongside stdio sessions, or a dedicated diagnostics view exists with equivalent visibility
- [x] Session transport is explicitly labeled
- [x] Claude Desktop/Mobile HTTP sessions can be distinguished from local stdio clients during troubleshooting
- [x] A regression or smoke-test procedure exists for verifying remote-session visibility

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

### 2026-03-17 - Runtime status scope alignment

**By:** Codex

**Actions:**
- Extended the same explicit scope caveat from `plug clients` to `plug status` so the live runtime
  client count no longer looks like full downstream transport parity.
- Added machine-readable status metadata exposing the same truth:
  - `live_client_scope: "daemon_proxy_only"`
  - `http_sessions_included: false`

**Learnings:**
- Scope honesty has to be consistent across every command that prints live client counts, or users
  will still infer parity from the least explicit surface.

### 2026-03-17 - Architecture follow-up plan captured

**By:** Codex

**Actions:**
- Wrote a dedicated next-phase plan at
  [docs/plans/2026-03-17-http-session-parity-architecture.md](../docs/plans/2026-03-17-http-session-parity-architecture.md)
  so the remaining work is framed as a transport/session inventory architecture project rather than
  vague UI polish.
- Chose the lower-risk initial recommendation: add a merged transport-aware session snapshot layer
  before considering daemon-owned HTTP serving.

**Learnings:**
- The remaining gap is now clear enough to scope independently from the broader auth/OAuth hardening
  program.

### 2026-03-17 - Transport-aware live-session response foundation landed

**By:** Codex

**Actions:**
- Added a shared downstream session snapshot model and read-only HTTP session listing support in
  `plug-core`.
- Added a transport-aware IPC/runtime response used by `plug clients` and overview/status surfaces.
- Preserved explicit scope reporting so the product now says `daemon_proxy_only` through one shared
  response path instead of each surface inferring scope independently.

**Evidence:**
- `plug-core/src/session/mod.rs`
- `plug-core/src/session/stateful.rs`
- `plug-core/src/ipc.rs`
- `plug/src/daemon.rs`
- `plug/src/runtime.rs`
- `plug/src/views/clients.rs`
- `plug/src/views/overview.rs`

**Learnings:**
- The current gap is no longer “lack of transport-aware inventory types”; that foundation now
  exists.
- The remaining gap is the actual aggregation boundary between daemon proxy state and standalone
  HTTP session state.

### 2026-03-17 - Standalone HTTP inventory export landed

**By:** Codex

**Actions:**
- Added a token-protected local operator endpoint on the standalone HTTP runtime so the CLI can
  query downstream HTTP session snapshots directly.
- Updated runtime aggregation to merge daemon proxy sessions with standalone HTTP session truth when
  available.
- Expanded scope semantics so operator surfaces can distinguish:
  - `daemon_proxy_only`
  - `http_only`
  - `transport_complete`
  - `unavailable`

**Learnings:**
- The remaining parity gap is no longer “HTTP sessions are invisible”; it is now about how far the
  product should go in surfacing partial/degraded cross-runtime states and whether HTTP should
  eventually move under daemon ownership.

### 2026-03-17 - Operator parity closed on `main`

**By:** Codex

**Actions:**
- Added merged live-session inventory semantics across normal operator surfaces:
  - `plug clients`
  - `plug status`
  - overview/runtime views
- Exposed explicit availability/scope metadata and transport labels in both text and JSON output.
- Added contract tests for operator JSON fields and standalone HTTP inventory failure-path tests.
- Updated the architecture plan to treat any remaining work as future runtime-unification scope
  rather than an open operator-visibility bug.

**Evidence:**
- `plug/src/runtime.rs`
- `plug/src/views/clients.rs`
- `plug/src/views/overview.rs`
- `plug/src/commands/auth.rs`
- `plug/src/commands/misc.rs`
- `docs/plans/2026-03-17-http-session-parity-architecture.md`

**Verification:**
- `cargo test -p plug runtime::tests -- --nocapture`
- `cargo test -p plug views::clients -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug commands::auth::tests -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- The original parity incident is resolved once normal CLI/operator surfaces can display merged
  transport-aware live inventory with explicit scope semantics.
- The remaining design question is not visibility parity; it is whether the product eventually wants
  a single daemon-owned runtime truth source.
