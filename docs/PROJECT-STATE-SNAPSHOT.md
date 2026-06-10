# Project State Snapshot

Baseline: `main` after PR #61 (first-class degraded-vs-absent upstream availability) and its post-merge truth pass

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
- reconnecting IPC proxy sessions
- session-store seam / stateless prep
- downstream protocol-version validation
- upstream MCP-Protocol-Version send-side (provided by rmcp 1.7.0's StreamableHttpClientTransport after initialization; repo-local confidence test confirms)
- roots forwarding with union cache across stdio, HTTP, and daemon IPC
- elicitation reverse-request forwarding across stdio, HTTP, and daemon IPC
- sampling reverse-request forwarding across stdio, HTTP, and daemon IPC
- legacy SSE upstream transport with HTTP→SSE auto-fallback, SSRF hardening, and auth support
- OAuth 2.1 + PKCE upstream auth with credential storage, background token refresh, CLI auth commands, doctor checks, and correct HTTP auth header construction (PR #36, PR #47)
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
- downstream OAuth authorize-redirect allowlist (loopback-default) closing the open-redirector on `build_authorize_redirect`, with percent-encoded code/state and rejection logging (PR #60)
- secretless-OAuth exposure guard: config validation rejects `http.auth_mode = "oauth"` without `oauth_client_secret` when the server is reachable off-loopback (non-loopback bind *or* non-loopback `public_base_url`, e.g. a cloudflared tunnel) (PR #60)
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
- artifact cache pruning at startup, periodic background maintenance, and oldest-first size eviction
- centralized config env traversal reused by doctor env checks, with broader coverage across config fields
- stricter runtime-truth handling across `status`, `tools`, `servers`, `clients`, and `doctor` when the daemon is reachable but IPC/runtime inspection fails

Partial on `main`:

- daemon continuity recovery is proven narrowly for stdio-over-IPC reconnect, not as full cross-transport persistence
- some low-priority internal simplification remains possible in reload/session/SSE helper structure, but no roadmap-critical correctness work remains open

## What Exists Off-Main

Candidate future state only:

- `fix/subscription-rebind-confidence` — large checkpoint branch containing extractable future work (OAuth, SSE client, research docs), not mergeable whole-cloth
Off-main work must not be described as current implementation.

## Release Status

The current roadmap is complete on `main`.
No required roadmap items remain for the current production-ready bar.
Any further work is optional future scope rather than a blocker.

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

## Audit Artifacts

- [BASELINE-2026-03-08](./audit/BASELINE-2026-03-08.md)
- [CLAIM-REGISTRY-2026-03-08](./audit/CLAIM-REGISTRY-2026-03-08.md)
- [MAIN-TRUTH-MATRIX-2026-03-08](./audit/MAIN-TRUTH-MATRIX-2026-03-08.md)
- [OFF-MAIN-STATE-2026-03-08](./audit/OFF-MAIN-STATE-2026-03-08.md)
- [DOC-RECONCILIATION-2026-03-08](./audit/DOC-RECONCILIATION-2026-03-08.md)

## Rule

If a statement conflicts with `main`, `main` wins.
