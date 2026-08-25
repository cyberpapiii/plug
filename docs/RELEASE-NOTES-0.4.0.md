# Plug 0.4.0

Plug 0.4.0 is the largest reliability, security, and protocol update since the first public release. It keeps current MCP clients working while adding a carefully gated path to the MCP 2026 protocol generation.

## What users will notice

- **More reliable long-running agents.** Plug now recovers cleanly from daemon reconnects, silent IPC connections, route refreshes, upstream restarts, and interrupted SSE delivery. Active subscriptions and session capabilities are restored instead of leaving old tasks permanently stale.
- **Safer remote access.** Remote MCP clients receive isolated OAuth registrations and grants, approve access through an owner passkey, and can be revoked independently. Tokens are bound to the correct server, refresh-token reuse revokes the affected token family, and each method family is checked against the scopes actually granted.
- **Better task behavior.** Long-running tool tasks survive the right disconnects, cancel predictably, expire cleanly, and cannot leak into another client's session.
- **Faster, steadier catalogs.** Resources, prompts, and templates refresh concurrently. A temporary upstream listing failure keeps the last known good catalog rather than making tools disappear.
- **Clearer diagnostics.** Status and doctor output distinguish connection health from catalog health, report live sessions across transports, expose upstream latency and restart history, and return machine-readable failures instead of optimistic guesses.
- **Fewer macOS interruptions.** Local reinstall now signs and verifies the new binary before replacing the running copy, avoiding the unsigned-binary window that caused repeated Keychain prompts.

## For agents and MCP clients

Plug remains a dual-era gateway. Existing Claude, Cursor, Codex, and other clients continue using the proven MCP 2025 lifecycle by default. MCP 2026 support is present behind explicit downstream and upstream gates, so operators can certify peers individually without breaking their current setup.

The modern path adds sessionless discovery, protocol-era-aware routing, principal-owned tasks, bounded extension and trace metadata, and secure multi-round tool continuations. Plug advertises only capabilities it can complete end to end; unsupported mixed-era interactions fail before side effects rather than stranding an agent midway.

Local stdio and daemon-IPC clients remain trusted by the local operator. Remote HTTP clients receive least-privilege, scope-filtered method access and notifications. A full default remote grant covers tools, resources, prompts, completion, tasks, subscriptions, logging control and delivery, and modern continuations; explicitly narrowed grants remain narrow.

## Security and operational hardening

- Standards-based multi-client OAuth with PKCE S256, exact redirects, resource-bound tokens, consent, durable owner approval, and client-specific revocation.
- Fail-closed OAuth persistence with owner-only permissions, atomic writes, bounded records, and issuer-bound upstream credentials.
- Stronger SSRF checks for IPv4-bearing IPv6 forms and safer registration rate-limit identity behind trusted local proxies.
- Scope-filtered capabilities and broadcast notifications, including separate permissions for changing log levels and receiving upstream log messages.
- Bounded task teardown, abort-first cleanup, duplicate request protection, continuation replay prevention, and safer shutdown coordination.
- Updated dependencies, RustSec enforcement, Rust 1.88 minimum-version checks, and exact RMCP 3.1.0 pinning.

## Compatibility and migration

Existing local clients require no configuration change. Existing remote OAuth clients are migrated from the earlier unlimited legacy policy to the operator-configured scope set at startup, so the enforcement upgrade does not silently remove access they already had.

Operators who explicitly pin `http.oauth_scopes` keep that exact grant. Add `logging:read` only for a scoped remote client that opens the SSE stream and needs upstream log notifications. The MCP 2026 gates remain off by default and should be enabled only for clients and upstreams that have been tested against Plug's modern path.

## Distribution

The macOS release artifacts are signed with a Developer ID certificate, submitted to Apple's notarization service, and verified again after packaging. Linux archives, checksums, a shell installer, and an updated Homebrew formula are published from the same tagged build.

For the complete technical inventory, see the [changelog](../CHANGELOG.md) and [project state snapshot](PROJECT-STATE-SNAPSHOT.md).
