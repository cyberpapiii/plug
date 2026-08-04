# MCP 2026 dual-era protocol guide

> Development-branch status: this guide describes code on the MCP modernization
> branch as of August 4, 2026. It is not a claim about `main`, a published
> release, or the Plug binary currently installed on your machine.

Plug is becoming an MCP `2026-07-28` gateway without forcing every client and
server to upgrade at the same time. The modern path is explicit and opt-in; the
legacy path remains the default because the clients observed on the development
machine still use the earlier lifecycle.

## What dual-era means

Plug negotiates the downstream client and each upstream server independently.
The routing, ownership, task, subscription, and policy engines remain shared.
Only the wire-level lifecycle is adapted at the edges.

In this branch, modern downstream negotiation is gated across HTTP, stdio, and
daemon IPC. Existing clients still negotiate the legacy lifecycle by default;
enabling the gate adds the modern option instead of forcing a cutover.

| Downstream client | Upstream server | Branch behavior |
| --- | --- | --- |
| Legacy | Legacy | Preserve the current `initialize`-based behavior. |
| Legacy | Modern | Catalog negotiation can succeed, but `tools/call` is rejected before upstream effects because a legacy client cannot represent an unexpected modern input request. |
| Modern | Legacy | Translate ordinary supported operations and local task wrapping; suppress modern multi-round continuation. |
| Modern | Modern | Use discovery, modern transport semantics, and secure native synchronous multi-round continuations when policy admits them; task calls into modern upstreams remain suppressed. |

This compatibility layer is not a second copy of Plug. Legacy and modern
clients use the same internal routing and ownership rules, which keeps behavior
consistent and makes legacy support removable later without replacing the core.

## Defaults and installed-client compatibility

Both modern gates default to off:

```toml
modern_upstream_enabled = false

[http]
modern_downstream_enabled = false
```

With those defaults, existing client and server configurations continue using
the legacy lifecycle. No existing server needs a new `protocol` key.

The compatibility audit performed on August 3, 2026 found these installed
clients using legacy-era protocol support:

| Client observed | Version observed | Highest verified protocol |
| --- | --- | --- |
| Claude Code | 2.1.221 | No live `2026-07-28` negotiation proven |
| Claude Desktop | 1.24012.11 | No live `2026-07-28` negotiation proven |
| Cursor | 3.14.7 | No live `2026-07-28` negotiation proven |
| Codex CLI | 0.146.0, RMCP 1.8.0 | No verified `2026-07-28` support |

That is a dated test snapshot, not a permanent compatibility promise. Client
updates may change it. Plug's selected-protocol telemetry should be the source
of truth when evaluating a newer client.

## Configuration reference

| Key | Default | Meaning |
| --- | --- | --- |
| `modern_upstream_enabled` | `false` | Global permission for any upstream to negotiate the modern lifecycle. When false, every per-server modern policy is held to legacy behavior. |
| `http.modern_downstream_enabled` | `false` | Allows modern downstream clients on Plug's HTTP endpoint. It does not force legacy clients to become modern. |
| `servers.<name>.protocol` | `"legacy"` | Selects `legacy`, `auto`, or `modern` for one upstream server. |

The canonical per-server key is `protocol`. The older internal spelling
`protocol_mode` is accepted as a configuration alias, but new configuration
should use `protocol`.

The per-server values are:

- `legacy`: use only the earlier `initialize` lifecycle.
- `auto`: try `server/discover`, then fall back only when the upstream returns
  the JSON-RPC method-not-found error.
- `modern`: require `server/discover`; do not fall back to legacy behavior.

Plug on this branch pins RMCP exactly to `3.1.0`. That SDK foundation supports
both protocol eras, but the branch still uses explicit Plug gates and tests
because the SDK upgrade alone cannot validate Plug's routing, ownership, OAuth,
task, subscription, and reverse-request behavior.

## Safe activation

Start with one known modern upstream. Do not enable every server at once. An
upstream-only canary proves discovery, catalog, and health behavior; legacy
clients cannot call that modern upstream's tools through this branch.

1. Back up `~/.config/plug/config.toml`.
2. Add the global upstream gate and set one HTTP server to `auto`:

   ```toml
   modern_upstream_enabled = true

   [http]
   modern_downstream_enabled = false

   [servers.modern-example]
   transport = "http"
   url = "https://example.com/mcp"
   protocol = "auto"
   ```

3. Validate before saving the final change:

   ```sh
   plug config check
   ```

4. Let the running daemon's configuration watcher apply the change, or restart
   Plug through your normal service workflow if the watcher is not active.
5. Check health and the selected protocol:

   ```sh
   plug status --output json
   ```

6. Exercise discovery, catalogs, resources, prompts, and authentication against
   the canary server before changing another server. Tool calls require a
   modern downstream canary as well.

Enable modern downstream HTTP only when you have a real modern client to test:

```toml
[http]
modern_downstream_enabled = true
```

This does not disable legacy downstream clients. It admits the additional
modern lifecycle on the HTTP endpoint.

## Rollback

Rollback is intentionally independent of server credentials and registrations.
Set both gates to false and validate the configuration:

```toml
modern_upstream_enabled = false

[http]
modern_downstream_enabled = false
```

```sh
plug config check
plug status --output json
```

Per-server `protocol` values may remain in the file; the global upstream gate
overrides them. OAuth credentials and client registrations do not need to be
deleted. This makes a rollback reversible instead of turning it into a fresh
setup.

## Practical impact for users

- Existing Claude, Cursor, and Codex connections retain their current behavior
  while the gates are off.
- A single modern server or client can be tested without migrating the other
  direction or the rest of the fleet.
- Modern tasks can continue after an HTTP request disconnects, then be queried
  or cancelled by the same authorized principal.
- Tracing and admitted extension metadata survive more proxy boundaries, making
  failures easier to correlate without trusting arbitrary upstream fields.
- Unsupported capabilities remain hidden, so a client does not begin a flow
  Plug cannot finish.

## Practical impact for agents

- A modern agent receives modern discovery and method semantics rather than a
  rewritten version string over the legacy lifecycle.
- Modern task ownership gives long-running work an explicit lifecycle instead
  of tying it to one HTTP request.
- Native modern-to-modern multi-round tool requests can pause for additional
  input and resume securely.
- Catalog behavior is deterministic and capability advertisement is conservative,
  reducing plans based on features that are not actually usable.
- Legacy agents continue to use the compatible surface they already understand.

## Modern tasks

The modern task path is policy-gated and owner-scoped. Tool access requires the
appropriate tool scope, task operations require task permission, and durable
operations require a stable authenticated identity. Task records are not shared
between principals. Cancellation, expiration, client revocation, and owner
teardown participate in cleanup.

Request disconnect and task cancellation are different events: disconnecting a
request does not silently abandon an accepted durable task, while an authorized
task cancellation is forwarded through the bounded upstream cancellation path.

## Native modern-to-modern multi-round tool requests

Plug supports multi-round tool continuations only when both sides use the modern
protocol. Continuation state is stored server-side and represented to the client
by an integrity-protected token bound to the authenticated principal, original
request, and selected route. It expires, is single-use, has bounded storage, and
is cleaned up on revocation or teardown.

This prevents another client from replaying or redirecting a continuation. It
also avoids treating a changed JSON-RPC request ID as proof of identity.

Mixed-era continuation bridging remains suppressed. A legacy-to-modern or
modern-to-legacy flow is not advertised as supported because Plug cannot yet
guarantee that every pause, reply, cancellation, and retry will complete without
stranding the request.

## Metadata and tracing

The modern adapter preserves admitted namespaced extension metadata, result and
descriptor metadata, supported schema information, and W3C `traceparent`,
`tracestate`, and `baggage` context. Unknown metadata is constrained by size,
nesting, and value-count limits.

Plug-reserved, authorization, credential, secret-shaped, identity, routing, and
continuation-control fields are stripped or rejected. Preserved metadata cannot
change who the caller is, what they may access, where a request routes, or which
continuation it resumes.

MCP Apps/UI metadata may pass through as opaque policy-limited data, but Plug
does not advertise the Apps/UI capability and does not render applications.

## Honest limitations

- Modern downstream negotiation is available behind the same global gate for
  HTTP, stdio, and daemon IPC; real installed clients remain on the legacy path
  until they actually request the modern protocol.
- `subscriptions/listen` remains unadvertised and unavailable. The ownership
  and quota foundations are not the same as a completed listener lifecycle.
- Legacy-downstream calls into modern upstream tools are rejected before
  effects. Modern-downstream calls into legacy upstream multi-round tools and
  task-plus-modern-upstream calls are also suppressed. Only native synchronous
  modern-to-modern continuation is supported.
- MCP Apps/UI capability advertisement is suppressed even when related metadata
  is preserved.
- Plug does not synthesize list-result cache directives. A merged catalog spans
  multiple upstreams and cannot safely invent one TTL or cache scope after the
  original page and principal provenance has been lost.
- Both modern gates remain off by default until official or reference
  conformance peers and at least one independent real client and server validate
  the complete lifecycle.

These are deliberate capability boundaries, not silent fallbacks. Plug should
advertise a modern feature only when it can carry the operation through effects,
cancellation, authorization, and cleanup.

## Evidence required before changing the defaults

Default-on should require more than source tests:

- A reference or official MCP `2026-07-28` client and server both complete the
  supported lifecycle through Plug.
- At least one independent modern client and upstream server pass the same
  tests.
- Negotiation logs prove the requested and selected versions in both directions.
- Tools, resources, prompts, tasks, cancellation, OAuth, metadata, and native
  multi-round requests pass without cross-principal leakage or legacy
  regression.
- The operator rehearses the two-gate rollback and verifies the installed
  daemon afterward.

Until those checks pass on a released and installed build, the safe default is
the existing legacy lifecycle.
