# Architecture

Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for current implementation status. This
document describes the architecture of the merged system, not branch-only or historical plan state.

## System Overview

`plug` is a single Rust binary with two active downstream front doors:

- `plug connect` for stdio clients
- `plug serve` for standalone foreground Streamable HTTP clients, with optional HTTPS termination
- daemon-owned HTTP when the shared background service is running

Both paths run on the same core runtime model:

```text
Downstream clients
  stdio clients              -> plug connect -> daemon-backed or standalone proxy
  HTTP / remote clients      -> plug serve or daemon-owned HTTP -> HTTP/HTTPS server + shared engine

Core runtime
  Engine
    -> ServerManager
    -> ToolRouter
    -> config snapshot
    -> lazy tool policy / session working sets
    -> event bus
    -> health / reconnect tasks

Upstream servers
  stdio child-process servers
  streamable-http upstream servers
```

## Runtime Model

### Engine

`Engine` is the single owner of shared runtime truth:

- current config snapshot
- upstream server registry
- merged tool/resource/prompt routing state
- event bus
- shutdown coordination

### ServerManager

`ServerManager` owns upstream lifecycle:

- startup/shutdown
- health state
- circuit breakers
- per-server semaphores
- server-status snapshots

### ToolRouter

`ToolRouter` owns the shared downstream-facing protocol surface:

- merged tools/resources/prompts
- capability synthesis
- tool/resource/prompt routing
- lazy tool policy resolution and bridge working-set visibility
- progress/cancellation correlation
- notification fan-out substrate
- compact `plug__*` discovery tools for bridge clients

### Daemon

The daemon is the authoritative shared local runtime when the background service is running:

- Unix socket IPC
- downstream HTTP/HTTPS server ownership
- admin auth token for control commands
- downstream client registry
- downstream HTTP session inventory
- reconnecting IPC proxy sessions

The daemon-backed path now covers the real shared runtime for both downstream stdio and downstream
HTTP, not just basic tool calls.

## Downstream Capabilities

Current downstream support includes:

- tools
- resources
- prompts
- notifications
- progress
- cancellation
- pagination
- client-aware lazy tool discovery

This applies across stdio and HTTP/HTTPS, with transport-specific details only at the edge.

## Lazy Tool Discovery

`plug` separates the canonical routed catalog from the client-visible tool surface:

- The canonical routed catalog remains global and contains every healthy upstream tool under its normal routed name.
- A per-client lazy policy chooses `standard`, `native`, or `bridge` behavior from config plus client detection.
- Bridge sessions maintain a session-scoped loaded-tool working set.
- `tools/list` for bridge sessions returns compact `plug__*` discovery tools plus any real routed tools loaded into that session.
- `plug__search_tools` returns machine-readable matches from the hidden routed catalog.
- `plug__load_tool` adds a real routed tool to the session working set, returns the full tool definition, and emits `tools/list_changed`.
- `plug__evict_tool` removes a loaded routed tool and emits `tools/list_changed`.
- Loaded tools use the normal routed call path. `plug__invoke_tool` remains only as fallback/debug infrastructure.

This keeps one routing system while allowing clients with weak native lazy behavior, currently OpenCode by default, to avoid receiving hundreds of schemas on every initial tool discovery.

## Session Model

Current HTTP downstream handling uses a `SessionStore` abstraction with one concrete
`StatefulSessionStore` implementation.

That means:

- today’s behavior remains stateful
- HTTP lazy working sets are keyed by downstream HTTP session id
- stdio/daemon lazy working sets are keyed by downstream proxy session id
- the seam for future stateless downstream handling is now explicit
- stateless handling is still design-only, not implemented

## Honest Limitations

The architecture does **not** currently claim:

- a live TUI product surface
- full stateless downstream MCP handling
- Tasks or other future-facing post-June-2026 MCP primitives
- automated ACME / Let's Encrypt certificate management

Those are the next major architecture questions after the `v0.2.0` boundary.
