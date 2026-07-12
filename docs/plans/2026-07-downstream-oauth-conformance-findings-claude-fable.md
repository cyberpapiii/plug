# Downstream OAuth conformance findings (todo 057 spike)

**Spike plan**: `plans/018-downstream-oauth-conformance-spike-claude-fable.md`
**Executed**: 2026-07-12, branch `docs/downstream-oauth-conformance-spike`, base commit `23e0f51`
**Protocol version targeted**: MCP `2025-11-25` (see `plug-core/src/http/server.rs` `PROTOCOL_VERSION`
and the `supportedProtocolVersions` field in the server card). If plug's targeted protocol
version moves past `2025-11-25`, this matrix must be re-verified against the newer MCP
authorization spec revision.

**Specs used**:
- MCP Authorization spec, protocol revision 2025-11-25 (`https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization`)
- RFC 8414 — OAuth 2.0 Authorization Server Metadata
- RFC 9728 — OAuth 2.0 Protected Resource Metadata
- RFC 8707 — Resource Indicators for OAuth 2.0 (referenced by the MCP spec for audience binding)

**Scope**: downstream OAuth only — the surface at `plug serve` that lets remote HTTP MCP
clients (e.g. Claude Desktop's remote connector) authorize against plug. Upstream OAuth
(`plug-core/src/oauth.rs`, plug acting as a client toward upstream MCP servers) is out of
scope per the spike plan and todo 057, and was not touched or read for this doc.

**Drift check**: branch HEAD (`docs/downstream-oauth-conformance-spike`) is identical to
base commit `23e0f51` (`git diff 23e0f51 --stat` is empty going in). `plug-core/src/downstream_oauth/mod.rs:510`
already contains the `crate::fs_perm::ensure_dir_0700(dir)` hardening from plan 001 — no
further drift beyond what was pre-verified by the reviewer. `todos/057-*.md` is unchanged
from the base commit.

**A pending sibling change (not on this branch)**: plan 020 (on a separate branch, not yet
merged as of this spike) reportedly adds an eager expiry sweep for the downstream-oauth
token store and changes `client_credentials` token reuse behavior. Everything in this doc
describes the code **as it exists on this branch** (lazy, read-time expiry eviction — see
`validate_access_token` below). Anywhere plan 020 materially changes a finding, a note is
added inline; the finding itself is not adjusted for code this branch does not contain.

---

## 1. What plug implements today (opening summary)

### Endpoints served (`plug-core/src/http/server.rs:352-392`, `build_router`)

Unauthenticated discovery routes (mounted on a `discovery` sub-router exempt from
origin validation, `server.rs:356-373`):

| Route | Handler | file:line |
|---|---|---|
| `GET /.well-known/mcp.json` | `get_server_card` | `server.rs:357`, handler `server.rs:969-1025` |
| `GET /.well-known/mcp-server-card` | `get_server_card` | `server.rs:358` |
| `GET /.well-known/oauth-authorization-server` | `get_oauth_authorization_server_metadata` | `server.rs:359-362`, handler `server.rs:1027-1054` |
| `GET /.well-known/oauth-protected-resource` | `get_oauth_protected_resource_metadata` | `server.rs:363-366`, handler `server.rs:1056-1071` |
| `GET /.well-known/oauth-protected-resource/mcp` | `get_oauth_protected_resource_metadata` | `server.rs:367-370` |
| `GET /oauth/authorize` | `oauth_authorize_not_implemented` (despite the name, this is the live authorize endpoint) | `server.rs:371`, handler `server.rs:1073-1115` |
| `POST /oauth/token` | `oauth_token_not_implemented` (same naming note) | `server.rs:372`, handler `server.rs:1117-1210` |

Protected MCP route (`server.rs:378-389`), layered innermost-first: `validate_origin` →
`validate_bearer_auth` → 4MB body limit:

| Route | Handler |
|---|---|
| `POST/GET/DELETE /mcp` | `post_mcp` / `get_mcp` / `delete_mcp` |

Operator-only route, mounted separately in `plug/src/runtime.rs:192-205` (`build_runtime_router`,
merged with `build_router`'s output), guarded by a distinct operator bearer token
(`x-plug-operator-token` header, constant at `runtime.rs:15`), unrelated to the downstream
OAuth token store:

| Route | Handler | Auth |
|---|---|---|
| `GET /_plug/live-sessions` | `operator_live_sessions` (`runtime.rs:154-164`) | `x-plug-operator-token` header, checked via `plug_core::auth::verify_auth_token` (`runtime.rs:162`) |

There is no separate health-check route (`/health`, `/healthz`, etc.) anywhere in
`plug-core/src/http/server.rs` or `plug/src/runtime.rs` — confirmed by grepping every
`.route(` call in both files.

### Flows supported (`plug-core/src/downstream_oauth/mod.rs`)

- **Authorization Code + PKCE (S256 only)** — `build_authorize_redirect` (`mod.rs:160-216`)
  issues a code; `exchange_authorization_code` (`mod.rs:218-...`) redeems it. PKCE method
  other than `S256` is rejected outright (`mod.rs:173-175`).
- **Refresh token grant** — `exchange_refresh_token` (present; issues a new access token,
  confirmed live in probe transcript below).
- **Client credentials grant** — `exchange_client_credentials` (`mod.rs:324-363`), gated on
  `oauth_client_secret` being configured (`mod.rs:331-333`).
- Only `client_secret_basic` and `client_secret_post` are advertised as
  `token_endpoint_auth_methods_supported` when a client secret is configured, else `"none"`
  (`server.rs:1035-1038`).

### Storage / token model

- Tokens are opaque `uuid::Uuid::new_v4()` strings (e.g. `mod.rs:187`, `mod.rs:344`), not JWTs.
- State (`pending_codes`, `access_tokens`, refresh records) is an in-process `Mutex`-guarded
  struct, persisted to a JSON file under `config_dir()/downstream_oauth/` via `persist_state`
  (`mod.rs:501-...`), which as of this branch creates the directory with `crate::fs_perm::ensure_dir_0700`
  (`mod.rs:510`) — 0700 permissions, confirmed present (drift-check item, not a new finding).
- **Token validation is a binary existence+expiry check.** `validate_access_token`
  (`mod.rs:365-381`) looks the token up, checks `record.expires_at >= epoch_secs()`, and
  explicitly discards the client_id/refresh_token/scopes fields it just read
  (`let _ = &record.scopes;` at `mod.rs:371`) — scopes are stored but **never consulted** to
  authorize a request. This is load-bearing for the scope-enforcement finding in §2 and §5.
- Expired-token cleanup in `validate_access_token` is lazy (only evicted the next time that
  exact token is looked up, `mod.rs:374-378`) on this branch. Plan 020 (pending merge,
  not on this branch) reportedly adds an eager sweep — noted here because it changes *when*
  stale tokens leave the store, not whether scopes are enforced.

---

## 2. Conformance matrix

Verdict legend: **conforms** / **gap** / **partial** / **N/A** (with reason).

### RFC 9728 — OAuth 2.0 Protected Resource Metadata

| # | Requirement | Spec ref | Plug behavior | file:line | Verdict |
|---|---|---|---|---|---|
| 1 | PRM document MUST be served at `/.well-known/oauth-protected-resource` (optionally with path insertion for a resource with a path) | §3.1 | Served at the bare well-known URI and at the path-inserted variant `/.well-known/oauth-protected-resource/mcp` (the MCP resource lives at `/mcp`) | `server.rs:363-370` | **conforms** |
| 2 | PRM `resource` field is REQUIRED and MUST match the resource identifier the client used | §2 | `"resource": format!("{base}/mcp")` | `server.rs:1065` | **conforms** (live-confirmed: `probe-discovery.txt` — `"resource":"http://127.0.0.1:8791/mcp"`) |
| 3 | PRM `authorization_servers` SHOULD be present when RS != AS discovery is needed | §2 | `"authorization_servers": [base]` always present | `server.rs:1066` | **conforms** |
| 4 | PRM `bearer_methods_supported` is OPTIONAL | §2 | `["header"]` | `server.rs:1068` | **conforms** |
| 5 | PRM `scopes_supported` is RECOMMENDED | §2 | Present, derived via `resource_scopes()` (filters out `offline_access`, which is a refresh-grant marker, not a resource scope) | `server.rs:1067`, `mod.rs:448-461` | **conforms** |
| 6 | PRM `resource_documentation` is OPTIONAL | §2 | Not emitted | — | **N/A** (optional field, no functional gap) |
| 7 | WWW-Authenticate on a 401 SHOULD carry a `resource_metadata` parameter pointing at the PRM URL | §5.1 | `HttpError::UnauthorizedWithMetadata` sets `Bearer resource_metadata="...", scope="..."` | `error.rs:67-90`, wired at `server.rs:441-469` | **conforms** (live-confirmed: `probe-preauth.txt`) |

### RFC 8414 — OAuth 2.0 Authorization Server Metadata

| # | Requirement | Spec ref | Plug behavior | file:line | Verdict |
|---|---|---|---|---|---|
| 8 | AS metadata MUST be served at `/.well-known/oauth-authorization-server` (with path insertion if the issuer has a path component) | §3 | Served at the bare well-known URI; issuer has no path component (`issuer` = `public_base_url` with no suffix), so no path-inserted variant is required | `server.rs:359-362`, `server.rs:1044` | **conforms** |
| 9 | `issuer` is REQUIRED and MUST be an `https` URL with no query/fragment (loopback `http` is the MCP spec's explicit carve-out for local development) | §2 | `issuer` = `public_base_url` trimmed of trailing `/`; plug does not itself enforce `https` — this is delegated entirely to operator config (`http.public_base_url`) and `validate_config`'s existing "oauth requires https off-loopback" checks (see config notes below) | `server.rs:1034`, `1044` | **conforms in the intended deployment** (config-time enforcement, not metadata-endpoint enforcement — see finding in §5 for the one config field this does *not* cover) |
| 10 | `response_types_supported` REQUIRED | §2 | `["code"]` | `server.rs:1047` | **conforms** |
| 11 | `authorization_endpoint` conditionally required (required unless AS supports no grant types using it) | §2 | `format!("{base}/oauth/authorize")` | `server.rs:1045` | **conforms** |
| 12 | `token_endpoint` conditionally required | §2 | `format!("{base}/oauth/token")` | `server.rs:1046` | **conforms** |
| 13 | `scopes_supported` RECOMMENDED | §2 | `manager.config.oauth_scopes` (raw configured scopes, including `offline_access`) | `server.rs:1051` | **conforms** |
| 14 | `grant_types_supported` OPTIONAL, defaults to `["authorization_code", "implicit"]` if omitted | §2 | Explicitly listed: `authorization_code`, `refresh_token`, plus `client_credentials` when a client secret is configured. `implicit` is correctly never advertised (OAuth 2.1 drops implicit) | `server.rs:1039-1042` | **conforms** |
| 15 | `token_endpoint_auth_methods_supported` OPTIONAL, defaults to `["client_secret_basic"]` | §2 | `["client_secret_basic","client_secret_post"]` when a secret is configured, `["none"]` for public clients | `server.rs:1035-1038` | **conforms** (live-confirmed: `probe-discovery.txt`) |
| 16 | `code_challenge_methods_supported` OPTIONAL (OAuth 2.1 / PKCE) | RFC 8414 §2, OAuth 2.1 | `["S256"]` only — `plain` is never advertised or accepted | `server.rs:1049`, enforced at `mod.rs:173-175` | **conforms** |
| 17 | AS metadata document MUST be internally consistent — the `issuer` value returned MUST exactly match the issuer the client expected to reach (issuer identifier exact-match, §3.3) | §3.3 | Single-issuer, single-process design: `issuer` is always `public_base_url` as configured; there is no proxying or multi-tenant issuer aliasing that could produce a mismatch | `server.rs:1034` | **conforms** (architecturally not exposed to this failure mode) |

### MCP Authorization spec (2025-11-25) — discovery, PKCE, audience binding, error handling

| # | Requirement | Spec ref | Plug behavior | file:line | Verdict |
|---|---|---|---|---|---|
| 18 | RS MUST implement PRM per RFC 9728 | §2.3 | See rows 1-7 | — | **conforms** |
| 19 | RS MUST use the `WWW-Authenticate` header to indicate the location of the resource server metadata, OR clients fall back to the well-known URI | §2.3 | Both are true: `WWW-Authenticate: Bearer resource_metadata="..."` on 401 AND the well-known URI is served | `error.rs:67-90`, `server.rs:363-370` | **conforms** |
| 20 | Clients MUST use PKCE with S256 | §2.4 | Server-side enforced: any `code_challenge_method != "S256"` is rejected (server can't force client behavior, but it refuses to complete a flow that doesn't use S256) | `mod.rs:173-175` | **conforms** (as far as a server can enforce a client-side requirement) |
| 21 | Redirect URIs MUST be `localhost`/loopback URIs OR MUST use HTTPS (never plain HTTP on a non-loopback host) | §2.4, referencing OAuth 2.1 §4.1.1 | `redirect_uri_allowed()` accepts (a) any exact string match against the operator's `oauth_redirect_uri_allowlist`, no scheme check, or (b) URLs whose host is `127.0.0.1`/`localhost`/`::1` (any scheme). Path (a) has **no HTTPS requirement** — an operator-configured `http://` non-loopback redirect URI is accepted and a real authorization code is redirected to it | `mod.rs:124-137` (esp. `125-126`) | **gap** — live-confirmed, see §3 probe 6. Severity notes in §5. |
| 22 | AS SHOULD bind issued tokens to the resource(s) they were requested for using RFC 8707 `resource` parameter (audience restriction), and clients SHOULD/MUST include `resource` on both `/authorize` and `/token` requests | §2.5, RFC 8707 | Neither the authorize nor the token endpoint reads or validates a `resource` parameter at all. `OAuthAuthorizeParams` has no `resource` field; `oauth_token_not_implemented`'s form-param extraction never looks up `"resource"` | `server.rs:417-425` (struct definition, no `resource` field), `server.rs:1117-1189` (only `grant_type`/`client_id`/`client_secret`/`code`/`redirect_uri`/`code_verifier`/`refresh_token`/`scope` are read) | **gap** — live-confirmed, see §3 probe 2 (a `resource` param was supplied and silently ignored — flow succeeded identically with or without it) |
| 23 | RS MUST validate that an access token was issued specifically for it (i.e., reject tokens minted for a different audience/resource) | §2.5 (security requirement) | Because plug's downstream AS and RS are the same process sharing one opaque token table (`access_tokens: HashMap<token, IssuedAccessToken>`), a token literally cannot exist unless this exact process's AS minted it for this exact resource — there is no second resource or second AS in this deployment for a token to be confused with | `mod.rs:365-381`; architecture: single-process AS+RS, `runtime.rs` wiring | **N/A / conforms-by-construction** — the audience-confusion attack this rule exists to prevent (a token minted by AS-A being replayed against RS-B) has no attack surface here because there is exactly one AS and it is not a proxy in front of anything else. Row 22's `resource`-parameter gap is a separate, protocol-conformance-only issue (a strict client that requires echoing `resource` may refuse to proceed even though the resulting token is safe) |
| 24 | RS SHOULD return `403` with `WWW-Authenticate: Bearer error="insufficient_scope"` when a valid token lacks the scope for the requested operation | §2.6 (referencing RFC 6750 §3.1) | `insufficient_scope` does not appear anywhere in the codebase (`grep -rn insufficient_scope plug-core/src plug/src` → no matches). This is architecturally consistent with row 25: since scopes are never checked, there is no code path that could distinguish "valid token, wrong scope" from "valid token" | — (absence confirmed by repo-wide grep) | **gap** — SHOULD-level, see §5 |
| 25 | Scopes requested/granted SHOULD be enforced — a client should not receive access broader than, or different from, what it can legitimately request, and the RS should gate operations on token scope | §2.6, general OAuth scope semantics | (a) `build_authorize_redirect` and `exchange_client_credentials` take the client-supplied `scope` string, split on whitespace, and store it verbatim with **no filtering against `self.config.oauth_scopes`** — a client can request and receive any scope string it likes, including ones never configured on the server. (b) `validate_access_token` never reads the stored scopes to gate `/mcp` access — any validly-issued (even bearer-fabricated-scope) token unlocks the same full `/mcp` route | `mod.rs:188-195` (authorize), `mod.rs:335-342` (client_credentials), `mod.rs:365-381` esp. `371` (never enforced) | **gap** — live-confirmed, see §3 probe 5 (requested `scope=admin:everything`, never configured, minted verbatim) |
| 26 | AS SHOULD reject/limit `client_credentials` grant scope to what's configured for that grant type, and (relevant to plan 020) SHOULD NOT silently reuse a previously-issued client_credentials token across unrelated requests without re-evaluating expiry | §2.2 general grant hygiene | Same unfiltered-scope behavior as row 25. On token reuse: this branch's `exchange_client_credentials` mints a **fresh** UUID token on every call (`mod.rs:344`) — it does not reuse or cache a prior client_credentials token. Plan 020 (pending merge, not on this branch) reportedly changes client_credentials to reuse an existing unexpired token instead of always minting fresh — that is a behavior change to token issuance volume/reuse, not to scope filtering; the scope-filtering gap in row 25 is orthogonal and would persist under either behavior unless plan 020 also addresses it (out of scope for this spike to confirm, since that code isn't on this branch) | `mod.rs:324-363` | **gap** (scope portion, same as row 25); **N/A** for the reuse question on this branch specifically (not implemented here either way — see plan 020 note) |

### Discovery-adjacent items checked, no gap found

| # | Requirement | Verdict |
|---|---|---|
| 27 | Server card / `.well-known/mcp.json` must not require auth to fetch (clients need it to bootstrap discovery) | **conforms** — `get_server_card` has no auth middleware (`server.rs:356-358`, outside the `mcp` sub-router's auth layers), live-confirmed 200 in `probe-discovery.txt` |
| 28 | 401 response body must not leak internal state (session IDs, stack traces, etc.) | **conforms** — body is a fixed JSON-RPC error envelope `{"error":{"code":-32001,"message":"authentication required"}}`, no session/token echoed (`error.rs`, live-confirmed `probe-preauth.txt`); pre-existing test `session_not_found_does_not_leak_session_id` also covers a related path |
| 29 | An operator-only control route (`/_plug/live-sessions`) must not be reachable without its own credential | **conforms** — distinct `x-plug-operator-token` header check, 401 with no token and with a bogus token (live-confirmed `probe-operator-route.txt`) |

---

## 3. Real-client probe results

All probes ran against an isolated scratch instance: `HOME` overridden to a short throwaway
directory (`/tmp/p18h`, required only because the real scratchpad path exceeds macOS's
Unix-domain-socket `SUN_LEN` limit for the daemon's control socket — all config, logs, and
probe transcripts otherwise live under the task scratchpad), with a scratch
`--config` pointing at a generated `config.toml` (`auth_mode = "oauth"`, dummy client
id/secret, `bind_address = 127.0.0.1`, no upstream servers). No operator config, daemon,
launchd service, or keychain entry was touched. All token values below are ephemeral
credentials minted by this throwaway instance for its own dummy `spike-client` client, not
real credentials — grepped this document for `Bearer `, `token=`, and other key-like
strings before committing; none of the strings below authenticate anything except an
already-destroyed scratch server.

**Judgment call on isolation**: `--config` alone does *not* isolate `config_dir()` (used
for the operator/bearer token files and the downstream-oauth JSON state file — see
`plug-core/src/config/mod.rs` `config_dir()`, `directories::ProjectDirs::from("", "", "plug")`,
no env-var override exists in plug's own code). Reading the vendored `directories`/`dirs-sys`
crate source confirmed that on macOS `ProjectDirs`/`home_dir()` resolution honors the `$HOME`
environment variable. Overriding `HOME` for the scratch process's launch achieves full
isolation of every credential/state path plug touches, without any code change — this is
believed to satisfy the plan's STOP condition intent ("scratch instance cannot run isolated")
better than a literal reading of "the config-path flag" would suggest, since the config-path
flag alone leaves those other paths pointed at the operator's real `~/Library/Application
Support/plug/`.

### Probe 1 — Discovery documents (unauthenticated)

```
GET /.well-known/oauth-protected-resource → 200
{"authorization_servers":["http://127.0.0.1:8791"],"bearer_methods_supported":["header"],
 "resource":"http://127.0.0.1:8791/mcp","scopes_supported":["tools:read"]}

GET /.well-known/oauth-protected-resource/mcp → 200 (identical body)

GET /.well-known/oauth-authorization-server → 200
{"authorization_endpoint":"http://127.0.0.1:8791/oauth/authorize",
 "code_challenge_methods_supported":["S256"],
 "grant_types_supported":["authorization_code","refresh_token","client_credentials"],
 "issuer":"http://127.0.0.1:8791","response_types_supported":["code"],
 "scopes_supported":["tools:read","offline_access"],
 "token_endpoint":"http://127.0.0.1:8791/oauth/token",
 "token_endpoint_auth_methods_supported":["client_secret_basic","client_secret_post"]}

GET /.well-known/mcp.json (unauthenticated) → 200, server card with
 remotes[0].headers = [{"name":"Authorization","isRequired":true,"isSecret":true,...}]
 (advertises the auth requirement, does not itself require auth to fetch)
```

### Probe 2 — Pre-auth `/mcp` surface

```
POST /mcp, no Authorization header → 401
  www-authenticate: Bearer resource_metadata="http://127.0.0.1:8791/.well-known/oauth-protected-resource", scope="tools:read"
  body: {"error":{"code":-32001,"message":"authentication required"},"id":null,"jsonrpc":"2.0"}

POST /mcp, garbage bearer token → 401 (identical body/headers to above)
GET  /mcp (SSE), no auth → 401 (identical body/headers to above)
```

### Probe 3 — Full authorization_code + PKCE flow, including a `resource` parameter

```
GET /oauth/authorize?response_type=code&client_id=spike-client
    &redirect_uri=http://127.0.0.1:9999/callback&state=spike-state-123
    &code_challenge=<S256 challenge>&code_challenge_method=S256
    &resource=http://127.0.0.1:8791/mcp   <-- RFC 8707 resource param, supplied deliberately
  → 302 Found
  location: http://127.0.0.1:9999/callback?code=<redacted-uuid>&state=spike-state-123
  (state echoed correctly; resource param had no observable effect on the response)

POST /oauth/token  grant_type=authorization_code, code=<above>, redirect_uri=<matching>,
    code_verifier=<matching>, resource=http://127.0.0.1:8791/mcp, client secret correct
  → 200 {"access_token":"<redacted>","expires_in":3600,
         "refresh_token":"<redacted>","scope":"tools:read","token_type":"Bearer"}
  (no audience/aud claim in the opaque token; token is a bare UUID; identical shape whether
   or not `resource` was supplied)

POST /oauth/token  same grant, wrong client secret → 400 "token exchange failed: invalid client"

POST /mcp  Authorization: Bearer <minted access_token> → 200, full initialize response
  (mcp-session-id assigned, protocolVersion 2025-11-25 echoed)

POST /oauth/token  grant_type=refresh_token, refresh_token=<above> → 200, fresh access_token
  + refresh_token pair issued
```

### Probe 4 — Redirect URI allowlist enforcement (normal case)

```
GET /oauth/authorize?...&redirect_uri=http://some-unlisted-host.example.com/callback&...
  → 400 "authorization request rejected: invalid authorization request"
  (server log: "rejected /oauth/authorize: redirect_uri is not loopback and not on
   http.oauth_redirect_uri_allowlist")
```
Confirms the allowlist rejection path works correctly for a host that is neither loopback
nor allowlisted.

### Probe 5 — Scope enforcement (client_credentials grant, arbitrary/unconfigured scope)

```
POST /oauth/token  grant_type=client_credentials, client secret correct,
    scope=admin:everything   <-- never configured anywhere in oauth_scopes
  → 200 {"access_token":"<redacted>","expires_in":3600,
         "scope":"admin:everything","token_type":"Bearer"}
```
The server minted and returned a token scoped exactly to the client-supplied string,
including a scope value that does not exist in `http.oauth_scopes`. This access token is
fully valid for `/mcp` (per §2 row 25 — scope is never checked at authorization time).

### Probe 6 — Redirect URI scheme (operator-configured, insecure, live-confirmed)

Scratch config changed to `oauth_redirect_uri_allowlist = ["http://insecure-non-loopback.example.com/callback"]`
(a plain-HTTP, non-loopback allowlist entry — the kind of value an operator *could* type into
their real config; note this required a full process restart to take effect, since the
config file watcher's hot-reload only refreshed `[servers]` state — `added=0 removed=0
changed=0` was logged for the HTTP-config-affecting edit, confirming HTTP/OAuth config is
not part of live reload, consistent with "fully live runtime reconfiguration" being listed
as out of scope in this repo's `CLAUDE.md`):

```
GET /oauth/authorize?...&redirect_uri=http://insecure-non-loopback.example.com/callback&...
  → 302 Found
  location: http://insecure-non-loopback.example.com/callback?code=<redacted-uuid>&state=x
```
A real, single-use authorization code was redirected over plain HTTP to a non-loopback host.
On the open internet this code would be visible to any network intermediary between the
client and `insecure-non-loopback.example.com`. This live-confirms the code-level finding
in matrix row 21.

### Probe 7 — Operator route isolation

```
GET /_plug/live-sessions, no token       → 401 (empty body)
GET /_plug/live-sessions, bogus token    → 401 (empty body)
```
Confirms the operator-only route is not reachable via the downstream OAuth token (which was
never sent) or without any credential.

---

## 4. Privacy findings — pre-auth information exposure inventory

| Surface | Reachable without auth? | Contents | Verdict |
|---|---|---|---|
| `/.well-known/mcp.json`, `/.well-known/mcp-server-card` | Yes (by design — needed for client bootstrap) | Static server name/version/description/repo URL, `remotes[].url = "/mcp"`, a note that Authorization is required. No server inventory, no tool names, no upstream server names. `get_server_card` builds this from constants only (`server.rs:989-1001`) | **fine** |
| `/.well-known/oauth-authorization-server` | Yes (required for discovery) | Issuer/base URL (already known to anyone with the server's address), endpoint paths, supported grant/auth-method/challenge-method lists, configured scope *names* (not values/secrets) | **fine** |
| `/.well-known/oauth-protected-resource` (+ `/mcp` variant) | Yes (required for discovery) | Resource URL, AS URL, scope names | **fine** |
| `401` body on `/mcp` | Yes (that's the point) | Fixed JSON-RPC error envelope, no session ID, no stack trace, no hint about which credential field was wrong (generic "authentication required" whether the header was missing or the token was garbage — does not distinguish, which is itself good practice, avoids a user-enumeration-style oracle) | **fine** |
| `WWW-Authenticate` header on 401 | Yes | `resource_metadata` URL (already public) and `scope` (space-joined *names* of resource-facing scopes, filtered to exclude `offline_access`) | **fine** — no secret material, matches RFC 9728 §5.1 intent |
| `400` body on a rejected `/oauth/authorize` or `/oauth/token` request | Yes | Generic strings: `"authorization request rejected: invalid authorization request"` / `"token exchange failed: invalid client"` — no echoing of the submitted client_secret, code, or code_verifier | **fine** |
| `/_plug/live-sessions` (operator route) | No — 401 without `x-plug-operator-token` (live-confirmed) | N/A (not reachable pre-auth) | **fine** |
| Any health-check endpoint | N/A — none exists | — | **N/A** (no such route in `plug-core/src/http/server.rs` or `plug/src/runtime.rs`; grepped every `.route(` call in both files) |

No leak-severity findings. Everything reachable pre-auth is either intentionally public
(discovery, required by spec, for client bootstrap) or a generic, non-informative error.

---

## 5. Triaged follow-up list

Each item below should become its own implementation plan; this spike does not fix anything.

| # | Finding | Matrix row(s) | Severity | Effort | Next batch? |
|---|---|---|---|---|---|
| F1 | `oauth_redirect_uri_allowlist` entries are accepted verbatim with no scheme check — an operator can (even accidentally, e.g. typo'ing `http://` instead of `https://`) allowlist a plain-HTTP non-loopback redirect URI, and plug will hand a real authorization code to it. Fix: reject/warn at config-validation time (`plug-core/src/config/mod.rs` `validate_config`) and/or at `redirect_uri_allowed()` (`mod.rs:124-137`) for any allowlist entry whose scheme is not `https` and whose host is not loopback | 21 | **spec-gap-no-known-impact** in plug's actual deployment (the operator controls this list and is expected to only put their own callback URLs on it — Claude Desktop's remote connector currently only needs a loopback callback), but it is a genuine MUST-level violation of the redirect-URI rule and a config-time footgun. Escalate to **breaks-strict-clients** if/when plug ever needs to support a non-loopback third-party redirect target | S | Yes — cheap, config-validation-level fix, no protocol redesign needed |
| F2 | Neither `/oauth/authorize` nor `/oauth/token` reads or validates an RFC 8707 `resource` parameter; it is silently accepted and ignored if a client sends one. A strict MCP client that requires `resource` to be echoed/bound (per the 2025-11-25 spec's audience-binding guidance) may refuse to proceed, or may (incorrectly, from its own perspective) believe its token is scoped to a resource that plug never actually validated | 22, 23 | **spec-gap-no-known-impact** today (single-process AS+RS, opaque tokens, no audience-confusion attack surface per row 23's analysis) but likely to become **breaks-strict-clients** as MCP client implementations mature and start enforcing `resource` echoing as a hard requirement | M | Recommend for next batch — implement `resource` capture on `/authorize`, `resource` echo/validation on `/token` (matching the value used at authorize time), even though the resulting audience check is currently a no-op given the architecture — this is about client compatibility, not about closing an exploitable hole |
| F3 | Requested OAuth scopes (on both `authorization_code` and `client_credentials` grants) are accepted verbatim from the client with no validation against `http.oauth_scopes` — a client can mint a token scoped to a string the operator never configured | 25, 26 | **spec-gap-no-known-impact** for confidentiality/authorization purposes today, because scopes are cosmetic everywhere downstream (see F4) — an attacker who can already complete the OAuth flow (i.e., already holds valid client credentials or an allowlisted redirect target) gains nothing extra by fabricating a scope string, since no code path branches on it. It IS a spec-conformance gap and a footgun if scope enforcement is added later without also fixing scope *issuance* filtering | S | Bundle with F4 — fix both in the same batch so scope issuance and scope enforcement land together and stay consistent |
| F4 | `validate_access_token` never inspects `record.scopes` — any validly-issued token unlocks the entire `/mcp` route regardless of what scope it was minted with; consequently `insufficient_scope` (RFC 6750 §3.1 / MCP spec §2.6) is never emitted anywhere in the codebase | 24, 25 | **spec-gap-no-known-impact** — plug currently has exactly one protected resource (`/mcp`) and no finer-grained operations to scope-gate, so there is no *authorization* consequence yet; this is a SHOULD-level protocol-completeness gap, not an access-control hole | M | Defer unless/until plug introduces scope-differentiated operations (e.g. read-only vs. full tool access) — implementing enforcement now would be speculative; F3 (issuance filtering) is worth doing sooner regardless since it's cheap and prevents scope values from becoming meaningless noise in tokens/logs |
| F5 (hygiene, not a spec gap) | HTTP/OAuth-affecting config fields (e.g. `oauth_redirect_uri_allowlist`, `auth_mode`) are not covered by the config file watcher's hot-reload — confirmed live: editing them and waiting for the "config reloaded via file watcher" log line does not change the running server's authorize-time allowlist; only `[servers]` changes take effect without a restart. This matches the documented "fully live runtime reconfiguration" out-of-scope note in this repo's `CLAUDE.md`, so it is **not a new gap**, just worth flagging so a future operator doesn't assume editing the redirect allowlist live takes effect immediately | — (not a spec row; operational note) | **hygiene** | — | No — already accepted as out of scope for this repo's current bar |

**No gap found** for: PRM/AS-metadata document completeness and correctness (rows 1-17),
PKCE S256 enforcement (row 20), `WWW-Authenticate` + well-known fallback discoverability
(row 19), issuer consistency (row 17), audience-confusion resistance by construction (row
23), and the full pre-auth privacy inventory (§4). These are true "no gap" results and are
recorded as such per the plan's honesty requirement — todo 057's standards-conformance
concern is largely already addressed for the metadata/discovery surface; the open items are
narrower (redirect-URI scheme validation, RFC 8707 resource binding, and OAuth scope
issuance/enforcement).
