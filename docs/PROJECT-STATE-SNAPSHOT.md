# Project State Snapshot

Baseline: `main` @ `ab743da` (post tasks support, metadata enrichment, and review hardening)

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
- daemon-backed local sharing
- reconnecting IPC proxy sessions
- session-store seam / stateless prep
- downstream protocol-version validation
- upstream MCP-Protocol-Version send-side (provided by rmcp 1.1.0's StreamableHttpClientTransport after initialization; repo-local confidence test confirms)
- roots forwarding with union cache across stdio, HTTP, and daemon IPC
- elicitation reverse-request forwarding across stdio, HTTP, and daemon IPC
- sampling reverse-request forwarding across stdio, HTTP, and daemon IPC
- legacy SSE upstream transport with HTTP→SSE auto-fallback, SSRF hardening, and auth support
- OAuth 2.1 + PKCE upstream auth with credential storage, background token refresh, CLI auth commands, doctor checks, and correct HTTP auth header construction (PR #36, PR #47)
- mock OAuth provider integration coverage for metadata discovery, auth-code exchange persistence with state cleanup, token refresh persistence, and reconnect using refreshed credentials (PR #51)
- daemon IPC notification parity: progress, cancelled, and list_changed push forwarding (PR #38); resource subscribe remains unsupported over IPC
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

On 2026-03-22, `main` absorbed the core MCP Tasks tranche and related follow-through work that:

- added task lifecycle support across stdio, HTTP, and daemon IPC
- prefers upstream task pass-through when supported, with proven wrapper-mode fallback
- enriched tool semantics and branding metadata for downstream clients
- closed the blocking review findings around monotonic task state, reconnect-stable IPC task ownership, and fail-closed pass-through dispatch

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
