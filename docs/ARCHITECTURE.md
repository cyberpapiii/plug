# Architecture

`main` and `CHANGELOG.md` are the record of what is implemented; `docs/STATUS.md` lists open work. This
document describes the architecture of the merged system, not branch-only or historical plan state.

## System Overview

`plug` is one Rust binary. On macOS it ships inside `Plug.app`, a SwiftUI menu
bar app that registers the daemon with SMAppService, owns its lifecycle, and
talks to it over the same Unix socket the CLI uses. On Linux the binary runs on
its own.

One shared daemon serves two downstream front doors:

- `plug connect`, the stdio adapter local clients launch; it proxies to the
  daemon over a Unix socket (length-prefixed JSON)
- the daemon's Streamable HTTP server at `/mcp`, with optional TLS, bearer or
  OAuth auth, for remote clients

`plug serve` without `--daemon` runs the same HTTP server standalone in the
foreground.

```text
Plug.app (macOS)             -> owns and observes the daemon

Downstream clients
  stdio clients              -> plug connect -> Unix socket -> daemon
  HTTP / remote clients      -> daemon HTTP/HTTPS server (/mcp)
  Slack Events API (opt-in)  -> daemon HTTP/HTTPS server (/events/slack)

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
  legacy SSE upstream servers
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

The daemon is the shared runtime for both downstream stdio and downstream HTTP.
On macOS launchd runs it from the app bundle; do not start a second one by hand.

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
- reverse requests (roots, sampling, elicitation) routed to the client that
  owns the call
- tasks, owner-scoped

This applies across stdio and HTTP/HTTPS, with transport-specific details only at the edge.

## Lazy Tool Discovery

`plug` separates the canonical routed catalog from the client-visible tool surface:

- The canonical routed catalog remains global and contains every healthy upstream tool under its normal routed name.
- A per-client lazy policy chooses `standard`, `native`, or `bridge` behavior from config plus client detection.
- Bridge sessions maintain a bounded session-scoped loaded-tool working set.
- `tools/list` for bridge sessions returns `plug__search_tools` plus any real routed tools loaded into that session.
- `plug__search_tools` ranks machine-readable matches from the hidden routed catalog, loads the returned tool definitions into the session working set, and emits targeted `tools/list_changed` when the visible set changes.
- Loaded tools use the normal routed call path under their real routed names.
- Deprecated `meta_tool_mode = true` remains a legacy compatibility surface for `plug__list_servers`, `plug__list_tools`, `plug__search_tools`, and `plug__invoke_tool`; those tools are not the primary bridge UX.

This keeps one routing system while allowing clients with weak native lazy behavior, currently OpenCode by default, to avoid receiving hundreds of schemas on every initial tool discovery.

## Protocol Eras

Plug speaks two MCP lifecycles and negotiates each downstream client and each
upstream server independently:

- **Legacy** (`initialize`, sessions): the default everywhere.
- **Modern** (MCP `2026-07-28`: `server/discover`, sessionless requests,
  multi-round tool requests): opt-in through `http.modern_downstream_enabled`,
  `modern_upstream_enabled`, and per-server `protocol`.

Routing, ownership, tasks, and policy are shared; only the wire lifecycle
differs at the edges. What each pairing supports is in
[guides/mcp-2026-dual-era.md](guides/mcp-2026-dual-era.md).

## Session Model

Legacy HTTP downstream handling uses a `SessionStore` abstraction with one
concrete `StatefulSessionStore` implementation:

- HTTP lazy working sets are keyed by downstream HTTP session id
- stdio/daemon lazy working sets are keyed by downstream proxy session id

Modern downstream requests carry no session; identity comes from the
authenticated principal.

## Downstream OAuth

In `auth_mode = "oauth"` the daemon is its own authorization server: dynamic
client registration and client ID metadata documents, PKCE, rotating refresh
tokens, per-method-family scopes, and an owner passkey gate on consent. State is
an owner-only file per issuer. Details are in
[OPERATOR-GUIDE.md](OPERATOR-GUIDE.md).

## Slack Event Adapter

`plug-core/src/slack_events/` is the one place Plug originates data instead of
passing MCP through. When `[http.slack_events]` is configured, the HTTP server
accepts Slack's Events API at `/events/slack`, filters messages, queues them
durably, and a worker delivers `slack.ditto_message` to one downstream OAuth
client by signed webhook. That client manages its subscription with the modern
`events/list`, `events/subscribe`, and `events/unsubscribe` methods.

The adapter is Slack-specific by construction: one upstream name, one event,
one subscriber. It is not an event bus for other servers. See
[slack-mcp-events.md](slack-mcp-events.md).

## Honest Limitations

The architecture does **not** currently claim:

- `subscriptions/listen` or mixed-era multi-round tool requests
- event pass-through from upstream servers, or any event source besides Slack
- fully live runtime reconfiguration; some changes need a restart
- automated ACME / Let's Encrypt certificate management
- a macOS command-line install separate from `Plug.app`

Those remain out of scope.
