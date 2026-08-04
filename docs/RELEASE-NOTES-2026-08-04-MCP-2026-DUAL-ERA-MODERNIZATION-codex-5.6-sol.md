# Plug MCP 2026 dual-era modernization

**Released on `main` — August 4, 2026**

PR #68 is merged on `main`, and the signed binary is installed and running on
this machine. The live daemon came back with 25 connected clients; Exa is
healthy with its bearer credential and eight tools.

## The short version

Plug can now grow into MCP `2026-07-28` without breaking the clients and servers
you already use. The release adds a real modern protocol path alongside the
legacy path, upgrades the Rust MCP foundation to RMCP `3.1.0`, and keeps both
modern gates off by default until real-peer conformance testing is complete.

For a user, this means a future upgrade can be introduced one client or server
at a time and rolled back with two configuration switches. For an agent, it
means modern discovery, durable tasks, trace and metadata continuity, and secure
multi-round tool interactions when both sides genuinely support them.

This is not a version-string update. Plug's custom HTTP, routing, OAuth,
ownership, task, subscription, and reverse-request layers have explicit
dual-era behavior.

## What stays the same by default

The default configuration remains legacy-compatible:

```toml
modern_upstream_enabled = false

[http]
modern_downstream_enabled = false
```

Existing server entries continue to behave as `protocol = "legacy"` when the
key is omitted. Existing Claude, Cursor, Codex, and other legacy MCP clients do
not need to be reconfigured merely because the branch contains modern support.

No old one-off Claude configuration is retained as a separate legacy product
path. Compatibility lives in a narrow protocol adapter around the same Plug
engine, rather than duplicated routing or configuration.

## What changes for users

### Safer gradual adoption

The upstream and downstream directions are controlled independently. You can
test one modern upstream while every client remains legacy, or admit a modern
HTTP client while other clients continue normally.

Each upstream chooses one of three policies:

- `protocol = "legacy"` uses only the earlier initialization lifecycle.
- `protocol = "auto"` tries modern discovery and falls back only when discovery
  is explicitly unsupported.
- `protocol = "modern"` requires modern discovery and fails instead of silently
  downgrading.

The global `modern_upstream_enabled` switch must also be true before `auto` or
`modern` can take effect. This prevents one forgotten server setting from
activating a new protocol across a production daemon.

### Reversible rollback

Turning both gates back to false restores the legacy-only posture. Per-server
protocol settings, OAuth credentials, and client registrations can stay in
place, so rollback does not create another authentication or setup project.

### Better long-running work

Modern tasks have explicit ownership, status, retrieval, cancellation, expiry,
and cleanup. A task accepted for durable execution can continue if its original
HTTP request disconnects. Only the same authorized principal can operate on it.

### Better diagnostics without trusting arbitrary metadata

Plug preserves admitted W3C tracing context and namespaced extension metadata
through the proxy. That makes one operation easier to follow across client,
Plug, and server, while strict bounds and reserved-field filtering prevent
unknown metadata from changing identity, authorization, routing, credentials,
or continuation state.

## What changes for agents

Modern agents can receive a genuine discovery-based lifecycle and protocol-era
method semantics. They are not shown a legacy session flow disguised with a new
protocol version.

Across the proven modern routes, an agent can:

- Discover the server through the modern lifecycle.
- Use durable, principal-owned tasks on supported routes; task calls targeting
  a modern upstream remain suppressed until task input-required handling is complete.
- Receive preserved result types, supported schema details, admitted extension
  metadata, and trace context.
- Pause a tool interaction for additional input and resume it through protected
  continuation state.

Legacy agents continue to receive their existing compatible surface against
legacy upstreams. A legacy client's `tools/call` into a modern upstream is
rejected before effects because the client cannot represent an unexpected
modern input request. Other mixed-era behavior is exposed only where the
specific adapter is proven.

## Secure modern-to-modern multi-round requests

The native modern path supports multi-round tool requests without exposing raw
server state as authority. Plug parks continuation data server-side and returns
an integrity-protected request-state token. That token is bound to the caller's
principal, the original request, and the selected route; it expires, is
single-use, is stored within bounded quotas, and is cleaned up on revocation or
owner teardown.

The practical result is that a different client cannot steal, replay, or reroute
another client's paused tool interaction. Cancellation and changed JSON-RPC IDs
do not bypass the ownership check.

This capability is deliberately limited to modern-to-modern routes. Plug does
not claim that a legacy client or server can participate in the same secure
continuation lifecycle yet.

## Metadata, tracing, and Apps/UI

The branch introduces a bounded extension envelope for admitted protocol data.
It can preserve namespaced extension fields, descriptor and result metadata,
schema information, and W3C `traceparent`, `tracestate`, and `baggage` values.

MCP Apps/UI-related metadata and resources can be transported as opaque data
when policy admits them. Plug does not advertise the Apps/UI capability and does
not become an app renderer. That distinction prevents clients from assuming an
interactive surface Plug cannot presently complete.

Plug also does not synthesize cache directives for merged list results. One
catalog may combine several upstreams with different freshness and principal
scope, so inventing a single TTL or cache scope would be misleading.

## Compatibility evidence

The August 3, 2026 development-machine audit found only legacy-era support in
the installed clients used for real work:

| Installed client observed | Observed version | Verified protocol posture |
| --- | --- | --- |
| Claude Code | 2.1.221 | No live `2026-07-28` negotiation proven |
| Claude Desktop | 1.24012.11 | No live `2026-07-28` negotiation proven |
| Cursor | 3.14.7 | No live `2026-07-28` negotiation proven |
| Codex CLI | 0.146.0, RMCP 1.8.0 | No verified `2026-07-28` support |

Versions move quickly, so this table is evidence for today's conservative
default, not a forecast. A newer installed client should be judged by actual
negotiation and conformance behavior.

## How to try one modern upstream

Use a canary server known to support the modern lifecycle:

```toml
modern_upstream_enabled = true

[http]
modern_downstream_enabled = false

[servers.modern-example]
transport = "http"
url = "https://example.com/mcp"
protocol = "auto"
```

Then validate and inspect health:

```sh
plug config check
plug status --output json
```

This upstream-only canary proves discovery, catalog, and health behavior. Its
tools cannot be called by a legacy downstream through this branch. Keep
downstream modern support off until a real modern client is ready. When it is,
set:

```toml
[http]
modern_downstream_enabled = true
```

The running daemon's configuration watcher can apply safe changes. If that
watcher is not active in your setup, restart Plug through your normal service
workflow.

The complete canary procedure is in the
[MCP 2026 dual-era guide](guides/mcp-2026-dual-era.md).

## How to roll back

Restore both gates to false:

```toml
modern_upstream_enabled = false

[http]
modern_downstream_enabled = false
```

Run `plug config check`, apply or reload the configuration, and confirm with
`plug status --output json`. You do not need to remove per-server `protocol`
settings, OAuth credentials, or client registrations.

## What is intentionally not enabled

- Modern downstream negotiation is gated across HTTP, stdio, and daemon IPC.
  Existing clients remain legacy unless they actually request the new protocol.
- `subscriptions/listen` is unadvertised. Its ownership and quota foundations
  exist, but the listener lifecycle is not complete.
- Legacy-downstream calls into modern upstream tools, modern-downstream calls
  into legacy upstream multi-round tools, and task-plus-modern-upstream calls
  are suppressed. Native synchronous modern-to-modern continuation is the
  supported path.
- MCP Apps/UI capability advertisement is suppressed. Opaque admitted metadata
  transport is not a promise to render an app.
- Synthesized cache hints for merged list results are suppressed because Plug
  cannot safely reconstruct their original page and principal provenance.
- Both modern gates remain off by default pending official or reference
  conformance testing plus real independent client and server testing.

These limits are user protection. Plug should never advertise a feature that an
agent can start but cannot finish through authorization, effects, cancellation,
and cleanup.

## Final review hardening

The final adversarial review found and fixed several boundary cases before the
branch was considered releasable:

- Legacy OAuth credentials without a verified issuer now fail closed and ask
  for reauthorization instead of being silently attached to a newly discovered
  authority. Startup detects that legacy file state before touching Keychain,
  so a token that cannot be admitted does not create a useless password prompt.
- Duplicate in-flight JSON-RPC IDs from the same modern principal are rejected
  atomically, so cancellation can never target the wrong operation.
- Expired or revoked principals are removed from the downstream OAuth lifecycle,
  while existing leases remain inactive.
- Task expiry aborts local work first, forwards bounded upstream cancellation,
  and retains quota until cleanup finishes.
- Authorization-required failures remain machine-readable instead of looking
  like generic upstream outages.
- Modern Host validation accepts the configured public tunnel URL without
  weakening unrelated-origin protection.
- Failed stdio discovery probes fall back cleanly to explicit legacy
  initialization instead of permanently latching a modern protocol era.
- Conformance selectors now fail when they match no tests, preventing a green
  check that exercised nothing.

The final source gate ran 981 workspace tests, Clippy with warnings denied,
format checking, Rust 1.88 compatibility checking, dependency advisory checks,
the todo-status guard, and the local MCP conformance inventory and selector
self-test. All passed.

## Release and installation posture

The release pins RMCP to exactly `3.1.0` and carries repository-level tests for
the implemented behavior. Source tests are necessary but are not sufficient to
turn a new wire protocol on by default.

Before these defaults change, Plug should pass the supported lifecycle through
an official or reference modern client and server, at least one independent
modern client and server, the real installed clients listed above, OAuth flows,
daemon restart, installed-binary health checks, and a rollback rehearsal.

The final GitHub matrix passed formatting and Clippy, macOS and Ubuntu tests,
Rust 1.88 compatibility, both cross-compilation checks, dependency advisories,
and the 10 MiB binary-size gate. The local signed install is running from
`~/.cargo/bin/plug` with the stable `Plug Local Signing` identity.

Notion, Todoist, and Krisp currently report `AuthRequired`. Their older token
files predate issuer binding, so Plug refuses to silently trust or relabel them;
each needs one explicit OAuth consent flow. Exa and the other non-OAuth
upstreams are healthy. This is a deliberate security cutover, not lost API-key
data.
