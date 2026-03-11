# Downstream OAuth Design Notes

Date: 2026-03-10

## Goal

Define a clean architecture seam for downstream OAuth on `plug serve` before implementing the flow.

This work is intentionally about boundaries, not shipping the full auth lifecycle yet.

## Why A New Seam Is Needed

Current downstream HTTP auth in `plug` is bearer-oriented and historically tied to bind-address behavior.

That is insufficient for the intended production deployment:

- `plug serve` may stay loopback-bound on a Mac mini
- the service may still be internet-reachable through a stable HTTPS edge
- remote clients such as Claude custom connectors may prefer or require OAuth-oriented auth UX

So downstream auth needs its own explicit model, not an extension of loopback-vs-non-loopback heuristics.

## Responsibilities

The downstream OAuth seam should own:

- translating `HttpConfig` into a valid downstream OAuth config
- constructing advertised metadata for downstream clients
- generating authorization requests
- validating callback state
- exchanging callback results into authenticated server-side state
- linking downstream auth state to MCP session creation

The seam should not own:

- the HTTP router itself
- generic bearer-token auth
- upstream OAuth flows
- tunnel/proxy deployment

## Proposed Module Boundary

`plug-core/src/downstream_oauth/`

Initial types:

- `DownstreamOauthConfig`
- `DownstreamOauthManager`
- `DownstreamAuthChallenge`

Rationale:

- `plug-core` already owns shared runtime/auth/protocol concerns
- downstream OAuth is not a CLI-only concern
- later HTTP handlers can call into a manager rather than growing route-local ad hoc logic

## Relationship To HttpConfig

The config seam should map from:

- `http.auth_mode = "oauth"`
- `http.public_base_url`
- `http.oauth_client_id`
- `http.oauth_client_secret`
- `http.oauth_scopes`

The conversion should fail closed:

- if auth mode is not `oauth`, return no downstream OAuth config
- if required public settings are missing, return no downstream OAuth config

This keeps config parsing and runtime behavior aligned.

## Session Model

OAuth completion should not be conflated with MCP session establishment.

Instead:

1. downstream OAuth establishes authenticated user/client state
2. MCP `initialize` still creates the MCP session
3. the MCP session is associated with the authenticated identity/context

That keeps auth and protocol lifecycle distinct and easier to reason about.

## Metadata Direction

The next phase should expose metadata only when `http.auth_mode = "oauth"`.

The metadata should be derived from `http.public_base_url`, not from local bind address.

That is necessary for:

- reverse-proxy deployments
- stable tunnel deployments
- clients that rely on discovery metadata from the public URL

## Non-Goals For The Next Phase

The next OAuth phase should not include:

- full OIDC identity semantics unless required by clients
- certificate automation
- reverse proxy automation
- custom user management beyond what remote MCP clients require

## Near-Term Follow-Up

The next implementation phase should do three narrow things:

1. expose downstream OAuth metadata endpoints
2. define the auth challenge path for unauthenticated downstream requests
3. fail honestly for unsupported OAuth runtime branches rather than silently downgrading
