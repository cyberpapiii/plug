---
title: "RMCP Streamable HTTP authentication requires raw bearer tokens"
date: 2026-07-13
category: integration-issues
module: plug-core/src/server
problem_type: integration_issue
component: authentication
symptoms:
  - "Static-token-authenticated Streamable HTTP upstreams returned unauthorized after the RMCP 2.2 upgrade"
  - "The emitted Authorization header contained two Bearer prefixes"
  - "Unauthenticated upstreams continued to connect, making the failure look provider-specific"
root_cause: wrong_api
resolution_type: code_fix
severity: high
related_components:
  - "RMCP Streamable HTTP client"
  - "OAuth token retrieval"
tags:
  - rmcp
  - streamable-http
  - bearer-auth
  - oauth
  - dependency-upgrade
---

# RMCP Streamable HTTP authentication requires raw bearer tokens

## Problem

After Plug upgraded to the exact `rmcp = "=2.2.0"` release, static-token-authenticated Streamable HTTP upstreams could no longer initialize. Exa exposed the shared formatting defect; restoring a valid credential alone could not correct the malformed wire header.

Plug treated `StreamableHttpClientTransportConfig::auth_header` as though it accepted a complete HTTP `Authorization` value. The static-token path constructed `Bearer <token>` and passed that string to RMCP. RMCP's reqwest transport treats the configured value as a raw bearer token and adds the scheme itself, so the request went out as:

```text
Authorization: Bearer Bearer <token>
```

## Symptoms

- Static-token-authenticated Streamable HTTP upstreams failed during connection or initialization even when their credentials were present.
- Restoring the same valid credential did not help because every request still used the malformed header.
- Unauthenticated upstreams did not exercise the malformed-header branch.
- The credential could be correct at rest while its wire representation was wrong.

## What Didn't Work

- Re-entering or rotating the provider key could repair a missing or revoked secret, but not deterministic double formatting.
- Treating `auth_header` as a literal-header API delegated prefix ownership to both Plug and RMCP.
- Changing downstream bearer verification would address the wrong side of the proxy. Incoming clients send a complete `Authorization` header; outgoing RMCP transport configuration receives raw token material.
- Green workspace gates, a responsive daemon, and aggregate tool counts were not enough to prove this path. They did not exercise the exact static-token-authenticated Streamable HTTP request boundary. *(session history)*

## Solution

Commit `fbac796` renamed the local value from `auth_header` to `auth_token`, removed Plug's caller-side prefix, and passed raw static and OAuth token values to RMCP:

```rust
let auth_token = if config.auth.as_deref() == Some("oauth") {
    crate::oauth::current_or_stored_access_token(name).await
} else {
    config
        .auth_token
        .as_ref()
        .map(|token| token.as_str().to_string())
};

if let Some(token) = auth_token {
    transport_config = transport_config.auth_header(token);
}
```

The production OAuth branch retains its existing missing-token error path; the condensed example only shows the shared representation contract.

The same commit added `streamable_http_static_bearer_token_has_one_prefix`. It starts a local Streamable HTTP server, configures Plug with a fixed placeholder, rejects any authorization value other than `Bearer static-token`, and verifies that upstream initialization succeeds. The test observes the actual wire header rather than an intermediate string.

## Why This Works

Each representation now has one owner. Plug retrieves raw secret material from static configuration or the OAuth store. RMCP converts that raw token into an HTTP bearer authorization value. With only RMCP adding the scheme, the server receives exactly one prefix.

Using the same representation for static and OAuth credentials also removes an accidental asymmetry. Both branches produce raw token material, and the transport builder performs protocol formatting.

The black-box regression test protects the behavior at the boundary that matters. A unit test of `SecretString::as_str()` would not catch another prefix introduced by a dependency. Post-fix live verification confirmed that Exa became healthy and its tool catalog was available. *(session history)*

## Prevention

- Name values by representation: use `auth_token` for raw secret material and reserve `authorization_header` for a complete field value.
- Assign scheme formatting to exactly one layer. Recheck whether helpers such as `bearer_auth` and RMCP's `auth_header` accept a token or a full header.
- Keep a transport-level regression that asserts the header received by a real HTTP endpoint.
- Exercise authenticated and unauthenticated Streamable HTTP upstreams during dependency-upgrade verification.
- Keep static and OAuth credential sources on the same raw-token contract.
- Never print or persist real credentials while debugging; use fixed placeholders and inspect structural facts such as prefix count and response status.

## Related Issues

- [Original Streamable HTTP upstream transport design](mcp-multiplexer-http-transport-phase2.md) — contains the historical prefixed-token example that this fix supersedes.
- [Earlier SecretString auth-header regression](review-fixes-critical-http-auth-ipc-parity-20260307.md) — distinguishes safe secret extraction from transport formatting.
- [Historical RMCP API integration patterns](rmcp-sdk-integration-patterns-plug-20260303.md) — reinforces checking the dependency's public contract rather than inferring semantics from a method name.
- [RMCP HTTP transport boundary and TLS portability](review-fixes-tls-backend-portability-20260307.md) — related Streamable HTTP construction guidance.
