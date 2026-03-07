---
title: Downstream HTTPS serving
status: completed
date: 2026-03-07
---

# Downstream HTTPS Serving

## What We're Building

Add first-class HTTPS support to `plug serve` so remote clients can connect to plug securely over the public internet. The product already supports downstream Streamable HTTP and SSE semantics; this tranche adds TLS termination for the downstream server.

## Why This Approach

Remote use over plain HTTP is not acceptable. Auth tokens, session identifiers, and tool-call payloads must not traverse the network in cleartext.

This is not the same problem as outbound HTTPS to upstream servers. Outbound TLS is a dependency/runtime portability issue. Downstream HTTPS is a product feature with user-visible configuration, certificate handling, and serving behavior.

## Scope

This feature should include:
- HTTPS termination for `plug serve`
- configuration for cert/key file paths
- TLS-enabled Axum serving path
- explicit provider initialization shared by both outbound and inbound rustls users
- tests proving HTTPS startup and basic downstream request handling

This feature should not include:
- ACME / Let's Encrypt automation in the first tranche
- certificate generation UX
- reverse-proxy deployment guides beyond a short operator note

## Options Considered

### Option 1: Keep HTTP only and tell users to terminate TLS elsewhere

Rejected as the default product answer. A reverse proxy is a valid deployment option, but the product requirement is that remote clients can connect securely to plug itself.

### Option 2: Add downstream HTTPS with cert/key paths only

Chosen for the first tranche. It is the smallest honest implementation that secures remote connections without dragging in ACME and automation complexity.

### Option 3: Full ACME / Let's Encrypt integration immediately

Deferred. Useful later, but too much scope for the first secure-serving slice.

## Key Decisions

- Implement downstream HTTPS now as a dedicated feature tranche, not as a hidden extension of PR 21.
- Use the same ring-backed rustls direction for inbound and outbound TLS.
- Move crypto-provider initialization to a shared/global place once downstream HTTPS work begins.
- Start with operator-supplied certificate and key files.

## Constraints

- Keep `plug` a single binary.
- Do not reintroduce OpenSSL/native-tls.
- Do not widen license policy unnecessarily.
- Preserve current HTTP behavior for local/non-TLS use unless TLS is explicitly configured.

## Success Criteria

- `plug serve` can bind HTTPS with configured cert/key paths
- remote downstream clients can initialize and make MCP calls over HTTPS
- local HTTP behavior remains intact when TLS is not configured
- rustls provider initialization is shared correctly between outbound and inbound paths
- tests cover HTTPS startup and at least one end-to-end downstream flow

## Open Questions

None for planning. The implementation details to resolve are technical, not product-scope questions.
