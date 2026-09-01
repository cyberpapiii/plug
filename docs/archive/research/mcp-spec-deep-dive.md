# MCP Specification Deep Dive

Research for the Plug MCP multiplexer project. All findings cite specific spec sections or URLs.

**Spec version analyzed**: 2025-11-25
**Date of research**: 2026-03-03

---

## 1. Is `inputSchema` REQUIRED or OPTIONAL in `tools/list` Responses?

**ANSWER: REQUIRED.**

The authoritative JSON schema for the 2025-11-25 spec defines the `Tool` type with a `required` array of `["name", "inputSchema"]`.

Source: [schema/2025-11-25/schema.json](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2025-11-25/schema.json) -- the Tool definition:

```json
{
  "required": ["name", "inputSchema"],
  "properties": {
    "name": { "type": "string" },
    "inputSchema": {
      "description": "A JSON Schema object defining the tool's input parameters.",
      "type": "object"
    },
    "description": { "type": "string" },
    "title": { "type": "string" },
    "outputSchema": { ... },
    "annotations": { ... },
    "icons": { ... }
  }
}
```

The spec's Data Types section for Tools (https://modelcontextprotocol.io/specification/2025-11-25/server/tools) lists `inputSchema` without the word "Optional" (unlike `title`, `outputSchema`, `annotations`, and `icons` which are explicitly marked optional). The spec further states:

> `inputSchema`: JSON Schema defining expected parameters
> - **MUST** be a valid JSON Schema object (not `null`)
> - For tools with no parameters, use: `{ "type": "object", "additionalProperties": false }`

**Impact on token efficiency strategy**: You CANNOT omit `inputSchema` from `tools/list` responses and remain spec-compliant. Even tools with no parameters must include `{ "type": "object" }` at minimum. The `description` field, however, is NOT in the `required` array per the JSON schema (only `name` and `inputSchema` are required), though the spec prose lists it without "Optional". The TypeScript schema at `schema.ts` appears to mark it as required too, but the JSON schema is authoritative.

**Fields that ARE optional** (can be omitted to save tokens):
- `title`
- `description` (per JSON schema `required` array -- only `name` and `inputSchema` are required)
- `outputSchema`
- `annotations`
- `icons`

**Source**: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2025-11-25/schema.json, https://modelcontextprotocol.io/specification/2025-11-25/server/tools

---

## 2. Is Session State Per-Session or Per-Request?

**ANSWER: Per-session. Multiple clients CANNOT share one upstream session.**

### Evidence from `logging/setLevel`

The `logging/setLevel` request (https://modelcontextprotocol.io/specification/2025-11-25/server/utilities/logging) sets a minimum log level. The spec's message flow diagram shows:

```
Client->>Server: logging/setLevel (info)
... Server sends info/warning/error ...
Client->>Server: logging/setLevel (error)
... Server only sends error level and above ...
```

There is no per-request scoping. The `setLevel` call mutates server-side state that persists for the duration of the session. If Client A sets `debug` and Client B (hypothetically sharing the session) sets `error`, the server would have no way to differentiate -- it would apply the last-set level to all notifications.

### Evidence from `resources/subscribe`

The `resources/subscribe` mechanism (https://modelcontextprotocol.io/specification/2025-11-25/server/resources) creates a persistent subscription. When a subscribed resource changes, the server sends `notifications/resources/updated` to the subscribing client. This is inherently per-session state -- the server must track which URIs each session has subscribed to.

### Evidence from Capability Negotiation

The lifecycle spec (https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle) makes clear that capabilities are negotiated once during initialization and persist for the session:

> "During the operation phase, the client and server exchange messages according to the negotiated capabilities. Both parties MUST: Respect the negotiated protocol version; Only use capabilities that were successfully negotiated."

### Evidence from Session Management

The transports spec (https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#session-management) defines:

> "An MCP 'session' consists of logically related interactions between a client and a server, beginning with the initialization phase."

Each session has its own `Mcp-Session-Id`. There is no mechanism for multiple clients to share a session ID.

**Multiplexer implication**: If Client A and Client B both connect through our multiplexer to the same upstream server, they need separate upstream sessions (or the multiplexer must maintain per-downstream-client state and translate). If Client A calls `logging/setLevel("debug")`, only Client A's logs should be affected. The multiplexer must either:
1. Maintain separate upstream sessions per downstream client, OR
2. Maintain a single upstream session but track per-client state internally (e.g., filter log notifications by each client's requested level, maintain per-client subscription sets)

---

## 3. Streamable HTTP `Mcp-Session-Id` Lifecycle

**Source**: https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#session-management

### Assignment

1. The server **MAY** assign a session ID during initialization by including `Mcp-Session-Id` in the HTTP response header of the `InitializeResult`.
2. The session ID **SHOULD** be globally unique and cryptographically secure (e.g., UUID, JWT, cryptographic hash).
3. The session ID **MUST** only contain visible ASCII characters (0x21 to 0x7E).

### Client Obligation

4. If the server returned an `Mcp-Session-Id`, the client **MUST** include it in the `Mcp-Session-Id` header on **all** subsequent HTTP requests.
5. Servers that require a session ID **SHOULD** respond to requests without `Mcp-Session-Id` (other than initialization) with **HTTP 400 Bad Request**.

### Expiration / Termination

6. **Server-initiated**: The server **MAY** terminate the session at any time. After termination, it **MUST** respond to requests containing that session ID with **HTTP 404 Not Found**.
7. **Client detection**: When a client receives **HTTP 404** in response to a request containing an `Mcp-Session-Id`, it **MUST** start a new session by sending a new `InitializeRequest` without a session ID.
8. **Client-initiated**: Clients **SHOULD** send an **HTTP DELETE** to the MCP endpoint with the `Mcp-Session-Id` header to explicitly terminate the session. The server **MAY** respond with **HTTP 405 Method Not Allowed** if it does not support client-initiated termination.
9. The server **MAY** also terminate an SSE stream if the session expires.

### Protocol Version Header

Additionally, the client **MUST** include `MCP-Protocol-Version: 2025-11-25` on all subsequent requests (https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#protocol-version-header). If the server does not receive this header and cannot determine the version from initialization, it **SHOULD** assume `2025-03-26`.

### Complete Lifecycle Sequence

```
Client  -> POST /mcp  InitializeRequest (no session ID)
Server  <- 200 OK     InitializeResult + Mcp-Session-Id: abc123
Client  -> POST /mcp  InitializedNotification + Mcp-Session-Id: abc123
Server  <- 202 Accepted
...normal operations with Mcp-Session-Id: abc123...
Client  -> DELETE /mcp  Mcp-Session-Id: abc123  (graceful shutdown)
Server  <- 200 OK (or 405 if unsupported)
```

**Multiplexer implication**: The multiplexer must manage upstream session IDs (tracking them per upstream connection) and downstream session IDs (issuing its own to downstream clients). When an upstream session returns 404, the multiplexer must re-initialize and re-establish state.

---

## 4. Legacy SSE Backwards Compatibility Procedure

**Source**: https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#backwards-compatibility

### Server-side Backward Compat

Servers wanting to support older clients should:
- Continue hosting both the SSE and POST endpoints of the old transport alongside the new MCP endpoint.
- It is also possible to combine the old POST endpoint and the new MCP endpoint, but "this may introduce unneeded complexity."

### Client-side Fallback Procedure (exact steps)

Clients wanting to support older servers should:

1. Accept an MCP server URL from the user (may point to either old or new transport).
2. **Attempt to POST an `InitializeRequest`** to the server URL, with an `Accept` header listing both `application/json` and `text/event-stream`.
3. **If it succeeds**: The client can assume this is a server supporting the new Streamable HTTP transport.
4. **If it fails with HTTP 400 Bad Request, 404 Not Found, or 405 Method Not Allowed**:
   - Issue a **GET request** to the server URL, expecting it will open an SSE stream.
   - Expect an `endpoint` event as the **first SSE event**.
   - When the `endpoint` event arrives, the client can assume this is a server running the old HTTP+SSE transport.
   - Use the old transport for all subsequent communication.

**NOTE**: The 2025-11-25 spec lists specific failure codes: "400 Bad Request", "404 Not Found", or "405 Method Not Allowed". The slightly newer 2025-06-18 spec simplifies this to "an HTTP 4xx status code (e.g., 405 Method Not Allowed or 404 Not Found)".

**Multiplexer implication**: The multiplexer should implement this exact fallback when connecting to upstream servers. POST first with both Accept types; on 4xx, fall back to GET for SSE with endpoint event detection.

---

## 5. Origin Header Validation

**Source**: https://modelcontextprotocol.io/specification/2025-11-25/basic/transports (Security Warning section)

### What the Spec Says

The spec's Security Warning for Streamable HTTP states:

> 1. Servers **MUST** validate the `Origin` header on all incoming connections to prevent DNS rebinding attacks
>    - If the `Origin` header is present and invalid, servers **MUST** respond with HTTP 403 Forbidden. The HTTP response body **MAY** comprise a JSON-RPC error response that has no `id`.
> 2. When running locally, servers **SHOULD** bind only to localhost (127.0.0.1) rather than all network interfaces (0.0.0.0)
> 3. Servers **SHOULD** implement proper authentication for all connections

### What the Spec Does NOT Say

The spec does **not** provide specific rules for:
- What Origins are valid for localhost (e.g., `http://localhost:3000`, `http://127.0.0.1:8080`)
- How `.localhost` subdomains should be handled
- Whether `null` Origin should be accepted or rejected
- A whitelist of acceptable Origin patterns

**UNCONFIRMED**: The spec leaves the exact Origin validation rules to implementations. It only mandates that validation MUST happen and invalid Origins MUST get 403.

### Implementation Guidance from Python SDK

The official MCP Python SDK (https://github.com/modelcontextprotocol/python-sdk) implements DNS rebinding protection with:
- `TransportSecuritySettings` with `allowed_hosts` and `allowed_origins` parameters
- Port wildcarding support (e.g., `localhost:*`)
- Invalid hosts/origins get **421 Misdirected Request**
- Can be disabled for development with `enable_dns_rebinding_protection=False`

### Recommended Approach for the Multiplexer

Based on the spec requirements and SDK implementation patterns:

1. **MUST** validate Origin on all incoming HTTP requests.
2. **For localhost servers**: Accept Origins matching `http://localhost:<port>`, `http://127.0.0.1:<port>`, `http://[::1]:<port>`.
3. **`.localhost` subdomains**: Per RFC 6761, `.localhost` and its subdomains (e.g., `foo.localhost`) resolve to loopback. These SHOULD be accepted for local servers. UNCONFIRMED whether the spec intends this.
4. **`null` Origin**: This is sent by privacy-sensitive contexts (redirects, file:// URLs, sandboxed iframes). **Recommendation**: Reject `null` Origin by default. Accepting it opens an attack vector where malicious pages in sandboxed iframes could access local servers.
5. **Remote servers**: Only accept the Origin matching the server's own origin, or Origins explicitly configured as allowed.

---

## 6. MCP Transport Future

**Source**: https://blog.modelcontextprotocol.io/posts/2025-12-19-mcp-transport-future/

### Key Changes Coming

1. **Stateless Protocol Design**: The protocol will move toward statelessness by default. The initialization handshake will be replaced with per-request information. Discovery will be available via a separate mechanism. This eliminates sticky sessions.

2. **Session Management Redesign**: Sessions will migrate from implicit transport-level constructs to **explicit data model elements**. The proposal explores "cookie-like mechanisms" to decouple sessions from transport, mirroring standard HTTP patterns where applications manage stateful semantics independently.

3. **Elicitations and Sampling Optimization**: Server requests will function similarly to chat APIs -- servers return elicitation requests, and clients respond with both request and response together, enabling stateless reconstruction.

4. **Subscription Model Overhaul**: Replacing general `GET` streams with explicit subscription streams. Adding **TTL values** and **ETags** for intelligent caching decisions independent of notifications.

5. **HTTP Optimization**: Routing information will be exposed via **standard HTTP headers and paths** rather than requiring JSON body parsing by infrastructure. This is critical for load balancers and API gateways.

6. **Server Cards**: New metadata documents at `/.well-known/mcp.json` will enable capability discovery before connection establishment. This enables autoconfiguration, automated discovery, static security validation, and reduced latency for UI hydration.

### Timeline

- SEPs (Specification Enhancement Proposals) target **Q1 2026** completion.
- Specification release tentatively scheduled for **June 2026**.
- Official transports remain: **STDIO** (local) and **Streamable HTTP** (remote) only.

### Multiplexer Implication

The move toward statelessness is favorable for our multiplexer design. Cookie-like session management and HTTP header-based routing would simplify multiplexing significantly. However, we must design for both the current stateful model AND the coming stateless model. The subscription model changes (TTL + ETag) could enable much more efficient caching in the multiplexer.

---

## 7. MCP Roadmap

**Source**: https://modelcontextprotocol.io/development/roadmap (last updated 2025-10-31)

### Priority Areas for the November 2025 Release (completed)

The roadmap page as fetched was last updated 2025-10-31 and focused on the **2025-11-25 release**. It outlined:

1. **Asynchronous Operations** -- Adding async support for long-running tasks. Tracked in [SEP-1686](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1686).

2. **Statelessness and Scalability** -- Addressing horizontal scaling challenges. Focus: [SEP-1442](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1442).

3. **Server Identity** -- Servers advertising via `.well-known` URLs. Working across multiple industry projects on a common standard for "agent cards."

4. **Official Extensions** -- Officially recognizing popular protocol extensions for specific industries (healthcare, finance, education).

5. **SDK Support Standardization** -- Tiering system for SDKs based on spec compliance, maintenance, and feature completeness.

6. **MCP Registry General Availability** -- The [MCP Registry](https://github.com/modelcontextprotocol/registry) launched preview September 2025, progressing toward GA.

7. **Validation** -- Reference client/server implementations, compliance test suites.

### June 2026 Plans

The roadmap page itself does not explicitly mention June 2026. However, based on the transport future blog post (Question 6), the **next spec release is tentatively scheduled for June 2026**, which would incorporate the stateless protocol changes, session management redesign, server cards, and subscription model overhaul described in the transport future blog post.

**UNCONFIRMED**: The roadmap page has not been updated since October 2025. The June 2026 timeline comes from the December 2025 blog post. There is no dedicated roadmap page for the June 2026 release as of this research date.

---

## 8. SEP-1576: Token Bloat Mitigation

**Source**: https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1576

### Status

- **Status**: Open / Proposal (carries "SEP-requested" label)
- **Type**: Standards Track
- **Created**: 2025-09-30
- **Authors**: Zeze Chang, Jinyang Li, Zhen Cao (Huawei)
- **Accepted**: **No. This is NOT accepted.** It remains an open proposal.

### Problem

AI agents consume significantly more tokens than chatbots, with tool invocation via MCP being a dominant reason. Analysis of the official GitHub MCP server's 60 tools found:
- 60% of tools share an identical `owner` field definition
- 65% share an identical `repo` field definition
- Massive schema redundancy across tool definitions

### Four Proposed Mechanisms

1. **Schema Deduplication via JSON `$ref`**: Use JSON Schema `$ref` references to eliminate redundant parameter definitions across tools. Instead of repeating the `owner` schema in 36 tools, define it once and reference it.

2. **Adaptive Optional Fields**: Servers conditionally return optional content (like `outputSchema`) based on client requests. Prevents unnecessary token consumption when not needed.

3. **Response Granularity Levels**: Servers adjust verbosity -- returning simple, unstructured results for lightweight requests versus detailed, schema-conformant responses for complex tasks.

4. **Embedding-Based Tool Selection**: Implement similarity matching to filter tool lists before sending to LLMs. Return only top-k relevant tools based on the query, reducing cognitive load and token usage.

### Backward Compatibility

The proposal explicitly states it introduces "no backward incompatibilities."

### Discussion Highlights

Recent comments mention complementary work on output compression (structured JSON instead of verbose CLI text achieving 70-90% token reduction) and alternative filtering approaches like BM25 scoring.

### Multiplexer Relevance

This is highly relevant. Even without spec changes, the multiplexer could implement:
- Tool deduplication / `$ref`-based compression on the tools/list pass-through
- Client-side filtering (top-k tool selection) to reduce what's sent to the LLM
- Caching and diffing of tool schemas to minimize re-transmission

---

## 9. SEP-1442: Stateless Mode

**Source**: https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1442

### Status

- **Status**: Draft
- **Type**: Standards Track
- **Authors**: Jonathan Hefner, Mark Roth, Shaun Smith, Harvey Tuch, Kurtis Van Gent
- **Accepted**: **Not yet accepted.** It remains a draft. 84 comments of discussion.

### Core Problem

The current MCP specification requires a mandatory initialization handshake that establishes persistent session state. This causes three critical issues:

1. **Scalability**: Stateful connections cannot use simple load balancers. Requires sticky sessions, creating infrastructure complexity and uneven load.
2. **Fault Tolerance**: Server instance failure loses all session state. Requires expensive reconnection and re-initialization.
3. **Implementation Complexity**: Both server-side (per-client session state) and client-side (persistent connections, reconnection handling) impose significant burdens.

### Proposed Changes to Session Management

#### Initialization Unbundled

Instead of a monolithic 3-way handshake (`initialize` -> `InitializeResult` -> `initialized`), the proposal decentralizes it:

- **Protocol Version**: Included with every request via HTTP headers (`MCP-Protocol-Version`) or in request `_meta` fields.
- **Server Discovery**: New optional `server/discover` RPC allows clients to query capabilities without establishing a session.
- **Per-Request Capabilities**: Clients specify capabilities individually rather than negotiating once.

#### Sessions Become Optional

- Clients MAY perform initialization to create a stateful session (receiving a `sessionId`).
- Sessions do not persist across connections unless explicitly maintained.
- Session IDs pass via HTTP headers (`Mcp-Session-Id`) or `_meta` fields.
- Sessions are "pay as you go" -- complexity introduced only when needed.

#### Deprecated Elements

The proposal removes:
- `logging/setLevel` RPC (replaced by per-request log level in `_meta`)
- `notifications/roots/list_changed` (replaced by server-initiated streaming)
- `notifications/initialized` (treated as no-op for backward compatibility)

#### New Error Codes

- **-32000**: Unsupported protocol version (includes list of supported versions)
- **-32001**: Invalid or required session ID

### Design Principles

The proposal follows a "pay as you go" philosophy:
1. **Statelessness**: Requests should be self-contained, requiring no retained server state.
2. **State References**: When statelessness is impractical, state references pass in every request.
3. **Statefulness as Last Resort**: Complex stateful logic accepted only for critical use cases.

### Backward Compatibility

This is a **fundamental, backward-incompatible change** requiring a new protocol version. However, dual-version servers could support both old and new clients.

### Discussion Highlights

The original plans for dedicated `sessions/create` and `sessions/delete` RPCs were removed "due to lack of consensus." The core stateless-first changes proceed while explicit session management is deferred.

### Multiplexer Relevance

This is the single most impactful SEP for our multiplexer:

**If adopted**, the multiplexer design simplifies dramatically:
- No need to maintain per-client upstream sessions
- Can use standard HTTP load balancing to upstream servers
- Per-request `_meta` carries all context needed
- `logging/setLevel` goes away (solved per-request)
- Tool/resource discovery can be cached and served without upstream sessions

**Current design should**: Build for the stateful model today but architect internal abstractions that can trivially switch to stateless mode when the spec changes. Specifically:
- Abstract session management behind a trait/interface
- Keep per-request context passing as the internal model even if external protocol is stateful
- Design the upstream connection pool to work both with sticky sessions and round-robin

---

## Summary Table

| Question | Key Finding | Confirmed? |
|----------|------------|------------|
| `inputSchema` required? | **Yes**, per JSON schema `required: ["name", "inputSchema"]` | CONFIRMED |
| Session state scope | **Per-session**, not per-request | CONFIRMED |
| `Mcp-Session-Id` lifecycle | Assigned at init, 404 = expired, client re-inits | CONFIRMED |
| Legacy SSE fallback | POST init first; on 400/404/405, GET for SSE `endpoint` event | CONFIRMED |
| Origin validation rules | MUST validate, MUST 403 on invalid; specific rules unspecified | PARTIALLY CONFIRMED |
| Transport future | Stateless default, cookie-like sessions, server cards, June 2026 target | CONFIRMED |
| June 2026 roadmap | Stateless protocol + session redesign + server cards (from blog, not roadmap page) | CONFIRMED via blog |
| SEP-1576 token bloat | Open proposal; `$ref` dedup, adaptive fields, top-k selection | CONFIRMED (not accepted) |
| SEP-1442 stateless mode | Draft; unbundles init, optional sessions, removes `setLevel` | CONFIRMED (not accepted) |
