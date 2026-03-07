# Architecture

## System Overview

`plug` is a single Rust binary with two active front doors:

- `plug connect` for downstream stdio clients
- `plug serve` for downstream Streamable HTTP clients

Both front doors use the same `Engine` type from `plug-core`, but not always the same live instance. In normal use, `plug connect` prefers the background daemon and falls back to standalone mode if the daemon is unavailable. `plug serve` starts its own engine unless you run daemon mode explicitly.

```text
Downstream clients
  Claude Code / Cursor / Codex / Zed  ->  plug connect  -> daemon or standalone engine
  Gemini / remote HTTP clients        ->  plug serve    -> dedicated engine instance

Shared engine
  Engine
    -> ServerManager
    -> ToolRouter
    -> Config snapshot
    -> Event bus
    -> Health / recovery tasks

Upstream servers
  stdio child processes
  streamable-http upstreams
```

## Runtime Model

### Engine

`Engine` is the single owner of runtime state:

- current config snapshot
- upstream server registry
- merged tool cache
- event bus
- shutdown coordination

The CLI, daemon, and HTTP server query the engine rather than owning parallel state.

### ServerManager

`ServerManager` owns upstream server lifecycle:

- starts configured upstream servers
- tracks health/circuit-breaker state
- stores per-server semaphores
- returns server-status snapshots for CLI and daemon callers

Reads are optimized through `ArcSwap` snapshots and mutable per-server state is kept in `DashMap`.

### ToolRouter

`ToolRouter` merges and exposes tool inventory and routes `tools/call` requests to the right upstream server.

Current behavior:

- tool names are always prefixed in `v0.1`
- client-aware filtering is applied for known client caps
- `plug__search_tools` is available when the configured threshold is exceeded
- reconnect-on-session-error is handled in the tool-call path

Not yet implemented:

- notification forwarding
- pagination
- full resources/prompts routing

### Daemon

The daemon is the authoritative shared runtime for local clients:

- Unix socket IPC for CLI and `plug connect`
- auth token for admin commands
- client session registry for live downstream connections
- graceful idle shutdown controlled by config

The daemon currently proxies tool operations through IPC. It does not yet proxy the full MCP surface.

## Transports

### Downstream

- `plug connect`: stdio adapter invoked by local AI clients
- `plug serve`: Streamable HTTP server on configured bind address / port
- `DELETE /mcp`: HTTP session termination
- `GET /mcp`: SSE notification stream for HTTP clients

### Upstream

- stdio child processes
- Streamable HTTP client transport

Legacy SSE is not part of the active `v0.1` server story, although some client export formats still use legacy transport labels such as `type: sse` for compatibility.

## Configuration

Config is loaded through Figment layering:

1. defaults
2. config file
3. environment

The runtime supports:

- server add/remove/change reload
- explicit restart-required warnings for settings that are not truly hot-applied

`v0.1` does **not** promise full live reconfiguration of router/runtime semantics.

## Truthful Limitations

The current architecture intentionally does not claim:

- a live TUI
- full MCP capability parity
- complete notification forwarding
- fully stateless MCP support

Those are future work. The `v0.1` architecture is the daemon-backed CLI product plus the current shared runtime.
