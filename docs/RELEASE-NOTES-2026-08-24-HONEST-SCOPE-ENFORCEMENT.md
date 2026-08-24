# Release notes 2026-08-24: honest downstream OAuth scope enforcement

## What changed

Downstream OAuth scopes are now enforced at `/mcp` on every protocol era. Before this release, per-request scope checks applied only in the gated modern era. The default legacy era granted every authenticated OAuth principal `local_trust`, which skipped the required-scope check entirely, so the scopes printed on the consent page and stored on the token had no runtime effect on legacy requests.

Two things ship together to make that enforcement honest rather than breaking.

1. **Honest default grants.** When `http.oauth_scopes` is not set and `auth_mode = "oauth"`, plug now defaults to the six scope families a normal MCP client exercises: `tools:read`, `resources:read`, `prompts:read`, `completion:use`, `tasks:use`, `subscriptions:listen`. Previously an absent list meant an empty configured set, which could not issue any token at all, so every OAuth deployment carried an explicit list. An explicit list is still honored and still must include `tools:read`.
2. **One-time widening of stored grants.** Access-token and refresh-token records issued before this release carry scopes that were never enforced. At the first startup on this release, any stored record without the new `scope_model = 2` marker has its scopes replaced with the currently configured scope set (the six-family default, or your explicit `http.oauth_scopes` list), is marked, and the store is persisted with the same fail-closed durability as every other write. One info-level log line is emitted per upgraded record.

Scope denial keeps its existing shape in both eras. A request outside the token's grant returns JSON-RPC error `-32005` (permission denied) inside an HTTP 200 response. The RFC 6750 `insufficient_scope` 403 challenge remains intentionally unwired.

## Why widening instead of re-consent

Pre-enforcement tokens effectively had unlimited method access, because `local_trust` bypassed the scope check. Widening their stored scopes to the honest set therefore strictly reduces real privilege. No token can do anything after this release that it could not already do before it.

The alternatives are worse. Widening at refresh time would violate RFC 6749 §6, which forbids a refresh grant from expanding scope. Forcing every client back through consent would break live connections for no security gain, since the resulting grant would be the same set the migration writes. Refresh rotation after migration keeps standard semantics. Rotated tokens inherit the refresh record's scopes, and a client that requests a narrower scope at refresh still gets the narrowing-only check.

## What operators must do

**If you set an explicit `http.oauth_scopes` list, widen it.** Enforcement is now real. A client holding a token scoped to your explicit list loses every method family the list does not name. A `["tools:read"]` list means remote clients can call tools but get `-32005` on `resources/*`, `prompts/*`, `completion/complete`, task methods, and `subscriptions/listen`. Either extend the list to the six families above or remove the key entirely to inherit the default grant. Note that the migration widens stored grants to your configured list, so updating the config before the first restart on this release gives existing tokens the widened grant in the same step.

**Rob's live config is affected.** `~/Library/Application Support/plug/config.toml` currently sets `oauth_scopes = ["tools:read"]` at the `[http]` level. That was written when scopes were cosmetic on the legacy era. Under enforcement it locks remote clients (the Claude Desktop connector path) out of every non-tools method family. Update it to the six families or delete the line to inherit the default, then restart `plug serve` the supported way so the migration runs against the widened configuration.

**No client action is needed.** Existing tokens keep working through the migration. Consent pages now list the full granted scope set, including descriptions for `completion:use`, `tasks:use`, and `subscriptions:listen`.
