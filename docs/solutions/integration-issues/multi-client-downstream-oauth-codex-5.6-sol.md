---
title: "Use dynamic public-client OAuth for remote MCP clients"
module: "downstream-oauth"
tags: [oauth, mcp, dcr, cursor, pkce, resource-indicators]
problem_type: integration
date: 2026-07-17
---

# Use dynamic public-client OAuth for remote MCP clients

## Problem

Plug's original downstream OAuth server recognized one client ID, one optional
secret, and one operator-maintained redirect allowlist. Discovery succeeded,
but a client such as Cursor stopped because the authorization server could not
register it. Adding more hard-coded clients would have multiplied shared
secrets and callback configuration without creating real client isolation.

## Solution

Treat the issuer, not a configured client, as the durable unit. Persist a
bounded registry of public clients plus their exact redirects and separately
owned grants. Support RFC 7591 Dynamic Client Registration for clients that
need it and HTTPS Client ID Metadata Documents for clients that publish their
identity. Require explicit consent, PKCE S256, the exact MCP resource, and an
allowed scope before creating a code.

Native application callbacks require a deliberately narrow exception. Cursor
Remote Control uses `cursor://anysphere.cursor-mcp/oauth/callback`, so Plug
accepts that exact reverse-domain callback while continuing to reject every
other arbitrary custom-scheme URI.

Use a new versioned issuer state file instead of importing singular-client
tokens. That clean boundary prevents an old shared grant from silently
becoming valid for a new client model. All mutations are clone, atomically
persist with owner-only permissions, then publish in memory; a failed write
therefore cannot produce a token that disappears on restart.

## Security boundaries

- Dynamic public clients receive a high-entropy ID and no reusable secret.
- Redirects are exact per client; only HTTPS or loopback HTTP is accepted.
- Codes, access tokens, and refresh tokens carry client, scope, and resource.
- Refresh tokens rotate; client revocation removes every owned grant.
- Metadata-document fetching disables redirects, pins public DNS resolution,
  rejects local/private addresses, and limits connect time, total time, and
  response size.
- Registration is rate-limited, quota-limited, and expired when unused.

## Operational lesson

Discovery compatibility is not authorization compatibility. A successful MCP
server-card or protected-resource lookup proves only that the client found the
service. Test the complete sequence: discovery, client identification,
consent, code exchange, authenticated MCP initialization, refresh, restart,
and revocation. For desktop clients, verify the real GUI because browser,
Keychain, and system prompts do not appear in terminal-only checks.
