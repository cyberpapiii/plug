# Release notes 2026-08-24: honest downstream OAuth scope enforcement

## What changed

Downstream OAuth scopes are now enforced at `/mcp` on every protocol era. Before this release, per-request scope checks applied only in the gated modern era. The default legacy era granted every authenticated OAuth principal `local_trust`, which skipped the required-scope check entirely, so the scopes printed on the consent page and stored on the token had no runtime effect on legacy requests.

Two things ship together to make that enforcement honest rather than breaking.

1. **Honest default grants.** When `http.oauth_scopes` is not set and `auth_mode = "oauth"`, plug now defaults to the six scope families a normal MCP client exercises: `tools:read`, `resources:read`, `prompts:read`, `completion:use`, `tasks:use`, `subscriptions:listen`. Previously an absent list meant an empty configured set, which could not issue any token at all, so every OAuth deployment carried an explicit list. An explicit list is still honored and still must include `tools:read`.
2. **One-time widening of stored grants, on legacy-era deployments only.** Access-token and refresh-token records issued before this release carry scopes that were never enforced *if the deployment ran the default legacy era*. At the first startup on this release, any stored record without the new `scope_model = 2` marker is marked and the store is persisted with the same fail-closed durability as every other write. Whether the marking also rewrites scopes depends on `http.modern_downstream_enabled`:

   - **`false` (the default, legacy era).** The record's scopes are replaced with the currently configured scope set (the six-family default, or your explicit `http.oauth_scopes` list). One info-level log line is emitted per upgraded record.
   - **`true` (modern era).** The record keeps the scopes it was granted, and is only marked. The modern `/mcp` path already gated method families on those stored scopes, so they reflect real consent. One info-level log line records that grants were marked without widening.

Scope denial keeps its existing shape in both eras. A request outside the token's grant returns JSON-RPC error `-32005` (permission denied) inside an HTTP 200 response. The RFC 6750 `insufficient_scope` 403 challenge remains intentionally unwired.

## Why widening instead of re-consent, and why only on the legacy era

On the legacy era, pre-enforcement tokens effectively had unlimited method access, because `local_trust` bypassed the scope check. Widening their stored scopes to the honest set therefore strictly reduces real privilege. No such token can do anything after this release that it could not already do before it.

That argument does not hold under `http.modern_downstream_enabled = true`. There the modern `/mcp` path already enforced the token's stored scopes, so a `["tools:read"]` token really was denied `resources/read`. Widening it would grant access the owner never approved at the passkey ceremony, and because refresh records are marked too, every rotation would re-mint the wider set permanently. Modern-era grants are therefore left at the scopes they were consented to.

The alternatives are worse. Widening at refresh time would violate RFC 6749 §6, which forbids a refresh grant from expanding scope. Forcing every client back through consent would break live connections for no security gain, since the resulting grant would be the same set the migration writes. Refresh rotation after migration keeps standard semantics. Rotated tokens inherit the refresh record's scopes, and a client that requests a narrower scope at refresh still gets the narrowing-only check.

## What operators must do

**If you set an explicit `http.oauth_scopes` list, widen it.** Enforcement is now real. A client holding a token scoped to your explicit list loses every method family the list does not name. A `["tools:read"]` list means remote clients can call tools but get `-32005` on `resources/*`, `prompts/*`, `completion/complete`, task methods, and `subscriptions/listen`. Either extend the list to the six families above or remove the key entirely to inherit the default grant. On the legacy era the migration widens stored grants to your configured list, so updating the config before the first restart on this release gives existing tokens the widened grant in the same step. On the modern era it does not; see below.

**Rob's live config is affected.** `~/Library/Application Support/plug/config.toml` currently sets `oauth_scopes = ["tools:read"]` at the `[http]` level. That was written when scopes were cosmetic on the legacy era. Under enforcement it locks remote clients (the Claude Desktop connector path) out of every non-tools method family. Update it to the six families or delete the line to inherit the default, then restart `plug serve` the supported way so the migration runs against the widened configuration.

**If you run `http.modern_downstream_enabled = true`, your stored grants keep their consented scopes.** The migration marks them but does not widen them, because they were already enforced. A client holding a narrow token stays narrow. To move it to the wider default, change `http.oauth_scopes` (or remove the key) and have the client re-authorize; the new grant is issued at the new set. There is no way to widen an existing grant without re-consent, by design.

**No client action is needed on the legacy era.** Existing tokens keep working through the migration. Consent pages now list the full granted scope set, including descriptions for `completion:use`, `tasks:use`, and `subscriptions:listen`.
