# Cross-Client Downstream OAuth Reliability Design

## Goal

Make Plug's remote MCP endpoint connect through standards-compliant clients with one client-side **Connect** action, one required Plug **Allow** action, and automatic return to the client. Users must not choose a transport, copy a token, edit a URL, restart Plug, or interpret raw OAuth JSON.

Plug cannot guarantee compatibility with clients that violate MCP or OAuth standards. Compatibility work must therefore target protocol behavior, not product-name exceptions.

## Compatibility Contract

Plug advertises OAuth discovery and accepts both RFC 7591 Dynamic Client Registration and OAuth Client ID Metadata Documents. Metadata capability lists are client-wide supersets: Plug requires support for the authorization-code flow it selects but ignores unrelated advertised extensions. Dynamic registration and token requests remain strict about capabilities Plug actually supports.

Remote MCP uses Streamable HTTP. Legacy SSE remains a compatibility path where already supported; users never choose between them during authorization. OAuth protects the same canonical MCP resource regardless of client.

Supported authorization behavior is client-neutral: exact registered redirects, PKCE S256, resource binding, requested-scope validation, rotating refresh tokens, persisted grants, revocation, restart recovery, and isolated client credentials.

## Consent Reliability

Consent remains a deliberate local security boundary. Minimum first-time journey is **Connect**, then **Allow**. Previously trusted clients may reconnect without another consent only when OAuth state permits it; Plug must not silently approve a new client.

Consent decisions become idempotent for the five-minute authorization lifetime. First decision atomically creates one authorization result. Repeated submission of the same `consent_id` returns the same redirect and never creates a second authorization code. A conflicting later decision cannot change the first decision. Completed decisions remain memory-only, bounded, expire quickly, and disappear on restart.

Authorization codes remain single-use at the token endpoint. Consent replay does not weaken code replay protection.

## Errors

OAuth JSON errors keep stable RFC error codes and add safe `error_description` text. Descriptions name the failed class and next action without exposing tokens, authorization codes, client metadata contents, filesystem paths, or internal network details.

Browser-facing authorization failures render a readable Plug page with the stable error code and retry guidance. Machine endpoints such as registration and token exchange continue returning JSON.

## Verification

Recorded, hand-checked fixtures cover client-neutral capability shapes:

- strict Dynamic Client Registration;
- Client ID Metadata Documents with only Plug capabilities;
- Client ID Metadata Documents with additional grant and response capabilities;
- missing required authorization-code capability;
- unsupported actual token requests;
- repeated and conflicting consent submissions;
- full registration, authorization, token, authenticated MCP request, refresh, restart, and revocation behavior.

Named client fixtures may preserve real interoperability regressions, but production branches must never select behavior by client name or vendor domain.

## Delivery Boundary

First tranche: metadata compatibility fix, retry-safe consent, actionable errors, and automated client-neutral acceptance coverage. A Plug-owned setup UI or deep link is separate work because external MCP clients own their **Connect** button and callback handling.
