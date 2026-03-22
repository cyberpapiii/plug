# MCP Specification Reference

This document captures the MCP protocol details that directly affect fanout's implementation. It is not a full spec reproduction — it's a focused reference for implementors.

**Current spec version**: 2025-11-25
**Spec URL**: https://modelcontextprotocol.io/specification/2025-11-25

---

## Spec Version History

| Version | Date | Key Changes |
|---------|------|-------------|
| 2024-11-05 | Nov 2024 | Original release. stdio + SSE transports. |
| 2025-03-26 | Mar 2025 | Streamable HTTP introduced. SSE deprecated. |
| 2025-06-18 | Jun 2025 | Structured Output. Resource Links in tool results. JSON-RPC batching REMOVED. `MCP-Protocol-Version` header required. |
| 2025-11-25 | Nov 2025 | Tasks (experimental). OAuth 2.1 + PKCE. OIDC Discovery. CIMD. Icons metadata. Elicitation URL mode. Sampling with tool calling. M2M client-credentials. |

**Next anticipated**: June 2026 — likely stateless mode, Server Cards, session elevation.

---

## Wire Format: JSON-RPC 2.0

All MCP communication uses JSON-RPC 2.0, UTF-8 encoded.

### Requests (require response)
```json
{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"cursor": "optional"}}
```

### Responses
```json
{"jsonrpc": "2.0", "id": 1, "result": {"tools": [...]}}
```

### Error Responses
```json
{"jsonrpc": "2.0", "id": 1, "error": {"code": -32602, "message": "Invalid params", "data": {}}}
```

### Notifications (no response expected, no `id` field)
```json
{"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}
```

**Critical**: JSON-RPC batching was REMOVED in 2025-06-18. Never send arrays of messages.

---

## Transport: stdio

- Client launches server as subprocess
- Messages on stdin/stdout, delimited by newlines (no embedded newlines in messages)
- Server MAY write UTF-8 to stderr for logging (fanout should capture and route to its own logs)
- Server MUST NOT write non-MCP content to stdout
- Shutdown: client closes stdin → wait → SIGTERM → SIGKILL

**fanout implications**:
- `fanout connect` command reads MCP from its stdin, writes to stdout (this is what clients like Claude Code invoke)
- Upstream stdio servers are spawned as child processes via `tokio::process::Command`
- Must capture and separate stderr from stdout

---

## Transport: Streamable HTTP

Single endpoint (e.g., `http://localhost:3282/mcp`). Supports POST and GET.

### Client → Server (POST)

Every JSON-RPC message is a new HTTP POST to the MCP endpoint.

**Required headers**:
- `Content-Type: application/json`
- `Accept: application/json, text/event-stream`
- `MCP-Session-Id: <session-id>` (after initialization)
- `MCP-Protocol-Version: 2025-11-25` (after initialization)

**Server response options**:
1. For notifications/responses from client: `202 Accepted` with no body
2. For requests: `Content-Type: application/json` (single JSON response)
3. For requests: `Content-Type: text/event-stream` (SSE stream with possible server-initiated messages before final response)

### Server → Client (GET)

Client MAY open a GET request to receive server-initiated messages (notifications, requests).

**Required headers**:
- `Accept: text/event-stream`
- `MCP-Session-Id: <session-id>`
- `MCP-Protocol-Version: 2025-11-25`

Server returns `text/event-stream` or `405 Method Not Allowed`.

### Session Management

- Server assigns `MCP-Session-Id` during initialization response
- Must be globally unique, cryptographically secure, visible ASCII (0x21-0x7E)
- Client includes on ALL subsequent requests
- Missing session ID: server SHOULD return `400 Bad Request`
- Expired session: server returns `404 Not Found` → client must re-initialize
- Client sends HTTP DELETE to terminate session

### Resumability

- Servers MAY attach `id` fields to SSE events (globally unique within session)
- Client reconnects via GET with `Last-Event-ID` header
- Server MAY replay missed messages from the disconnected stream only
- Resumption always via GET regardless of how original stream started

### Security

- Servers MUST validate `Origin` header (prevent DNS rebinding)
- Invalid Origin: `403 Forbidden`
- Local servers SHOULD bind to 127.0.0.1

### Backwards Compatibility with SSE

For clients that only support old SSE (e.g., OpenCode):
- Client attempts POST `InitializeRequest` first
- If 400/404/405: fall back to GET expecting `endpoint` event (old SSE protocol)
- fanout should serve BOTH new Streamable HTTP and old SSE for maximum compatibility

---

## Session Lifecycle

### Phase 1: Initialization

Client sends:
```json
{
  "jsonrpc": "2.0", "id": 1, "method": "initialize",
  "params": {
    "protocolVersion": "2025-11-25",
    "capabilities": {
      "roots": {"listChanged": true},
      "sampling": {},
      "elicitation": {"form": {}, "url": {}}
    },
    "clientInfo": {
      "name": "Claude Code", "version": "1.0.0"
    }
  }
}
```

Server responds:
```json
{
  "jsonrpc": "2.0", "id": 1,
  "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": {
      "tools": {"listChanged": true},
      "resources": {"listChanged": true, "subscribe": true},
      "prompts": {"listChanged": true},
      "logging": {},
      "completions": {}
    },
    "serverInfo": {"name": "fanout", "version": "0.1.0"},
    "instructions": "MCP multiplexer with 25 tools from 4 servers"
  }
}
```

Client confirms:
```json
{"jsonrpc": "2.0", "method": "notifications/initialized"}
```

### Version Negotiation

- Client sends latest supported version
- Server responds with same version (if supported) or its own latest
- If incompatible: client SHOULD disconnect

**fanout**: Advertise 2025-11-25. Accept 2025-03-26 and 2024-11-05 with reduced feature set.

### Phase 2: Operation

Normal request/response/notification flow.

### Phase 3: Shutdown

- stdio: client closes stdin, server should exit
- HTTP: client sends DELETE with session ID
- fanout: on shutdown, close all upstream connections, SIGTERM child processes, wait, SIGKILL

---

## Complete Method Reference

### Client → Server Requests

| Method | fanout Behavior | Priority |
|--------|----------------|----------|
| `initialize` | Synthesize capabilities from all upstreams | P0 |
| `ping` | Respond immediately (don't forward) | P0 |
| `tools/list` | Fan-out, merge, filter, cache | P0 |
| `tools/call` | Route to correct upstream via ToolRouter | P0 |
| `resources/list` | Fan-out, merge (always return `{resources:[]}` if none) | P0 |
| `resources/read` | Route to server that owns the resource URI | P1 |
| `resources/subscribe` | Route + track subscription per client | P2 |
| `resources/unsubscribe` | Route + remove subscription | P2 |
| `resources/templates/list` | Fan-out, merge | P2 |
| `prompts/list` | Fan-out, merge | P1 |
| `prompts/get` | Route to server that owns the prompt | P1 |
| `logging/setLevel` | Forward to all upstreams | P2 |
| `completion/complete` | Route to relevant upstream | P2 |

### Server → Client Requests

| Method | fanout Behavior | Priority |
|--------|----------------|----------|
| `ping` | Respond immediately | P0 |
| `sampling/createMessage` | Forward to the client that initiated the tool call | P2 |
| `elicitation/create` | Forward to the originating client | P2 |
| `roots/list` | Forward to the originating client | P2 |

### Notifications

| Notification | fanout Behavior | Priority |
|-------------|----------------|----------|
| `notifications/tools/list_changed` | Invalidate tool cache, re-fan-out, notify ALL clients | P0 |
| `notifications/resources/list_changed` | Invalidate cache, notify all clients | P1 |
| `notifications/prompts/list_changed` | Invalidate cache, notify all clients | P1 |
| `notifications/progress` | Forward to originating client (match via request map) | P1 |
| `notifications/cancelled` | Forward cancellation upstream | P1 |
| `notifications/message` (logging) | Forward to originating client + log | P2 |
| `notifications/resources/updated` | Forward to subscribed clients | P2 |
| `notifications/roots/list_changed` | Forward to upstream (if client changes roots) | P2 |

---

## Tool Definition Format

```json
{
  "name": "create_issue",
  "title": "Create GitHub Issue",
  "description": "Create a new issue in a GitHub repository",
  "icons": [{"src": "https://github.com/favicon.ico", "mimeType": "image/x-icon"}],
  "inputSchema": {
    "type": "object",
    "properties": {
      "repo": {"type": "string", "description": "Repository in owner/name format"},
      "title": {"type": "string", "description": "Issue title"},
      "body": {"type": "string", "description": "Issue body (markdown)"}
    },
    "required": ["repo", "title"]
  },
  "outputSchema": { ... },
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": false,
    "idempotentHint": false,
    "openWorldHint": true
  }
}
```

**Tool name rules**:
- 1-128 characters
- Case-sensitive
- Allowed chars: `A-Za-z0-9_-.`
- NO dots for Claude API interop (use `_` instead)

**plug prefixing**: `create_issue` → `GitHub__create_issue` (double underscore delimiter)

**Display metadata**:
- `name` is the stable invocation identifier
- top-level `title` is the preferred UI display label
- `annotations.title` is a backward-compatibility display fallback some clients still consume

---

## Tool Call Result Format

```json
{
  "content": [
    {"type": "text", "text": "Issue #123 created"},
    {"type": "image", "data": "base64...", "mimeType": "image/png"},
    {"type": "resource_link", "uri": "file:///path", "name": "file.rs"},
    {"type": "resource", "uri": "file:///path", "mimeType": "text/plain", "text": "..."}
  ],
  "structuredContent": { ... },
  "isError": false
}
```

**fanout**: Pass through ALL content types unchanged. Including resource_link, audio, images.

---

## Upcoming Spec Changes to Design For

### Stateless Mode (June 2026, SEP-1442)

- Replace mandatory initialization handshake with self-contained requests
- Sessions become explicit data (like cookies), not transport artifacts
- Clients can optimistically attempt operations without initialization
- Load balancers gain routing visibility without JSON-RPC parsing

**fanout preparation**: Design sessions as data model concerns. Support both stateful (current) and stateless (future) initialization modes via a config flag.

### Server Cards (June 2026)

- `/.well-known/mcp.json` endpoint for pre-connection discovery
- Advertises capabilities, auth requirements, available primitives

**fanout preparation**: Serve a `/.well-known/mcp.json` that describes fanout's aggregated capabilities. Include tool count, server list, supported transports.

### Session Elevation (June 2026)

- Mid-session permission upgrades
- Client requests additional capabilities after initialization

**fanout preparation**: Don't hardcode capabilities at initialization time. Keep a mutable capability set per session.
