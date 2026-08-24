# Downstream OAuth scopes are not enforced at `/mcp` in the legacy protocol era

**Status:** fixed 2026-08-24 on `fix/downstream-oauth-honest-scope-enforcement` (see Resolution)
**Severity:** low today (see Impact); a correctness/expectation gap rather than an exploit
**Found:** 2026-08-08, verifying the plan-018 conformance spike against current `main`
**Supersedes:** finding 3 of `docs/plans/2026-07-downstream-oauth-conformance-findings-claude-fable.md`, which reported scopes as entirely cosmetic. Half of that finding has since been fixed.

## Resolution (2026-08-24)

Shipped as honest issuance plus universal enforcement. None of the three
options below was taken as written; the shipped model removes the reason the
bypass existed instead of gating it.

- An absent `http.oauth_scopes` under `auth_mode = "oauth"` now defaults to the
  six-family grant `DEFAULT_DOWNSTREAM_OAUTH_SCOPES` in
  `plug-core/src/protocol.rs` (`tools:read`, `resources:read`, `prompts:read`,
  `completion:use`, `tasks:use`, `subscriptions:listen`), matching what a
  normal MCP client actually does. An explicit list still must include
  `tools:read`.
- `legacy_http_policy_context` now builds OAuth principals with
  `.with_authorization(principal, claims.scopes)`, so `decide_method` enforces
  method-family scopes at `/mcp` in both eras. The pinned compatibility test
  was inverted and renamed `legacy_oauth_principal_scopes_gate_method_families`.
- Stored grant and refresh records carry a `scope_model` marker (serde default
  1; new records written at 2). At store load, model-1 records have their
  scopes replaced with the currently configured set, are marked model 2, and
  the store is persisted fail-closed. Those tokens had unlimited method access
  under `local_trust`, so widening strictly reduces real privilege; refresh-time
  widening would violate RFC 6749 §6 and forced re-consent would break live
  clients. Refresh rotation after migration keeps standard narrowing-only
  semantics.
- Scope denial stays JSON-RPC `-32005` inside HTTP 200 in both eras. The
  RFC 6750 `insufficient_scope` 403 path in `plug-core/src/http/error.rs`
  remains intentionally unreachable; wiring it is a separate decision.

The analysis below is the pre-fix state, kept as the record of why the bypass
existed.

## Summary

Downstream OAuth scopes are now validated when a token is *issued*, but on the
default protocol era they are not checked when a request is *served*. A token
minted with only `resources:read` still reaches `tools/call`.

The `PROJECT-STATE-SNAPSHOT.md` line previously read "`tools:read` enforcement",
which was true only for the gated modern era. It has been corrected.

## What is fixed

Issuance-side validation is real and tested:

- `plug-core/src/downstream_oauth/mod.rs:847-865` — `validate_scopes` rejects any
  requested scope outside the configured `http.oauth_scopes` (and rejects an empty
  configured set); an absent `scope` parameter defaults to the full configured set.
- Called from `begin_authorization` at `mod.rs:613`; the validated set is what lands
  in the consent record, the authorization code, and the issued token
  (`mod.rs:735-741`, `mod.rs:1158-1176`).
- Surfaced as the RFC 6749 `invalid_scope` error at
  `plug-core/src/http/server.rs:1758` and `:1805`.
- `plug-core/src/config/mod.rs:623-633` requires `tools:read` to be present when
  `auth_mode = "oauth"`.
- Test: `mod.rs:1493-1499` asserts requesting `tools:write` against a `["tools:read"]`
  config returns `InvalidScope`.

## What is not enforced

The request-time machinery exists and is correct, but the default path bypasses it.

- Scope-to-method mapping: `plug-core/src/protocol.rs:139-154`
  (`ToolsList | ToolsCall => "tools:read"`, `Resources* => "resources:read"`, etc.).
- Policy: `protocol.rs:185-210` `decide_method` returns `Deny(PermissionDenied)` when
  the required scope is absent — at lines 205-209.
- **The bypass:** `protocol.rs:202-204` returns `PolicyDecision::Allow` early when
  `input.local_trust` is set, before the scope check runs.
- Modern-era requests build `modern_http_call_context`, which calls
  `.with_authorization(principal, claims.scopes)` (`server.rs:162-164`,
  `:1850-1867`); that sets `local_trust = false` (`plug-core/src/proxy/mod.rs:439-448`),
  so scopes are enforced.
- Legacy-era requests build `legacy_http_policy_context`, which calls
  `.with_local_principal(principal)` (`server.rs:194-199`); that sets
  `local_trust = true` (`proxy/mod.rs:450-454`), so every method family is allowed.
- Era is selected per request at `server.rs:908-913`, and modern is refused unless
  `router.modern_downstream_enabled()` — which defaults to `false`
  (`plug-core/src/config/mod.rs:288`, `:319`; `plug-core/src/proxy/mod.rs:830`).

This is intentional and pinned by a test, not an oversight:
`server.rs:3013-3056` (`legacy_tools_read_oauth_principal_keeps_pre_scope_method_compatibility`)
*asserts* that a `tools:read`-only token is allowed `ResourcesRead`, `PromptsGet`,
`Tasks`, and `Listeners`. The comment at `server.rs:194-199` states the rationale:
the legacy OAuth contract predates method-family scopes, so widening it would break
already-authorized clients. The modern-era counterpart is proven by
`server.rs:3200-3253`
(`resource_only_oauth_token_can_discover_and_list_resources_but_not_tools`).

## Secondary: the `insufficient_scope` challenge is unreachable

`plug-core/src/http/error.rs:39` and `:97-116` implement an RFC 6750 §3.1-correct
403 with `WWW-Authenticate: Bearer error="insufficient_scope", scope="…",
resource_metadata="…"` and a JSON-RPC `-32003` body. It is unit-tested at the
formatting layer (`error.rs:233-248`).

No request to a running `plug serve` can produce it. The only production
construction site is `server.rs:793-798`, reached only when
`validate_access_token_for` returns `AccessTokenValidation::InsufficientScope`
(`mod.rs:797-802`), which requires a non-empty `required_scopes` slice — and
`server.rs:789` passes `&[]`. Every other non-test caller also passes `&[]`.

Where scope denial *does* occur (modern era), the response is not RFC 6750 at all:
`ProtocolOutcome::PermissionDenied` becomes JSON-RPC `-32005` (`protocol.rs:243`)
served with HTTP 200 (`server.rs:2615-2631` maps only METHOD_NOT_FOUND to 404 and a
few codes to 400; everything else falls through to `StatusCode::OK`). The test at
`server.rs:3247` asserts that 200 explicitly.

## Impact

Low today. Every scope plug issues is validated against the operator's configured
set, and the configuration requires `tools:read` for OAuth mode — so the common
deployment issues tokens that legitimately carry `tools:read` anyway. The gap
matters when an operator configures several scopes intending to hand different
clients different privilege levels: on the legacy era that separation is not
enforced at request time.

## Options

1. **Leave as is** and keep the snapshot honest (done). Correct while the modern
   era is off by default and no operator relies on scope-based separation.
2. **Enforce scopes on the legacy era too**, dropping `local_trust` for OAuth
   principals. Breaks `legacy_tools_read_oauth_principal_keeps_pre_scope_method_compatibility`
   and forces already-authorized remote clients to re-consent with wider scopes.
   Needs an operator decision and a release note.
3. **Gate option 2 behind a config flag** (for example `http.enforce_oauth_scopes`),
   defaulting off, so an operator who wants privilege separation can opt in without
   breaking existing grants. Also makes the `insufficient_scope` path reachable —
   which requires deciding whether scope denial should return the RFC 6750 403 or
   stay a JSON-RPC `-32005` inside a 200.

Option 3 is the recommended shape if this is picked up.
