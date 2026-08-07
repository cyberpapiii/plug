# Solutions

Documented solutions and patterns for this repo, organized by category.
Each learning has YAML frontmatter (`module`, `tags`, `problem_type`, `summary`) for search.
See `CONCEPTS.md` for shared domain vocabulary and `AGENTS.md` for how this store fits Compound Engineering.

Plans and audits are not current truth; these files are historical compound knowledge.

## architecture-patterns

- [`router-snapshot-hot-path-secondary-indexes.md`](architecture-patterns/router-snapshot-hot-path-secondary-indexes.md) — Build RouterSnapshot secondary indexes at catalog publish for O(1) tools/call resolution
  - RouterSnapshot already pre-cached client-filtered tool lists for O(1) list; extend the same publish-time pattern with secondary indexes via with_indexes()
- [`resource-subscription-transitions-and-owner-reconciliation.md`](architecture-patterns/resource-subscription-transitions-and-owner-reconciliation.md) — Serialize resource-subscription transitions and reconcile the confirmed owner
  - Resource subscription state spans two systems: Plug records downstream members locally, while an upstream MCP server owns the actual `resources/subscribe` state. A local `HashSet` plus first-subscriber/last-subscriber calls is not enough on...

## build-errors

- [`dead-tui-dependencies-in-workspace-manifest-20260307.md`](build-errors/dead-tui-dependencies-in-workspace-manifest-20260307.md) — Dead TUI Dependencies in Workspace Manifest
  - Dead TUI Dependencies in Workspace Manifest

## code-quality

- [`2026-03-18-config-env-traversal-and-operator-loop-hygiene.md`](code-quality/2026-03-18-config-env-traversal-and-operator-loop-hygiene.md) — Centralized config env traversal and refreshed operator loop state
  - There were two small but persistent hygiene issues
- [`2026-03-18-final-p3-polish-and-runtime-model-cleanup.md`](code-quality/2026-03-18-final-p3-polish-and-runtime-model-cleanup.md) — Final P3 polish and runtime model cleanup
  - After the correctness and operator-truth fixes landed, a few lower-priority cleanup items still remained
- [`phase4-p3-polish-code-review-fixes.md`](code-quality/phase4-p3-polish-code-review-fixes.md) — Phase 4 P3 Polish: TUI dirty flag, RouterConfig DRY, and tracing split _(stale)_
  - Phase 4 P3 Polish: TUI dirty flag, RouterConfig DRY, and tracing split

## design-patterns

- [`backoff-reset-requires-sustained-recovery.md`](design-patterns/backoff-reset-requires-sustained-recovery.md) — Backoff is defeated if you reset it on transient health — gate the reset on sustained recovery
  - The `plug` daemon (an MCP multiplexer) added active upstream supervision (item 2b): if an upstream MCP server stays degraded past a threshold, the supervisor restarts it. To stop a perpetually-failing upstream from storming with restarts, e...

## integration-issues

- [`2026-03-18-auth-status-backing-store-warnings.md`](integration-issues/2026-03-18-auth-status-backing-store-warnings.md) — Auth status now surfaces keyring and token-file backing store drift
  - Credential persistence drift had become visible in logs, but not to operators
- [`2026-03-18-call-correlation-hardening.md`](integration-issues/2026-03-18-call-correlation-hardening.md) — Call correlation hardening for reverse requests, progress, and cancellation
  - The router previously treated several multiplexed behaviors as if one upstream server implied one downstream owner
- [`2026-03-18-control-notification-lag-signals.md`](integration-issues/2026-03-18-control-notification-lag-signals.md) — Control-channel lag now emits downstream-visible warning signals
  - When the control notification channel lagged, `plug` only logged a local warning. Downstream clients could miss `progress`, `cancelled`, or `list_changed` traffic without receiving any protocol-visible hint that delivery had degraded.
- [`2026-03-18-doctor-setup-guidance.md`](integration-issues/2026-03-18-doctor-setup-guidance.md) — Doctor now points missing-config recovery to plug setup
  - `plug doctor` still suggested `plug init` when the config file was missing, even though the supported onboarding flow is `plug setup`.
- [`2026-03-18-explicit-upstream-retirement-and-bounded-shutdown.md`](integration-issues/2026-03-18-explicit-upstream-retirement-and-bounded-shutdown.md) — Upstream replacement now retires old connections explicitly and engine shutdown stays bounded
  - Reconnect and restart cutover still relied too heavily on `Drop` behavior
- [`2026-03-18-http-reverse-request-fail-fast.md`](integration-issues/2026-03-18-http-reverse-request-fail-fast.md) — HTTP reverse requests now fail fast when no live SSE consumer exists
  - Reverse requests to downstream HTTP sessions still allocated timeout-backed pending state even when there was no live SSE consumer to receive them.
- [`2026-03-18-http-sse-keepalive-and-tools-empty-state.md`](integration-issues/2026-03-18-http-sse-keepalive-and-tools-empty-state.md) — HTTP SSE keepalives now preserve session activity and tool inventory empty states are explicit
  - HTTP SSE keepalives now preserve session activity and tool inventory empty states are explicit
- [`2026-03-18-injected-oauth-refreshability.md`](integration-issues/2026-03-18-injected-oauth-refreshability.md) — Injected OAuth credential refreshability follows configured client identity
  - `plug auth inject` previously stored all injected credentials with the synthetic client id `injected`.
- [`2026-03-18-ipc-interleaving-buffering.md`](integration-issues/2026-03-18-ipc-interleaving-buffering.md) — IPC interleaving buffering during daemon registration and roots updates
  - Two IPC paths still assumed a response-only stream even though the daemon can push notifications on registered connections
- [`2026-03-18-legacy-sse-fallback-timeout-hardening.md`](integration-issues/2026-03-18-legacy-sse-fallback-timeout-hardening.md) — Legacy SSE fallback and endpoint timeout hardening
  - The legacy SSE path had two brittle behaviors
- [`2026-03-18-legacy-sse-preinitialize-replay-and-liveness-probe.md`](integration-issues/2026-03-18-legacy-sse-preinitialize-replay-and-liveness-probe.md) — Legacy SSE startup and backlog handling are hardened and health checks use lighter liveness probes
  - Three reliability gaps were still open after the earlier SSE hardening work
- [`2026-03-18-oauth-credential-snapshot-unification.md`](integration-issues/2026-03-18-oauth-credential-snapshot-unification.md) — OAuth credential reads now use one freshest-persisted snapshot path
  - The OAuth store still had two different read behaviors
- [`2026-03-18-reload-health-refresh-coalescing.md`](integration-issues/2026-03-18-reload-health-refresh-coalescing.md) — Reload startup is now batched and health refreshes are coalesced
  - Two related control-plane costs were still higher than they needed to be
- [`2026-03-18-reload-topology-background-tasks.md`](integration-issues/2026-03-18-reload-topology-background-tasks.md) — Reload topology now rebuilds health and refresh task ownership
  - Hot reload previously updated server processes and config snapshots, but it did not rebuild the runtime maintenance topology to match.
- [`2026-03-18-reload-truth-followup-hardening.md`](integration-issues/2026-03-18-reload-truth-followup-hardening.md) — Reload serialization and operator truth follow-up hardening
  - The post-performance review found a second round of issues after the batching and fanout work landed
- [`2026-03-18-runtime-truth-config-env-session-oauth-hardening.md`](integration-issues/2026-03-18-runtime-truth-config-env-session-oauth-hardening.md) — Runtime truth, config env, session, and downstream OAuth hardening
  - The remediation review surfaced four related reliability gaps
- [`2026-03-18-sse-fanout-pre-serialization.md`](integration-issues/2026-03-18-sse-fanout-pre-serialization.md) — HTTP SSE fanout now reuses pre-serialized payloads
  - The HTTP/SSE notification path was doing extra work for every connected session
- [`2026-04-09-daemon-control-token-race-and-http-initialized-compat.md`](integration-issues/2026-04-09-daemon-control-token-race-and-http-initialized-compat.md) — Daemon control-token race and HTTP initialized-notification compatibility
  - Two separate issues combined into one misleading operator experience
- [`claude-remote-http-connector-stability-20260310.md`](integration-issues/claude-remote-http-connector-stability-20260310.md) — Claude remote HTTP connector stability
  - Fixed a cluster of Claude remote-connector failures by debouncing downstream list-changed notifications, adding explicit HTTP origin allowlisting for Claude-hosted connectors, aligning stdio protocol advertisement to 2025-11-25, and documenting that the remote connector must target the full `/mcp` endpoint rather than the tunnel root.
- [`completion-pass-through-forwarding-20260307.md`](integration-issues/completion-pass-through-forwarding-20260307.md) — Completion Pass-Through Forwarding for MCP Proxy
  - Completion Pass-Through Forwarding for MCP Proxy
- [`daemon-restart-context-keychain-and-spawn-storm.md`](integration-issues/daemon-restart-context-keychain-and-spawn-storm.md) — Restart the plug daemon in the user's login session, never from a detached/sandboxed context
  - Activating a freshly-installed `plug` binary by restarting the daemon from inside an agent/automation shell (or any detached, non-GUI-session context) took the whole multiplexer down. The daemon would start, connect most upstreams, then han...
- [`downstream-https-serving-20260307.md`](integration-issues/downstream-https-serving-20260307.md) — Downstream HTTPS serving
  - Added optional HTTPS termination to `plug serve` with cert/key configuration, ring-backed rustls provider installation, and a real TLS MCP regression covering initialize, tools/list, and SSE attach.
- [`krisp-oauth-authrequired-recovery-20260311.md`](integration-issues/krisp-oauth-authrequired-recovery-20260311.md) — Krisp OAuth AuthRequired Recovery Gap _(stale)_
  - Krisp was configured as an upstream HTTP MCP server with OAuth
- [`local-codesigning-identity-stops-keychain-reprompts.md`](integration-issues/local-codesigning-identity-stops-keychain-reprompts.md) — A stable self-signed code-signing identity stops repeated macOS Keychain prompts
  - On macOS, plug re-prompts for Keychain access ("plug wants to use your confidential information stored in 'plug' in your keychain") constantly — every time an agent or app that talks to plug starts. Clicking **Always Allow** does not stop i...
- [`mcp-logging-notification-forwarding-20260307.md`](integration-issues/mcp-logging-notification-forwarding-20260307.md) — MCP logging notification forwarding (Phase A1)
  - Every MCP SDK emits log notifications via `notifications/message` by default. plug silently dropped all of them because `UpstreamClientHandler` didn't implement `on_logging_message()`. Additionally
- [`mcp-multiplexer-http-transport-phase2.md`](integration-issues/mcp-multiplexer-http-transport-phase2.md) — Phase 2 HTTP Transport Implementation for MCP Multiplexer (plug)
  - Phase 1 of plug delivered a working stdio-only MCP multiplexer. Phase 2 required adding HTTP transport support for both inbound (web clients connecting to plug) and outbound (plug connecting to remote upstream MCP servers). This involved si...
- [`multi-client-downstream-oauth-codex-5.6-sol.md`](integration-issues/multi-client-downstream-oauth-codex-5.6-sol.md) — Use dynamic public-client OAuth for remote MCP clients
  - Plug's original downstream OAuth server recognized one client ID, one optional secret, and one operator-maintained redirect allowlist. Discovery succeeded, but a client such as Cursor stopped because the authorization server could not regis...
- [`phase2a-notification-infrastructure-tools-list-changed-20260307.md`](integration-issues/phase2a-notification-infrastructure-tools-list-changed-20260307.md) — Phase 2A notification infrastructure: upstream tools/list_changed fan-out
  - `plug` had a working tool-call path but no end-to-end server-notification path.
- [`phase2b-progress-cancellation-routing-20260307.md`](integration-issues/phase2b-progress-cancellation-routing-20260307.md) — Phase 2B progress and cancellation routing across stdio and HTTP
  - Phase 2A solved global notification plumbing, but it did not yet solve request-scoped control flow.
- [`phase2c-resources-prompts-pagination-20260307.md`](integration-issues/phase2c-resources-prompts-pagination-20260307.md) — Phase 2C resources, prompts, pagination, and capability synthesis
  - After Phase 2A and 2B, `plug` had a strong tool and control-flow path, but the next MCP surfaces were still not real
- [`phase3-resilience-token-efficiency.md`](integration-issues/phase3-resilience-token-efficiency.md) — Phase 3 Resilience & Token Efficiency — Circuit Breakers, Health Checks, and rmcp API Patterns
  - Phase 3 Resilience & Token Efficiency — Circuit Breakers, Health Checks, and rmcp API Patterns
- [`phase3a-meta-tool-mode-tool-drift-20260307.md`](integration-issues/phase3a-meta-tool-mode-tool-drift-20260307.md) — Phase 3A meta-tool mode with routed invocation and tool-definition drift detection
  - The shared router already knew how to merge and route tools, but it still assumed every client should see the whole tool catalog up front. That caused two product gaps
- [`phase3b-e2e-integration-test-foundation-20260307.md`](integration-issues/phase3b-e2e-integration-test-foundation-20260307.md) — Phase 3B end-to-end integration test foundation for stdio, HTTP, and shared upstream clients
  - The codebase already had good local confidence, but not enough boundary confidence.
- [`phase3c-daemon-continuity-recovery-20260307.md`](integration-issues/phase3c-daemon-continuity-recovery-20260307.md) — Phase 3C daemon continuity recovery requires active IPC connections to honor daemon shutdown
  - The reconnect story at the daemon boundary looked complete on paper
- [`phase3d-session-store-stateless-prep-20260307.md`](integration-issues/phase3d-session-store-stateless-prep-20260307.md) — Phase 3D session-store abstraction keeps HTTP behavior stable while creating the stateless seam
  - The code already had a working stateful downstream session manager, but it was still embedded inside the HTTP module.
- [`phase3e-release-closeout-truth-pass-20260307.md`](integration-issues/phase3e-release-closeout-truth-pass-20260307.md) — Phase 3E release closeout required a truth pass across the tracked operating docs _(stale)_
  - By the time Phase 3D was merged, the code and the repo’s tracked operational docs no longer agreed about the project’s state.
- [`phase4-tui-dashboard-daemon-patterns.md`](integration-issues/phase4-tui-dashboard-daemon-patterns.md) — Phase 4 TUI Dashboard and Daemon Mode Patterns _(stale)_
  - Phase 4 introduced two major subsystems — a live TUI dashboard and a background daemon — that both need to interact with the core multiplexer engine. This creates several intersecting challenges
- [`post-v0-2-upstream-restart-recovery-proof-20260307.md`](integration-issues/post-v0-2-upstream-restart-recovery-proof-20260307.md) — Post-v0.2 upstream restart recovery was already correct; it needed an end-to-end proof
  - Post-v0.2 upstream restart recovery was already correct; it needed an end-to-end proof
- [`pre-phase-downstream-http-bearer-auth-20260307.md`](integration-issues/pre-phase-downstream-http-bearer-auth-20260307.md) — Downstream HTTP Bearer Token Authentication
  - plug's downstream HTTP server (`plug serve`) had no authentication. The only protection was origin validation middleware that rejected non-localhost origins, but this is insufficient for remote access (phone clients, remote AI assistants). ...
- [`proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md`](integration-issues/proxy-timeout-handling-semaphore-bounds-stdio-reconnect-20260306.md) — Stdio tool-call timeout and reconnect semantics under contention
  - Two coupled resilience bugs existed in the stdio tool-call path
- [`review-fixes-critical-http-auth-ipc-parity-20260307.md`](integration-issues/review-fixes-critical-http-auth-ipc-parity-20260307.md) — Critical review fixes: authenticated HTTP upstreams, daemon IPC parity, and active-call cleanup
  - The Phase 2 / Phase 3 rollout was broadly sound, but the review found a few issues that were both real and severe enough to fix immediately
- [`review-fixes-tls-backend-portability-20260307.md`](integration-issues/review-fixes-tls-backend-portability-20260307.md) — Review fixes TLS backend portability
  - Replaced rmcp's default rustls/aws-lc HTTP client path with rustls-no-provider plus an explicit ring provider install so PR 21 could pass license and cross-target CI without reintroducing OpenSSL.
- [`rmcp-sdk-integration-patterns-plug-20260303.md`](integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md) — Historical RMCP SDK integration patterns
  - Historical RMCP SDK integration patterns
- [`rmcp-streamable-http-auth-requires-raw-bearer-tokens.md`](integration-issues/rmcp-streamable-http-auth-requires-raw-bearer-tokens.md) — RMCP Streamable HTTP authentication requires raw bearer tokens
  - After Plug upgraded to the exact `rmcp = "=2.2.0"` release, static-token-authenticated Streamable HTTP upstreams could no longer initialize. Exa exposed the shared formatting defect; restoring a valid credential alone could not correct the ...
- [`roadmap-tail-closeout-resource-subscribe-ipc-parity-20260307.md`](integration-issues/roadmap-tail-closeout-resource-subscribe-ipc-parity-20260307.md) — Resource subscribe parity and cleanup closeout
  - Finished the roadmap tail by adding `resources/subscribe` and `resources/unsubscribe` with targeted `notifications/resources/updated`, including daemon-backed `plug connect` parity via interleaved IPC notifications and reconnect subscription restore, plus dead dependency cleanup.

## ui-bugs

- [`management-action-menu-repaint-jitter-cli-20260305.md`](ui-bugs/management-action-menu-repaint-jitter-cli-20260305.md) — Management action menu repaint jitter in the CLI
  - The new management views for `plug clients`, `plug servers`, and `plug tools` rendered correctly at first, but the interactive action picker behaved badly once the selector moved. The menu appeared to jump upward, and extra visual framing m...

