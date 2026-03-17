# HTTP Session Parity Architecture Plan

**Goal:** Make downstream HTTP sessions visible with the same operator fidelity as daemon proxy
clients, without misrepresenting current daemon-only inventory as transport-complete.

## Problem

`plug clients`, `plug status`, and related views now explicitly state that live client/session
inventory is daemon-proxy-only. That honesty fix is correct, but it also confirms a remaining
product gap: downstream HTTP sessions are still not part of the live operator inventory.

This is not a simple missing label.

- daemon mode tracks proxy clients in `ClientRegistry`
- standalone `serve` mode tracks downstream HTTP sessions in `StatefulSessionStore`
- the two inventories do not currently share a transport-aware session snapshot model

## Verified Current Architecture

### Daemon mode

- `plug/src/runtime.rs`
  - `cmd_daemon()` starts the engine, config watcher, and `daemon::run_daemon(...)`
- `plug/src/daemon.rs`
  - `run_daemon()` owns Unix IPC, proxy session registration, runtime health, and auth state
  - `ListClients` returns only `ClientRegistry::list()`

### Standalone serve mode

- `plug/src/runtime.rs`
  - `cmd_serve()` creates `StatefulSessionStore`, builds `HttpState`, and calls `serve_router(...)`
- `plug-core/src/http/server.rs`
  - downstream HTTP sessions are created on initialize and stored in the HTTP session store
- `plug-core/src/session/mod.rs`
  - `SessionStore` has mutation/validation APIs and `session_count()`, but no snapshot/list API

## Consequence

There are currently two separate live-session worlds:

1. IPC proxy clients owned by daemon runtime
2. HTTP downstream sessions owned by standalone `serve`

So a true parity feature cannot be completed by extending `ListClients` alone.

## Desired Outcome

Operators should be able to answer these questions directly from the product:

- which downstream sessions are active right now?
- which transport does each session use?
- which logical client is it associated with?
- how long has it been connected?
- when was it last active?
- is the session coming from daemon proxy, local HTTP, or remote/public HTTP traffic?

## Options

### Option 1: Move downstream HTTP serving under daemon ownership

Create one background runtime that owns:

- engine
- IPC proxy client registry
- downstream HTTP router
- shared session inventory

Pros:
- one source of runtime truth
- simplest operator model
- `plug status`, `plug clients`, and `plug doctor` can all query one live service

Cons:
- larger behavior change
- touches daemon startup/shutdown lifecycle
- more risk to existing running setups

### Option 2: Keep daemon and standalone serve separate, add a merged snapshot layer

Add a transport-aware session snapshot API for HTTP sessions and merge it with daemon IPC clients in
the CLI/operator layer.

Pros:
- smaller runtime behavior change
- less risky than re-owning HTTP under daemon immediately
- can deliver visibility parity without first redesigning background service topology

Cons:
- still leaves two runtime truth sources
- requires explicit aggregation semantics
- can be unavailable when one side is unreachable

## Recommended Path

Take Option 2 first.

Reasoning:
- the current hardening program was about reducing confusion and making truth explicit
- Option 2 improves operator truth without destabilizing the existing runtime model
- if daemon-owned HTTP becomes desirable later, the snapshot model remains useful rather than wasted

## Proposed Design

### 1. Introduce a shared downstream session snapshot type

Likely location:
- `plug-core/src/session/mod.rs`
or
- `plug-core/src/ipc.rs`

Suggested fields:
- `transport`
- `session_id`
- `client_type`
- `client_info`
- `connected_at` or `connected_secs`
- `last_activity_at` or `last_activity_secs`

### 2. Extend `SessionStore` with read-only snapshot support

Add a method such as:

- `list_sessions() -> Vec<SessionSnapshot>`

This should be read-only and avoid exposing mutable store state.

### 3. Teach `StatefulSessionStore` to retain connection-time metadata

Current state already tracks activity and client type, but parity needs stable visibility fields for
inventory rendering and troubleshooting.

### 4. Introduce a merged live-session response

Instead of overloading current `ListClients`, either:

- replace it with a transport-aware live-session response, or
- add a new IPC request/response dedicated to session inventory

The merged response should preserve:
- daemon proxy sessions
- HTTP downstream sessions
- transport/source distinction

### 5. Update operator surfaces

Once the merged response exists, update:

- `plug clients`
- `plug status`
- overview/menu surfaces that show live clients

### 6. Add regression coverage

Required scenarios:
- daemon proxy clients only
- HTTP sessions only
- mixed daemon proxy + HTTP sessions
- daemon available, serve unavailable
- serve available, daemon unavailable

## Non-Goals

- reworking OAuth/session semantics themselves
- changing current auth standards behavior
- pretending parity already exists before the runtime model supports it

## Acceptance Criteria

- operators can see active downstream HTTP sessions from the normal CLI surfaces
- transport is explicit for every live session
- daemon proxy and HTTP sessions can coexist in one inventory view without ambiguity
- tests cover mixed transport session inventories and degraded partial-availability cases

## References

- [todos/056-pending-p2-http-session-ux-parity.md](../../todos/056-pending-p2-http-session-ux-parity.md)
- [docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md](./2026-03-16-auth-oauth-hardening-ux-plan.md)
