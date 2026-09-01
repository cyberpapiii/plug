# Stateless MCP Design Notes

## Purpose

Document where a future stateless downstream mode would integrate without implementing it yet.

## Current Shape

Today, downstream HTTP traffic assumes:

- explicit `initialize`
- server-issued session IDs
- in-memory session storage
- SSE sender attachment per session

That is a stateful downstream model.

## Future Stateless Entry Points

A stateless downstream mode would likely enter through:

- HTTP requests that carry enough client/discovery context per request
- capability discovery instead of initialization as the primary contract
- `/.well-known/mcp.json` as the discovery surface for stateless clients

## Likely Integration Boundary

The intended seam is now:

- `HttpState.sessions: Arc<dyn SessionStore>`

That means a future `StatelessSessionStore` would be responsible for:

- deriving a request-scoped identity from headers/request context
- treating validation as “derive or reject” instead of “look up session ID”
- deciding what, if anything, becomes queueable notification state

## Key Constraints

1. stateless downstream does not remove the need to bridge to stateful upstream stdio servers
2. SSE and targeted notifications become more complex when there is no durable downstream session ID
3. capability discovery must replace assumptions currently tied to `initialize`

## Non-Goals For This Tranche

- no stateless request handling implementation
- no `/.well-known/mcp.json` behavioral expansion beyond today
- no remote auth/session model
