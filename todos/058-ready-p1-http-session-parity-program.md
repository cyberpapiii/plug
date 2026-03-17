---
status: ready
priority: p1
issue_id: "058"
tags: [http, sessions, parity, topology, daemon, serve, ux, architecture]
dependencies: ["056", "057"]
---

# HTTP Session Parity and Runtime Topology Program

## Problem Statement

`plug` now tells the truth about its live client/session scope, but the product still lacks true
transport-complete live inventory. Daemon IPC proxy clients and downstream HTTP sessions are
tracked in different runtime models, so operators cannot yet see a unified, transport-aware view of
active downstream usage.

## Goal

Deliver transport-complete live session visibility without regressing the auth/OAuth hardening work
that just landed.

Operators should be able to answer, from normal CLI surfaces:

- which downstream sessions are active now
- which transport each session uses
- which logical client each session belongs to
- whether the session came from daemon proxy or downstream HTTP
- how recently the session connected / was active

## References

- [docs/plans/2026-03-17-http-session-parity-architecture.md](../docs/plans/2026-03-17-http-session-parity-architecture.md)
- [todos/056-pending-p2-http-session-ux-parity.md](./056-pending-p2-http-session-ux-parity.md)
- [todos/057-ready-p1-auth-oauth-hardening-program.md](./057-ready-p1-auth-oauth-hardening-program.md)

## Execution Strategy

Recommended first implementation path:

1. keep daemon and standalone `serve` separate for now
2. add a read-only downstream HTTP session snapshot model
3. add a merged live-session response for operator surfaces
4. update CLI views and JSON outputs to use the unified inventory
5. verify mixed-transport scenarios explicitly

## Task List

### Task 1: Define a shared transport-aware session snapshot model

Outcome:
- one canonical read-only session shape for operator inventory

Likely files:
- `plug-core/src/session/mod.rs`
- `plug-core/src/ipc.rs`

Acceptance:
- shared snapshot type exists
- fields cover transport, session id, client identity, and timing metadata

### Task 2: Extend `SessionStore` with read-only snapshot/list support

Outcome:
- downstream HTTP session state can be queried safely for inventory purposes

Likely files:
- `plug-core/src/session/mod.rs`
- `plug-core/src/session/stateful.rs`

Acceptance:
- `StatefulSessionStore` can return stable read-only snapshots
- timing metadata is preserved well enough for CLI display

### Task 3: Introduce merged live-session IPC/runtime response

Outcome:
- operator surfaces can ask one question and receive a transport-aware live session inventory

Likely files:
- `plug-core/src/ipc.rs`
- `plug/src/daemon.rs`
- `plug/src/runtime.rs`

Acceptance:
- live daemon IPC sessions and HTTP session snapshots can be surfaced in one response shape
- daemon-only environments still behave coherently
- partial-availability cases are explicit rather than hidden

### Task 4: Update `plug clients` to render unified live session inventory

Outcome:
- `plug clients` no longer needs the current daemon-proxy-only caveat once parity exists

Likely files:
- `plug/src/commands/clients.rs`
- `plug/src/views/clients.rs`

Acceptance:
- daemon proxy and HTTP sessions are both visible
- transport is explicit per live session
- Claude/Desktop/Mobile HTTP sessions can be distinguished during troubleshooting

### Task 5: Update `plug status` and overview surfaces

Outcome:
- runtime summary uses the same unified live session truth as `plug clients`

Likely files:
- `plug/src/views/overview.rs`
- `plug/src/ui.rs`

Acceptance:
- runtime/overview client counts are transport-complete
- text and JSON outputs agree on scope and semantics

### Task 6: Add regression and mixed-topology coverage

Outcome:
- the parity work is pinned by tests, not just by local observation

Likely files:
- `plug-core/src/session/stateful.rs`
- `plug/src/daemon.rs`
- `plug/src/commands/clients.rs`
- integration test locations as needed

Acceptance:
- tests cover daemon-only, HTTP-only, and mixed-session inventories
- tests cover degraded partial-availability behavior

### Task 7: Final truth-pass and UX cleanup

Outcome:
- remaining caveat text and docs match the new reality cleanly

Likely files:
- `docs/plans/2026-03-17-http-session-parity-architecture.md`
- `todos/056-pending-p2-http-session-ux-parity.md`
- `todos/057-ready-p1-auth-oauth-hardening-program.md`
- `docs/PROJECT-STATE-SNAPSHOT.md`
- `docs/PLAN.md`

Acceptance:
- docs state clearly what is now done on `main`
- obsolete scope caveats are removed or revised

## Agent Boundaries

- Agent A: session snapshot model in `plug-core/src/session/*`
- Agent B: IPC/runtime inventory integration in `plug-core/src/ipc.rs`, `plug/src/daemon.rs`, `plug/src/runtime.rs`
- Agent C: CLI/client view integration in `plug/src/commands/clients.rs`, `plug/src/views/clients.rs`, `plug/src/views/overview.rs`

These write scopes should remain mostly disjoint until integration.

## Verification

- focused unit tests for session snapshot model
- focused unit tests for merged IPC/runtime response
- focused CLI/view tests for clients/overview output
- full `cargo test`
- `cargo build --release`

## Work Log

### 2026-03-16 - Program created

**By:** Codex

**Actions:**
- Split the remaining runtime-topology/session-parity work out of the auth/OAuth hardening program.
- Captured the implementation order, acceptance criteria, and agent write boundaries.
- Chose the lower-risk initial path: merged snapshot model before daemon-owned HTTP serving.

**Learnings:**
- The remaining problem is architectural, not cosmetic.
- Keeping this work as a separate program reduces the risk of claiming session parity before the
  runtime model actually supports it.

### 2026-03-17 - Session snapshot and transport-aware runtime inventory foundation

**By:** Codex

**Actions:**
- Added a shared downstream session snapshot model in `plug-core` with explicit transport,
  `session_id`, client identity, and timing metadata.
- Extended the stateful HTTP session store with a read-only listing API so operator surfaces can
  inspect downstream HTTP session state without mutating the store.
- Introduced an additive transport-aware IPC/runtime response:
  - `IpcRequest::ListLiveSessions`
  - `IpcResponse::LiveSessions`
  - `LiveSessionTransport`
  - `LiveSessionInventoryScope`
- Updated daemon/runtime/client/overview surfaces to use the new response shape while preserving
  fallback compatibility with older daemons.
- Kept scope explicit: the new live-session inventory currently reports `daemon_proxy_only`
  truthfully instead of pretending HTTP session parity already exists.

**Verification:**
- `cargo test -p plug-core session -- --nocapture`
- `cargo test -p plug-core response_serialization_round_trip -- --nocapture`
- `cargo test -p plug-core requires_auth_identifies_admin_commands -- --nocapture`
- `cargo test -p plug views::clients -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug -- --nocapture`
- `cargo test -p plug-core -- --nocapture`

**Learnings:**
- The new response shape is worth keeping even before full parity because it gives every operator
  surface one explicit place to express live-session scope.
- True HTTP parity still requires a second runtime truth source or an aggregation boundary that can
  combine standalone HTTP session snapshots with daemon proxy sessions without hiding degraded
  availability.

### 2026-03-17 - Status surface aligned with live-session inventory model

**By:** Codex

**Actions:**
- Switched `plug status` to consume the transport-aware live-session inventory path instead of
  relying only on the older daemon `Status.clients` field.
- Added explicit `Live Sessions`, `Live Transports`, and `Inventory Scope` text output when the
  daemon supports the newer inventory response.
- Preserved the daemon-restart path for older runtimes by falling back to the legacy daemon proxy
  count with an explicit restart-required message.
- Extended status JSON with:
  - `live_session_count`
  - `live_session_transports`
  - `live_client_support`

**Verification:**
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug commands::clients::tests -- --nocapture`

**Learnings:**
- The top-level runtime view should use the same live-session vocabulary as `plug clients`, even
  before transport-complete parity exists.
- Keeping the old daemon `clients` field in JSON remains useful for compatibility, but it should no
  longer be the only operator-visible runtime count.

### 2026-03-17 - Standalone HTTP inventory export and scope expansion

**By:** Codex

**Actions:**
- Added a token-protected local operator endpoint on the standalone HTTP runtime:
  - `/_plug/live-sessions`
- Added a dedicated operator token path separate from downstream auth material:
  - `http_operator_token_<port>`
- Taught `fetch_live_sessions(...)` to query both:
  - daemon proxy live sessions via IPC
  - standalone HTTP session snapshots via the new local operator endpoint
- Expanded inventory scope semantics beyond the original binary model:
  - `daemon_proxy_only`
  - `http_only`
  - `transport_complete`
  - `unavailable`
- Updated `clients` / `overview` / `status` surfaces to understand the expanded scope values.

**Verification:**
- `cargo test -p plug runtime::tests -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug commands::clients::tests -- --nocapture`
- `cargo test -p plug views::clients -- --nocapture`

**Learnings:**
- A secure standalone export path is feasible without pretending the daemon owns HTTP.
- The current implementation can now distinguish daemon-only, HTTP-only, and fully merged session
  truth, which is enough to make later aggregation work incremental instead of architectural guesswork.
