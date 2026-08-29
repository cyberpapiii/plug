# Project State Snapshot

Baseline: `main` through `9695a09` (`fix: bind the daemon IPC socket before upstream startup (#146)`) on 2026-08-29, plus the change that carries this revision.

This is the canonical current-state doc for the project.

## What Is True On `main`

Implemented on `main`:

- downstream stdio via `plug connect`
- downstream Streamable HTTP via `plug serve`
- downstream HTTPS
- downstream bearer auth for non-loopback HTTP
- logging forwarding
- tools/resource/prompt list-changed forwarding for stdio, HTTP, and daemon IPC
- progress and cancelled routing for stdio, HTTP, and daemon IPC
- resources/prompts/templates forwarding
- resource subscribe/unsubscribe lifecycle
- completion forwarding across stdio, HTTP, and daemon IPC
- structured output pass-through, with strongest proof for `outputSchema`
- capability synthesis with per-transport masking
- tool behavior/metadata enrichment for `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, and `execution.taskSupport`
- canonical server/tool display metadata including server `title`, `icons`, and normalized tool titles
- meta-tool mode
- lazy tool discovery v2 with client-targeted lazy policy, OpenCode bridge search, bounded session working sets, and legacy `meta_tool_mode` compatibility
- daemon-backed local sharing
- reconnecting IPC proxy sessions with capability, subscription, and log-level replay plus a read-silence watchdog
- session-store seam / stateless prep
- downstream protocol-version validation
- upstream MCP-Protocol-Version send-side with requested/selected protocol telemetry
- exact RMCP 3.1.0 with explicit legacy/modern protocol-era policy; legacy MCP `2025-11-25` remains the default while gated MCP `2026-07-28` paths are available for proven peers
- roots forwarding with union cache across stdio, HTTP, and daemon IPC
- elicitation reverse-request forwarding across stdio, HTTP, and daemon IPC
- sampling reverse-request forwarding across stdio, HTTP, and daemon IPC
- legacy SSE upstream transport with HTTP→SSE auto-fallback, SSRF hardening, and auth support
- OAuth 2.1 + PKCE upstream auth with credential storage, background token refresh, CLI auth commands, doctor checks, and correct HTTP auth header construction (PR #36, PR #47)
- static-token Streamable HTTP auth passes raw token material to RMCP 3.1 so the transport emits exactly one Bearer prefix, protected by a wire-level regression
- mock OAuth provider integration coverage for metadata discovery, auth-code exchange persistence with state cleanup, token refresh persistence, and reconnect using refreshed credentials (PR #51)
- daemon IPC notification parity: progress, cancelled, list_changed, resource subscribe/unsubscribe, and targeted resource update forwarding
- localhost OAuth callback listener for `plug auth login` with `--no-browser` manual fallback (PR #39)
- `plug auth complete` for non-interactive OAuth code exchange (PR #40)
- IPC auth commands: `AuthStatus` query, `InjectToken` credential injection with server reconnect, `AuthStateChanged` push notification (PR #41)
- zero-downtime token refresh: actual OAuth refresh_token exchange before reconnect, with injected-token skip path, shared auth-failure classification for refresh/reconnect decisions, reconnect retry without re-refreshing after transient failure, `AuthStateChanged` observability for non-IPC clients via logging-channel fan-out, and a distinct refresh-exchange observability signal (PR #42, PR #43, PR #44, PR #45, PR #50)
- downstream OAuth remote server support
- remote Claude HTTP connector stability fixes
- pagination cursor forwarding and larger page size for remote clients
- initialize response protocol-version simplification/fixups for remote compatibility
- persisted token hydration before upstream connect
- downstream OAuth discovery/privacy hardening, more accurate metadata, and richer challenge behavior
- standards-based multi-client downstream OAuth: RFC 7591 Dynamic Client Registration for Cursor and other public clients, SSRF-hardened OAuth Client ID Metadata Documents, explicit consent, PKCE S256, exact HTTPS/loopback redirects, RFC 8707 resource-bound tokens, issuance-time scope validation against configured `http.oauth_scopes`, rotating refresh tokens, and cross-client grant isolation
  - honest scope grants, enforced in both eras: an absent `http.oauth_scopes` defaults to the nine-family grant (`tools:read`, `resources:read`, `prompts:read`, `completion:use`, `tasks:use`, `subscriptions:listen`, `logging:configure`, `logging:read`, `continuations:complete`; `DEFAULT_DOWNSTREAM_OAUTH_SCOPES` in `plug-core/src/protocol.rs`), and per-request scope checks at `/mcp` apply to OAuth principals on the legacy era as well as the modern one. Grant and refresh records stored before enforcement are widened once at store load to the configured scope set (`scope_model` marker), since they previously had unlimited method access via `local_trust`. Scope denial is JSON-RPC `-32005` inside HTTP 200 in both eras; the RFC 6750 `insufficient_scope` 403 path remains intentionally unreachable. See [`docs/bug-reports/downstream-oauth-scope-enforcement-legacy-era-bypass.md`](bug-reports/downstream-oauth-scope-enforcement-legacy-era-bypass.md)
- issuer-wide owner-only OAuth persistence with registration rate limits, quotas, unused-client expiry, restart recovery, and fail-closed writes; `plug auth clients list/revoke` provides bounded administration without exposing credentials
- cross-client OAuth reliability hardening: Client ID Metadata Documents accept unrelated extension capabilities while enforcing Plug's authorization-code requirements; local consent decisions are retry-safe and replay their first result; JSON, authorization-page, and redirect errors are actionable; automated DCR lifecycle coverage proves registration, consent, code exchange, refresh rotation, and replay rejection (PR #83)
- pre-public auth-surface hardening, closing an adversarial review of the downstream OAuth surface (PR #87, PR #88, PR #89, PR #90):
  - `logging/setLevel` is scope-gated behind a new `MethodFamily::Logging` requiring `logging:configure`
  - registration rate-limit keys bind to the real peer address, consulting forwarding headers only for a loopback peer and reading `x-forwarded-for` right-to-left
  - the Client ID Metadata Document address filter canonicalizes IPv4-mapped IPv6 before the denylist check and rejects the IPv4-compatible, 6to4, and NAT64 embeddings outright (`is_ipv4_bearing_ipv6`); note that `Ipv6Addr::to_canonical` unwraps only the IPv4-mapped form
  - `DownstreamCallContext` construction fails closed: HTTP contexts start untrusted and every adapter arm opts into `local_trust` explicitly
  - list-changed broadcasts are scope-filtered per session. `initialize` evaluates the three families once through `decide_method` and stores the verdict as a `BroadcastAudience`; the shared SSE fan-out consults it. The audience denies by default, because a session is visible to the fan-out before `initialize` can record one
  - RFC 9700 §4.14.2 refresh-token reuse detection: every pair minted from one authorization code shares a `family_id` inherited across rotations, spent tokens leave a tombstone, and replaying one revokes the whole family. Pre-family records are backfilled with individually distinct lineages, so one replay cannot revoke unrelated legacy grants
  - legacy `initialize` projects capabilities through `policy_decision` the way the modern era already did, ending the advertise-then-deny mismatch for narrowed tokens. Behavior-neutral for the full default grant and for the six-family grant predating `logging:configure`
  - log delivery is scope-gated behind `logging:read`, a read permission distinct from the `logging:configure` write scope that governs `logging/setLevel` (PR #93). `BroadcastKind::Unscoped` is gone: every broadcast kind now consults the session's `BroadcastAudience`, so upstream `notifications/message` content — which can name servers a principal holds no scope for — reaches only principals granted `logging:read`. `MethodFamily::LoggingRead` has no method string, since delivery is a broadcast rather than a request; it exists so the audience decision runs through `decide_method` like every other family. The legacy `logging` capability is advertised only to a grant holding both scopes. **Operator migration:** only a scoped remote client that opens the SSE stream and needs upstream log notifications must add `"logging:read"` to pinned `http.oauth_scopes` and re-consent. Local-trust stdio/IPC sessions remain unrestricted, and POST-only remote clients never receive broadcasts
  - the default OAuth grant includes `continuations:complete`, because Plug must reserve bounded, principal-owned continuation state before a first modern upstream tool round can cause side effects. This makes a full default grant sufficient for ordinary modern-to-modern tool calls while keeping explicitly narrowed grants fail-closed
- public HTTPS owner-passkey approval for downstream OAuth, with durable authorization transactions, restart-safe and replay-safe decisions, local proof-authenticated owner enrollment/administration, exact-origin WebAuthn verification, and bounded non-evicting approval ceremonies
- real-process browser coverage for owner enrollment, public consent, PKCE exchange, authenticated MCP use, refresh rotation, restart, denial, expiry, revocation, origin/security headers, error behavior, and redaction
- per-upstream operability metrics in `plug status --output json`: call/error counts, last-latency, degraded-since epoch, and circuit-state label per upstream, with a stable always-present schema (zero-filled for known servers) (PR #60)
- first-class upstream catalog availability (`healthy | degraded | absent`), distinct from connection health, surfaced additively on `ServerStatus` JSON: a transient listing failure (timeout/error) on a routable upstream is `degraded` and serves its last-known-good resources/prompts (preserving active resource subscriptions instead of pruning them); genuine removal still prunes. Closes the PR #58 subscription-rebind residual (PR #61)
- clearer operator auth/runtime UX across `plug status`, `plug doctor`, `plug auth status`, `plug clients`, and `plug servers`
- topology-aware setup/link/repair flows that preserve configured stdio vs HTTP downstream choices
- transport-aware live session inventory across daemon proxy and downstream HTTP sessions
- explicit live inventory scope/availability semantics:
  - `daemon-proxy-only`
  - `http-only`
  - `transport-complete`
  - `unavailable`
- core MCP Tasks support for tool-backed tasks across stdio, HTTP, and daemon IPC:
  - task-wrapped `tools/call`
  - `tasks/list`
  - `tasks/get`
  - `tasks/result`
  - `tasks/cancel`
- oversized result delivery hardening across stdio, HTTP, and daemon IPC:
  - artifact-backed success fallback for very large or attachment-like tool/task results
  - synthetic `plug://artifact/...` manifests and chunk reads via `resources/read`
  - daemon IPC chunking for medium oversized logical responses that should stay inline
  - symmetric IPC frame-size enforcement on read and write paths
- upstream task pass-through when an upstream advertises task-capable `tools/call`, with local wrapper-mode fallback otherwise
- downstream HTTP live-session operator endpoint with dedicated operator token protection
- daemon-owned downstream HTTP/HTTPS when the shared background service is running
- transport-complete live session inventory directly from the daemon in background-service mode
- standalone `plug serve` retained as an explicit foreground runtime path for deliberate non-daemon serving
- pinned machine-readable JSON contracts for operator inventory/auth/runtime surfaces
- standalone HTTP inventory failure-path coverage for missing token, empty token, unauthorized, and malformed response cases
- unified OAuth credential snapshot reads across runtime and operator auth surfaces
- fail-fast HTTP reverse requests for dead SSE targets plus explicit live-delivery feedback after enqueue
- bounded concurrent reload startup with single-flight engine reloads and safe shared upstream registration
- coalesced health-triggered tool refreshes and deduplicated proactive recovery task spawning
- pre-serialized HTTP/SSE notification fanout payloads
- artifact cache pruning at startup, periodic background maintenance, oldest-first size eviction, and blocking-pool writes for oversized payloads
- per-URI atomic resource subscription transitions with recorded-owner drains and same-refresh route healing
- owner-scoped HTTP and IPC task teardown with abort-first local cleanup and bounded upstream cancellation
- downstream OAuth entry sweeping, client-credentials token reuse, scope canonicalization, and fail-closed owner-only persistence
- reload/reconnect installation coordinated through one material-configuration check
- centralized config env traversal reused by doctor env checks, with broader coverage across config fields
- stricter runtime-truth handling across `status`, `tools`, `servers`, `clients`, and `doctor` when the daemon is reachable but IPC/runtime inspection fails
- gated MCP `2026-07-28` downstream discovery/sessionless HTTP and upstream `legacy | auto | modern` negotiation
- modern principal-owned task lifecycle, admitted extension/schema/trace propagation, and secure native modern-to-modern multi-round `tools/call` continuations
- issuer-bound upstream OAuth credentials with fail-closed handling for legacy unbound records
- native macOS 14+ Plug.app with one shared verdict model, a glanceable menu-bar surface, and full operator workspaces for servers, tools, connections, activity, settings, and upstream reauthorization
- human/CLI feature parity for normal operation: server add/import/edit/remove and enablement, per-tool enablement with wildcard-safe behavior, local client linking, remote-grant revocation, activity attribution, checkup, reload, restart, logs, and sign-out
- daemon operator IPC for version negotiation, health/inventory snapshots, bounded activity history, server lifecycle mutations, and downstream grant revocation; the app and CLI remain clients of the same daemon authority
- app-owned launchd lifecycle with first-run adoption of legacy daemon installs, leftover Homebrew Cellar/bin launchd adopt after formula uninstall, shared leftover-path classify pinned by `testdata/legacy_plug_programs.json`, handshake inspect errors mapped to `unknown` (fail-closed, never `Unmanaged`), app-managed staleness kept launchd-local with no handshake `stale` field, single-flight startup, visible bounded failure handling, and a bundled universal Plug daemon so the app is standalone
- Edit Server `GetServerConfig` gated on app-side `server_config_read`; operator IPC overlap documented with `OPERATOR_IPC_MIN` at 3; doctor leftover copy; `persist_config_atomic` comment drop; PlugIPC golden payloads (PR #124)
- direct Developer ID distribution as a signed, notarized, stapled DMG with Sparkle 2 signed updates and a Homebrew cask using the identical artifact
- official prerelease MCP 2026 server conformance at 22 passed, 0 failed, including request-scoped progress streaming, concrete resource-template routing, and `enable_prefix = false` behavior

Partial on `main`:

- daemon continuity recovery is proven narrowly for stdio-over-IPC reconnect, not as full cross-transport persistence
- some low-priority internal simplification remains possible in reload/session helpers, but no roadmap-critical correctness work remains open
- official modern npm conformance remains prerelease evidence (`0.2.0-alpha.10`), despite the full 22/22 server pass; stable real-client certification is still evaluated peer by peer

## What Exists Off-Main

No roadmap-affecting work currently exists off-main.

The native operator app, its daemon API and lifecycle ownership, distribution automation,
and the 22/22 official modern server conformance fixes landed on `main` in
PR #96 (`52b2c7b`); the public-install daemon-adoption repair landed in PR #101
(`9fdae06`). The calm single-sidebar operator redesign landed in PR #104
(`b045000`), and the Swift 6-safe ServiceManagement adoption repair landed in
PR #105 (`6dcfc1b`). The complete menu-bar/operator overhaul landed in PR #121
(`163bc87`), the real cold-start lifecycle repair landed in PR #122
(`a197f65`), the live v0.8.1 rollout record landed in PR #123
(`66b8d7a`), and leftover Homebrew launchd adopt plus fail-closed ownership
`unknown` landed in PR #124 (`a02ab18`). App-managed staleness stays
launchd-local; handshake `stale` is not on the wire.

The MCP `2026-07-28` modernization is done on `main` via PR #68. Both global
modern protocol gates still default to off. This Mac enables only the proven
modern downstream HTTP gate; modern upstream negotiation remains off and every
configured upstream remains pinned to `legacy` until its real client/server
combination is certified. Modern listeners, mixed-era
multi-round bridging, task-plus-multi-round calls, and Apps/UI capability
advertisement remain intentionally suppressed rather than advertised.
Synthesized multi-upstream catalog pages emit conservative cache directives
(`ttlMs: 0`, `cacheScope: private`) for modern peers; legacy responses strip them.

## Release Status

Release `v0.8.1` is the current unified macOS release. Plug.app contains the
matching universal CLI/runtime, owns daemon lifecycle after first-run adoption,
repairs the canonical `~/.local/bin/plug` link, and redirects older stray CLIs
into the signed bundle. Homebrew, GitHub, and website installs therefore
converge on one app, one version, and one runtime.

`v0.8.1` replaces the older diagnostic-heavy window with a minimal native
operator experience built around one shared verdict. Servers, tools,
connections, and activity are visible directly; Settings owns service control,
checkup, files, updates, and quit. The app now exposes the normal CLI operator
surface without creating a second configuration or runtime authority.

The 0.8.1 lifecycle fix also lets a real app-owned cold start finish. Plug.app
kickstarts `com.plug.daemon` once, then waits up to 90 seconds for the bundled
runtime and all configured upstreams instead of repeatedly killing a healthy
startup while it is still initializing. The delayed-readiness behavior is
covered by focused app tests.

A reliability pass on 2026-08-29 (PRs #139 through #142) closed the failure
class behind that lifecycle fix rather than the one instance of it. The daemon
claims its runtime lock before `Engine::start` and binds the IPC socket only
once every upstream is up, so a healthy cold start is indistinguishable from an
absent daemon for tens of seconds. The CLI allowed 8 seconds for that where
Plug.app allows 90, and force-kickstarted with no check for a start already in
flight, so a `plug connect` could kill the cold start the app was waiting on.
Both halves now share the same 90-second budget and consult the runtime lock
first, `plug`, `plug client list` and `plug servers` report that window as
starting rather than stopped, and reading the lock can no longer defeat the
start it is describing. Operator requests and `plug connect` session setup are
bounded, both plists restart the daemon on a non-zero exit, and a fatal startup
error reaches the daemon log instead of a stderr nobody redirects.

The same pass hardened the app's side of that seam. Sign-in and sign-out ran
`plug auth` through a second, worse copy of the process-running code that waited
for the child before draining its pipes, so a talkative CLI deadlocked the app;
they now go through `ProcessRunner`, which leaves no hand-rolled `Process` in
the app at all. A handshake reporting `unknown` ownership blocks instead of
publishing repairable drift, since drift is retried on every trigger and
`unknown` is an absence of evidence rather than a disagreement. Booting a legacy
launchd job out re-reads the job's program first, because `launchctl bootout`
addresses a job by label and a label is not an identity.

A performance pass on the same day (PR #144) removed work from three hot paths
by subtraction. `maybe_delegate_to_app` runs before Clap parsing on every
invocation, and delegation exists only to choose which binary should run; when
the running executable is already a bundle executable there is nothing left to
choose, so it now returns on a path comparison instead of spending a `codesign`
walk and two subprocess spawns re-proving the code already executing. Bundle
integrity stays covered by `plug doctor` and by the app's own reconciliation. A
bundle executable measured 68.8 ms per invocation before and 7.7 ms after, which
matters most for `plug connect`, the path every local MCP client takes. The
app's launchd inspection ran `launchctl print` over every label `launchctl list`
returned, and Apple reserves the `com.apple.` namespace for OS services that no
Plug install path has ever used; excluding it took a real login session from 563
prints and 1.117 s to 59 prints and 0.115 s. `Engine::start_all` batched server
starts with `chunks(startup_concurrency)`, which made every server in a batch
wait for the slowest one before the next batch could begin, and now uses one
`JoinSet` behind a semaphore so a freed permit goes straight to the next server;
`startup_concurrency` defaults to 12 rather than 3, because a start is process
spawn and handshake rather than computation.

PR #146 then removed the ordering that produced the whole reliability pass.
`cmd_daemon` claimed the runtime lock, ran `Engine::start` to completion, and
only then bound the Unix socket, so a healthy cold start was indistinguishable
from an absent daemon for as long as the slowest upstream took. `Engine::start`
now runs alongside the daemon and the IPC socket binds immediately, so the
daemon is discoverable the moment it owns the lock.

No answer goes out early. Capability negotiation happens once per session and is
never revisited, so a client told the daemon has no tools would have no way to
learn otherwise; that answer and every MCP request wait on an `Engine` readiness
signal instead, and the client-visible contract is unchanged. Downstream HTTP
deliberately keeps the old ordering, because a remote client cannot be told the
catalog grew. Total cold start is therefore still bounded by the slowest
upstream: a multiplexer cannot describe a catalog it has not finished reading.

The preceding 0.7.4 work ignores transient unrelated launchd jobs while keeping
Plug's exact `com.plug.daemon` label fail-closed, excludes the app's own
RunningBoard process from daemon ownership, carries the repaired shell link
forward as migration evidence, and no longer counts that canonical link as a
competing install. The drift banner names the exact disagreeing check. The
PlugApp XCTest suite runs in the `Test (Plug.app)` CI gate (signed
reconciliation fixtures excepted), so app-side regressions no longer pass with
green checks.

CI through PRs #117 and #119 starts all independent gates immediately, strips debug
symbols from CI-only dev/test profiles, caches Playwright browsers, and verifies
release artifacts with delegation disabled so it always executes the exact
binary under test. Documentation-only changes finish after a six-second,
fail-safe path classification instead of running the full build matrix. Any
code, configuration, dependency, script, or workflow change still runs every
validation gate.

On 2026-08-28 and 2026-08-29 the developer workflow was rebuilt so that keeping
the repo tidy and getting a change merged need no manual steps (PRs #131 through
#136). The workspace had no `[profile.dev]`, so every dependency carried full
debug info; `debug = "line-tables-only"` cut `target/` from 13 GB to 3.7 GB and a
cold build from 40 to 31 seconds. Measurement also disproved the assumption
behind the work: nothing in `target/` was stale, and fingerprint-based orphan
detection found zero dead artifacts, so the problem was generation rate rather
than garbage. `scripts/clean-build-artifacts.sh --guard` therefore enforces a
budget instead of sweeping age (10 GB for `target/`, 5 GB for this project's
Xcode DerivedData), costs about 40 milliseconds, and runs from `post-commit`,
`post-merge`, `post-checkout`, `pre-push`, and `scripts/dev.sh`.

`scripts/dev.sh` runs the lanes a change actually needs, selecting them with
`scripts/classify-changes.sh` — the same file CI's `classify` job runs, so the
local gate cannot disagree with the merge gate by construction.
`scripts/test-app.sh` is the same arrangement for the app: one definition of the
`xcodebuild` invocation, called by `dev.sh` locally and by the `Test (Plug.app)`
job with `--unsigned`. It exists because that invocation previously lived only in
`ci.yml`, and a hand-run `xcodebuild test` failed five fixture tests with an
unhelpful `NSCocoaErrorDomain Code=259` for want of a `MARKETING_VERSION`.
`scripts/ship.sh` takes a dirty tree to a merged pull request in one command and
stages tracked modifications only, which is what keeps private notes and local
credentials out of a commit. `scripts/setup-dev.sh` wires a fresh clone up and is
idempotent.

`main` is protected by exactly one required check, the `CI complete` aggregate
job, which passes when no lane reported failure or cancellation. Requiring the
individual lane jobs would wedge every pull request, because path selection skips
most of them on most changes. `enforce_admins` is deliberately off so an
emergency direct push is always possible.

Releases `v0.7.0` through `v0.8.0` are superseded. They established
the unified distribution and delegation model but each exposed installed-only
regressions: recursive version probing, cold-daemon preflight rejection,
unrelated launchd-label ownership classification, adoption/convergence defects,
remaining second-owner paths, or the repeated-kickstart cold-start failure fixed
in 0.8.1.

Release `v0.6.1` is the historical predecessor to the unified macOS line. Its
public checksums verify, the universal `Plug.app` and embedded daemon are
Developer ID signed with hardened runtime, and both the app and DMG are
notarized and stapled.

The app's ServiceManagement registration is live under
`com.cyberpapiii.plug`: launchd owns the bundled daemon as a PID-1 child from
`/Applications/Plug.app/Contents/Resources/plug`, with parent bundle version 27.
The installed app, CLI, and runtime all report 0.8.1; all 13 enabled upstreams
are healthy with 486 routed tools; the canonical shell link resolves into the
bundle; and the obsolete CLI LaunchAgent plist is absent. The app reports
all servers as running without a setup or reconciliation banner. Fresh
installed-runtime certification initialized a real stdio MCP client, listed 472
tools for that client's policy, and completed
a routed `Context7__resolve_library_id` call successfully. The
public OAuth resource metadata advertises the operator-pinned six-scope grant.
Production enables modern downstream HTTP only; modern upstream remains
legacy-compatible by design for the currently observed clients and servers.

The 0.6.2 source line also repairs a live app-owned launchd compatibility gap:
bare stdio commands such as `node` and `npx` resolve through the user's login
shell when launchd supplies only its minimal system `PATH`. Runtime startup and
`plug doctor` share this resolution path, so command checks match execution.

Owner-verified downstream OAuth is done on `main` through merge commit
`8adbcd1`. Fresh merged-main verification passed 1,106 workspace tests and 5
real-process browser tests with 1 intentional WebKit virtual-authenticator skip.
The exact source head is installed with the stable `Plug Local Signing`
identity, the shared daemon is running with one enrolled owner passkey, and all
12 configured upstreams are healthy.

Live Claude Desktop certification passed on 2026-08-09: Claude discovered Plug
from only `https://plug.plugtunnel.com/mcp`, used its official Client ID Metadata
Document and hosted callback, displayed the public Plug approval page, completed
owner-passkey approval without a localhost hop, exchanged the grant, and showed
the connector as connected. Plug recorded the Claude client as recently used
and reported active downstream HTTP sessions. ChatGPT, Codex, Cursor, OpenCode,
and a real WebKit platform-passkey ceremony remain separate manual compatibility
gates; no claim is made for those clients from the Claude result.

The cross-client downstream OAuth reliability work is done on `main` through
PR #83. Its automated release gates passed: 1,021 workspace tests, clippy, and
formatting. A signed local build from current `main` is installed and running as
the shared daemon with all 12 configured upstreams healthy. Live HTTPS CIMD
certification is complete for Claude Desktop. Other named clients remain manual
gates; do not infer their compatibility from Claude's successful post-consent
callback and live MCP connection.

On 2026-07-17, downstream OAuth moved from one manually configured client to
a standards-based public-client service. A remote MCP client now starts with
only the `/mcp` URL, registers itself (or uses a Client ID Metadata Document),
shows the user a Plug consent page, and receives a client-isolated,
resource-bound `tools:read` grant. The old singular client ID, shared secret,
and redirect allowlist are removed rather than retained as a legacy path, so
existing remote clients must authorize once after this upgrade. Local stdio
clients are unaffected. See
[`RELEASE-NOTES-2026-07-17-MULTI-CLIENT-OAUTH-codex-5.6-sol.md`](RELEASE-NOTES-2026-07-17-MULTI-CLIENT-OAUTH-codex-5.6-sol.md).

On 2026-07-13, `main` upgraded the Rust MCP SDK from RMCP 1.7.0 to the exact
stable RMCP 2.2.0 release. Plug migrated to RMCP's spec-aligned model types,
accepts cancellation notifications without a request id without touching an
unrelated active call, and keeps its existing stdio, Streamable HTTP, daemon
IPC, OAuth, Tasks, elicitation, sampling, resources, prompts, completion, and
notification behavior. At that historical point the negotiated wire revision
remained `2025-11-25` and `2026-07-28` was still rejected. PR #68 later upgraded
to RMCP 3.1.0 and added gated dual-era support; see the top of this snapshot for
current truth. Historical notes:
[`RELEASE-NOTES-2026-07-13-RMCP-2.2-codex-5.6-sol.md`](RELEASE-NOTES-2026-07-13-RMCP-2.2-codex-5.6-sol.md).

The same release refreshed all direct Rust dependencies to their latest
compatible stable versions. Keyring 4.1.4 preserves the existing macOS and
Linux credential identities; TOML 1.1.2 keeps whole-document client/import
parsing through explicit regressions; Tower HTTP 0.7.0 now enforces Plug's
intended 4 MiB request ceiling; and local macOS reinstalls publish only a
fully signed and verified binary through an atomic replacement. Read-only
daemon auth status also stays on memory and the protected mirror, so a missing
mirror cannot freeze IPC or HTTP behind a Keychain authorization dialog.

On 2026-07-12, `main` absorbed the 24-plan improve program plus four rounds of
counter-review repairs. The user-visible result is stronger reconnect and SSE
replay behavior, atomic resource subscription ownership across route refreshes,
bounded owner-scoped task teardown, safer downstream OAuth persistence, faster
catalog refresh, and non-blocking oversized artifact writes. At that point the source-build
minimum became Rust 1.88 and `rmcp` was constrained to the 1.7 release line; the
RMCP 2.2 upgrade on 2026-07-13 superseded that dependency constraint. The final
workspace gate passed 857 tests plus clippy, formatting, MSRV compilation,
RustSec advisories, and todo-status checks. See
[`RELEASE-NOTES-2026-07-12-codex-5.6-sol.md`](RELEASE-NOTES-2026-07-12-codex-5.6-sol.md)
for the user-facing summary and `plans/EXECUTION-REPORT-claude-fable.md` for
the technical record.

On 2026-07-03, `main` absorbed the improve-audit hardening batch (eight reviewed branches, merged directly): (1) daemon IPC frame reads are now cancellation-safe — a dedicated reader task feeds a bounded channel, ending the frame-desync ("frame too large") failure when notification delivery raced a mid-flight frame; the reverse-request read path shares the same ordered channel; (2) the daemon grace-period task re-checks on a bounded interval while held alive by HTTP sessions, so HTTP drain now triggers auto-shutdown instead of stranding the daemon; (3) `ServerManager::shutdown_all` swaps the map under `server_map_write_lock` and always retires + clears even when the map `Arc` is shared (shutdown can no longer silently no-op); (4) `try_send_to_session` only clears the SSE sender that actually failed (`same_channel` gate), so a racing reconnect's fresh sender survives and receives the event; (5) downstream auth guards (`auth_mode = "none"` rejection and `auto`-mode token minting) now key on the `externally_exposed` signal (non-loopback bind **or** non-loopback `public_base_url`) — **breaking** for tunneled no-auth configs; the TLS guard stays bind-only by design; (6) `config.toml` is written/tightened to 0600 and `SecretString`'s plaintext-`Serialize` asymmetry is documented + pinned by test; (7) supervision decision seams (healthy-blip non-reset, stable-recovery reset, backoff accumulation, disabled-mode) gained direct tests; (8) quick wins — macOS/cross/size CI jobs run on pushes to `main`, `install.sh` points at `cyberpapiii/plug`, todo 068 closed (rmcp already 1.7.0), anyhow → 1.0.103 clearing RUSTSEC-2026-0190. Workspace suite after merge: 511 + 43 + 176, clippy/fmt/advisories clean.

On 2026-06-10, `main` absorbed PR #67 — active upstream supervision (item 2b / R10), the final program item. When an upstream stays degraded past a threshold (sustained health-check failures **or** an open circuit breaker — the connected-but-failing case the existing Failed-recovery path doesn't reach, e.g. the iMessage continuation leak), the daemon supervises a bounded restart (process restart for stdio, reconnect-with-reset for HTTP/SSE) instead of waiting for a manual one. A `SupervisionConfig` (enabled by default, conservative thresholds) drives a pure `should_restart` decision with an exponential inter-episode backoff (capped) so a perpetually-failing upstream can't storm; restarts surface additively in `plug status --output json` (`restart_count`, `last_restart_epoch_secs`). An adversarial + reliability review found and this PR fixed four storm vectors (backoff defeated by reset-on-healthy-blip → now gated on stable recovery; zero-min-interval rejected in validation; backoff reset on reload; unified restart accounting). With this, the **2026-06-10 operability/hardening program is complete**: degraded-vs-absent model (#61), transport dispatcher + whole-surface parity gate (#63/#64), ToolRouter god-object decomposition (#65), IPC identity split (#66), and supervision (#67).

On 2026-06-10, `main` absorbed PR #66 — the `DownstreamTransport::Ipc` identity split (KTD3, the last dispatcher-deferred item). Daemon IPC clients no longer masquerade as `Stdio`: they now have a first-class `DownstreamTransport::Ipc`, an `ipc:{id}` lazy-session-key namespace, a `DownstreamCallContext::ipc_for_client` constructor, and `NotificationTarget::Ipc`. Every `daemon.rs` IPC site (reverse-request context, the notification-forwarding match, the `tools/list` lazy key, subscribe/unsubscribe targets, disconnect/replace cleanup, roots, bridge registration) was switched to `Ipc`; the in-process `StdioBridge` keeps `Stdio`. A stdio and an IPC client sharing an id no longer collide in the lazy working-set map. Behavior-affecting (internal namespace + target variant only; no wire change) — an 8-property correctness review and an adversarial dropped-notification/leak/wrong-delivery review both returned zero findings; guarded by the parity matrix + the IPC notification-delivery e2e tests (now exercising the `Ipc` target). Only active upstream supervision (item 2b / R10) remains.

On 2026-06-10, `main` absorbed PR #65 — the `ToolRouter` god-object decomposition (program item 1's "decompose along seams" corollary). `plug-core/src/proxy/mod.rs` was split **6,586 → 2,464 lines (63%)** into six cohesive child modules: `proxy/{tests,handler,tasks,completion,subscriptions,catalog}.rs`, each an `impl super::ToolRouter` block. Move-only, zero behavior change — proven by the unchanged full workspace suite (490+169+43) and the cross-transport parity matrix. The genuinely-coupled core stays in `mod.rs` by design: the struct + shared types, the routing engine (`call_tool*`/`call_tool_inner`/`handle_*`), the notification/active-call methods (they share the four `*_lookup` maps), and the cross-cutting `refresh_tools`. Remaining program work: the `DownstreamTransport::Ipc` identity split (KTD3) and active upstream supervision (item 2b / R10) — the next two PRs.

On 2026-06-10, `main` absorbed PR #64 (program item 1, requirement R8) — finishing the cross-transport parity deliverable across the **entire MCP method surface** plus IPC encode consolidation. Scope correction verified during planning: unlike `tools/call`, every other method family is already a single shared `ToolRouter` call behind thin per-transport shells (no progress/task/reverse-request complexity), so the value here is parity coverage + encode de-duplication, not a `DownstreamContext` trait migration:

- the parity matrix now drives `tools/list`, `resources/{list,templates,read}` (+ unknown-uri error), `prompts/{list,get}` (+ unknown-prompt error), `completion/complete`, and the `resources/{subscribe,unsubscribe}` lifecycle through the real stdio/HTTP/IPC transports and asserts identical decoded results + error codes. The harness was generalized to method-generic drivers (`parity_{stdio,http,ipc}_call` + `assert_parity`) normalizing to a canonicalized `MethodOutcome`; the existing `tools/call` rows pass unchanged (characterization guard)
- the mock upstream (`plug-test-harness/src/bin/mock-server.rs`) gained flag-gated prompts / completion / resource-template handlers so the rows compare real routed content, not empty-list agreement
- the duplicated per-arm IPC `serde_json::to_value → SERIALIZE_ERROR` ladder was consolidated into two shared helpers (`ipc_ok` / `ipc_from_mcp_result`) — behavior-preserving, proven by the matrix staying decoded-identical plus a direct helper unit test. An 8-persona review returned zero production-code findings; six test-quality fixes were applied
- still deferred: the `DownstreamTransport::Ipc` identity split (KTD3). Investigation showed `NotificationTarget::Stdio` is the shared bridge/delivery key for both the in-process stdio path and daemon IPC across ~64 sites, with subscription-rebind reconstructing `Stdio` targets on route refresh — a full split rewires notification delivery + reconnect-stable ownership, so it ships as its own PR (now de-risked by the parity matrix). The `ToolRouter` god-object decomposition and active upstream supervision (item 2b) remain the next program phases

On 2026-06-10, `main` absorbed the first slice of the transport `RequestDispatcher` via PR #63 (deferred program item 1, requirement R8) — the `tools/call` method family only:

- a new `plug-core/src/dispatch` module owns a transport-agnostic adapter shell (`DownstreamContext` trait + `dispatch_tools_call` returning a `ToolCallOutcome` over `CallToolResult`/`CreateTaskResult`); the routing core (`ToolRouter::call_tool_with_context` / `enqueue_tool_task`) is called unchanged. The program-plan premise of "three duplicated copies of tools/call" was corrected during planning: the route was already shared; only the per-transport adapter shell + error encoding were duplicated
- stdio (`proxy/mod.rs`), HTTP (`http/server.rs`), and daemon IPC (`daemon.rs`) now delegate their `tools/call` handling to the shared dispatcher; the task branch is gated per-transport via `supports_tasks()` (stdio false). No product-surface behavior change — client-aware filtering, meta-tool mode, progress/cancellation, and reverse-request forwarding preserved (8-persona review found zero production-code findings)
- a first end-to-end IPC test harness (none existed) plus a cross-transport parity matrix drive identical `tools/call` scenarios through the real stdio/HTTP/IPC transports and assert identical decoded results and error codes — the recurring parity-drift bug class is now a CI gate
- the empty-name response was converged (IPC's `INVALID_PARAMS` pre-check removed) so all three transports return `METHOD_NOT_FOUND`. Two divergences remain intentional and pinned by tests: none for empty-name (now converged); task-augmented calls reject on stdio (rmcp `ServerHandler` validation) but create a passthrough task on HTTP/IPC — a capability difference, not a defect
- deferred to follow-up: the remaining method families (`tools/list`, `resources/*`, `prompts/*`, completion) migrate to the dispatcher in their own PRs; a `DownstreamTransport::Ipc` identity split (IPC currently reuses the stdio identity, KTD3); and consolidating the duplicated mock `ServerConfig` fixture into `plug-test-harness`

On 2026-06-10, `main` absorbed parallel test execution via PR #62 (deferred program item 4): the workspace suite no longer needs `--test-threads=1` — the daemon/ipc/runtime tests that share the process-global runtime-paths slot serialize behind one shared lock while the rest run in parallel, and the mock server is pre-built once instead of `cargo run` per spawn. CI wall-clock for tests roughly thirds. No product-surface change. Full `RuntimePaths` injection (concurrent daemon tests too) remains deferred.

On 2026-06-10, `main` absorbed the first-class degraded-vs-absent availability model via PR #61 (deferred program item 3):

- the catalog refresh no longer conflates a stalled listing with an empty one: `ServerManager`'s resource/template/prompt listers classify each per-server call as fresh or unavailable (timeout/error) and carry last-known-good forward for an unavailable-but-routable upstream, so its URI set is unchanged across the cycle and the existing subscription prune/unsubscribe loop leaves it alone
- added a first-class `Availability { healthy | degraded | absent }` recomputed each refresh and surfaced additively on `ServerStatus` (schema-stable for `plug status --output json`): a routable upstream that fails to list is `degraded` (serving stale if cached, else nothing — never falsely `healthy`); `absent` is reserved for upstreams not in the routed set
- closes the PR #58 subscription-rebind residual; multi-agent review caught and fixed a real misclassification (failing-with-no-cache reported `healthy`) before merge

Residuals recorded in PR #61: shared listing-helper extraction, pre-existing `health` PascalCase vs `availability` lowercase JSON casing, an availability-scoped degraded-since timestamp (tied to deferred supervision), `refresh_tools` single-flight, and template/prompt degraded-path integration coverage. None is a roadmap blocker.

On 2026-06-10, `main` absorbed the operability + tunneled-OAuth hardening tranche via PR #60:

- closed the downstream OAuth open-redirector: `build_authorize_redirect` now checks the requested `redirect_uri` against a configured allowlist (defaulting to loopback hosts `127.0.0.1` / `localhost` / `::1`) *before* issuing the authorization code, percent-encodes code/state, and logs rejected URIs
- added a secretless-OAuth exposure guard in config validation: a server reachable off-loopback (non-loopback bind, or a non-loopback `public_base_url` such as a cloudflared tunnel) with `http.auth_mode = "oauth"` and no `oauth_client_secret` is now rejected at validation time — the original guard keyed only on bind address and missed the tunnel topology; the merged guard keys on exposure
- added per-upstream metrics to `plug status --output json`: call/error counts, last-latency-ms, degraded-since epoch, and a circuit-state label, surfaced per upstream with a stable schema (always present, zero-filled for known-but-idle servers) so agents can read "server X degraded since T" instead of inferring it

Known residual (tracked follow-up, not yet on `main`): downstream OAuth with a remote (non-loopback) `redirect_uri` now requires adding it to the allowlist (the loopback `plug auth login` path is unaffected; rejections are logged). An end-to-end metrics-recording test and an RAII recording guard remain deferred, as does an operator-guide note on `degraded_since` vs. health divergence. None is a roadmap blocker.

On 2026-06-10, `main` absorbed the code-review stabilization batch via PR #58:

- daemon IPC `tools/call` now forwards `_meta.progressToken`, making the progress-routing parity claim above genuinely true for the default `plug connect` path (it previously dropped the token on the non-task path)
- bounded per-server resource/prompt listing in `refresh_tools` by `call_timeout_secs`, so a connected-but-stalled upstream can no longer freeze the catalog refresh
- guarded `notification_refresh_in_progress` with an RAII drop guard plus a backstop timeout, so a panic or cancellation cannot permanently wedge `list_changed` delivery
- fixed a DashMap deadlock when reading a TTL-expired artifact, and made `read_chunk_text` read a single chunk instead of the whole payload
- `plug server edit --output json` now performs the edit instead of printing the unedited config; `plug doctor` exits with its computed code (1 = fail, 2 = warn) for agent/CI gating
- removed dead `sighup_reload` / `resource_subscription_count`; corrected stale `rmcp` / `serde` version claims across the docs

Known residual (tracked follow-up, not yet on `main`): the ≥16MB artifact write is still synchronous (not yet `spawn_blocking`). Not a roadmap blocker. (The other PR #58 residual — a transient listing timeout pruning/upstream-unsubscribing an active resource subscription without rebinding — was closed by PR #61.)

On 2026-03-22, `main` absorbed the core MCP Tasks tranche and related follow-through work that:

- added task lifecycle support across stdio, HTTP, and daemon IPC
- prefers upstream task pass-through when supported, with proven wrapper-mode fallback
- enriched tool semantics and branding metadata for downstream clients
- closed the blocking review findings around monotonic task state, reconnect-stable IPC task ownership, and fail-closed pass-through dispatch

On 2026-04-24, `main` absorbed lazy tool discovery v2 via PR #56:

- added client-targeted lazy policy with native/bridge/disabled modes
- added OpenCode bridge discovery as `plug__search_tools` followed by direct routed tool calls
- bounded bridge session working sets so repeated searches cannot regrow to the full catalog
- preserved legacy `meta_tool_mode` compatibility separately from bridge mode

On 2026-03-16, the previously working branch/runtime line was reconciled into `main`, verified with
the full test suite, and pushed as the new canonical baseline.

On 2026-03-18, `main` also absorbed the follow-on performance, efficiency, and operator-truth
hardening work that:

- unified credential snapshot reads and removed redundant auth-store probes from operator flows
- made reverse HTTP client requests fail immediately when live SSE delivery cannot be completed
- serialized reload execution and removed batched upstream registration races
- reduced SSE broadcast cost by reusing pre-serialized payloads
- clarified the difference between daemon reachability and successful runtime inspection across operator surfaces

## Documentation Taxonomy

Use docs by role:

- current truth:
  - `docs/PLAN.md`
  - `docs/ROADMAP-AUDIT-2026-03-08.md`
  - `docs/PROJECT-STATE-SNAPSHOT.md`
  - `docs/TRUTH-RULES.md`
- workflow enforcement:
  - `AGENTS.md`
  - `CLAUDE.md`
  - `docs/WORKFLOW-OPERATING-MODEL.md`
- issue tracking:
  - `todos/*.md`
- plans / intended work:
  - `docs/plans/*.md`
- historical / design context:
  - old phase plans and solutions docs

## Current Top Priorities

1. keep current-state docs aligned with `main`
2. continue optional operator/runtime polish around mixed-topology visibility and recovery clarity
3. consider low-priority simplification/perf cleanup in reload/session/SSE helper structure if the maintenance bar expands
4. keep all off-main work clearly marked as candidate future state only
5. preserve the CE adapter layer (`AGENTS.md`, `CLAUDE.md`, workflow guide) so future agents start in the right place
6. live client certs still missing: ChatGPT, Codex, Cursor, OpenCode, and a real WebKit platform-passkey ceremony
7. live formula rehearsal before the next tag: install formula, let the daemon start, brew uninstall, open Plug.app, Turn On. CI doubles do not prove that race
8. split doctor/unified snapshot out of `plug/src/commands/misc.rs`, `clients.rs`, and `plug-core/src/ipc.rs`. Diagnosis does not belong in `misc.rs`
9. #125 is a live secret-exposure decision (`GetServerConfig` returns full credential material to any auth-token holder), not a versioning question. #126 and #127 are post-merge cleanups and do not gate a release. #129 is generating leftover-path classify tables from the shared fixture

## Audit Artifacts

- [BASELINE-2026-03-08](./audit/BASELINE-2026-03-08.md)
- [CLAIM-REGISTRY-2026-03-08](./audit/CLAIM-REGISTRY-2026-03-08.md)
- [MAIN-TRUTH-MATRIX-2026-03-08](./audit/MAIN-TRUTH-MATRIX-2026-03-08.md)
- [OFF-MAIN-STATE-2026-03-08](./audit/OFF-MAIN-STATE-2026-03-08.md)
- [DOC-RECONCILIATION-2026-03-08](./audit/DOC-RECONCILIATION-2026-03-08.md)

## Rule

If a statement conflicts with `main`, `main` wins.
