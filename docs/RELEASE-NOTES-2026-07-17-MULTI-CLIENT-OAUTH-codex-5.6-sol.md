# Plug multi-client OAuth release — July 17, 2026

Plug's remote connection flow now works like a modern MCP service: give a
compatible client `https://plug.plugtunnel.com/mcp`, approve the request, and
start using tools. You no longer have to invent a client ID, copy a secret,
maintain callback URLs in TOML, or add a permanent Authorization header.

## What changes for you

- Cursor Remote Control can discover Plug and dynamically register itself.
- Claude and future MCP clients use their own identities and grants instead of
  sharing one preconfigured account.
- Plug shows a consent page naming the client, callback host, requested scope,
  and protected resource before it grants access.
- `plug auth clients list` shows registered remote clients without exposing
  tokens. `plug auth clients revoke <client-id>` removes a client and every
  grant it owns.
- Registrations and tokens survive normal Plug restarts, while abandoned
  registrations expire automatically.

This is an intentional clean cutover. The old downstream
`oauth_client_id`, `oauth_client_secret`, and shared redirect allowlist have
been removed, and old remote grants are not imported. Existing remote clients
must authorize once after installation. Local clients that use
`plug connect` over stdio are unaffected.

## What changes for agents

Every remote agent now has a separate security boundary. One client's
authorization code, access token, refresh token, or callback cannot be reused
by another client. Tokens grant only `tools:read` against Plug's exact `/mcp`
resource, so a token issued for Plug cannot be replayed at a different service.

Refresh tokens rotate on use. Revoking a client removes its pending approvals,
codes, access tokens, and refresh tokens together, without interrupting other
clients.

## Security and standards

The release adds:

- OAuth 2.0 Dynamic Client Registration (RFC 7591) for public MCP clients;
- OAuth Client ID Metadata Documents, fetched over HTTPS with redirect,
  private-network, response-size, and timeout protections;
- OAuth authorization-server metadata (RFC 8414) and MCP protected-resource
  metadata (RFC 9728);
- mandatory PKCE S256 and exact registered redirects;
- HTTPS callbacks, correctly formed loopback HTTP callbacks, or Cursor's exact
  application-claimed native callback only;
- the OAuth `resource` parameter and resource-bound tokens (RFC 8707);
- explicit `tools:read` issuance and enforcement;
- 256-bit opaque client IDs, codes, and tokens;
- rotating refresh tokens, per-client revocation, registration quotas and
  rate limits, and 90-day unused-registration expiry;
- atomic, fail-closed, owner-only persistence. A token is never reported as
  issued if its durable state could not be secured.

Plug advertises only public-client authorization-code and refresh-token flows.
The old shared-secret and client-credentials flows are gone.

## Operational limits

- Up to 100 active downstream client registrations.
- Up to 10 new registrations per source address per hour.
- Authorization and consent requests expire after five minutes.
- Authorization codes expire after five minutes.
- Access tokens expire after one hour.
- Refresh tokens expire after 30 days and rotate on every exchange.
- Unapproved client registrations expire after one hour; approved clients
  expire after 90 idle days.

## Recovery

If a remote client loses its authorization, remove its stale connection in the
client and add `https://plug.plugtunnel.com/mcp` again. If you want to force a
clean grant from Plug's side, run:

```sh
plug auth clients list
plug auth clients revoke <client-id>
```

Then reconnect and approve the new consent request. No Plug source or TOML
changes are required.

## Verification

The release is covered by focused tests for Cursor-style registration,
multi-client isolation, authorization-code and rotating-refresh flows, PKCE,
exact and malicious redirects, resource and scope enforcement, rate limits,
quotas, expiry, persistence, owner-only permissions, revocation, discovery,
and a full local DCR-to-MCP request. The complete workspace suite, clippy with
warnings denied, formatting check, MSRV build, dependency advisories, and todo
status gate are run before installation.
