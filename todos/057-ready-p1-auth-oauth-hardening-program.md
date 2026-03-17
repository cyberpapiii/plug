---
status: ready
priority: p1
issue_id: "057"
tags: [auth, oauth, ux, config, doctor, repair, status, transport, standards]
dependencies: []
---

# Auth / OAuth Hardening Program

## Problem Statement

`plug`'s auth, OAuth, config, doctor, setup, repair, and status surfaces are no longer aligned with
the actual complexity of the product. The runtime engine is substantial, but the standards-facing
downstream OAuth behavior, operator diagnostics, transport/auth topology UX, and client-setup flows
are not complete enough for reliable real-world use across all supported scenarios.

## Findings

- Downstream OAuth discovery currently has standards and privacy gaps.
- Downstream OAuth metadata does not always match actual supported auth methods.
- `plug doctor`, `plug status`, and `plug auth status` can contradict each other.
- Auth recovery paths are fragmented and too engineer-readable.
- Setup and repair flows are too blunt and flatten client topology to `plug connect`.
- Status/menu UX does not clearly show downstream transport, upstream transport, and auth mode per
  client/server.
- Plaintext token fallback warnings are ambiguous and not deterministic enough for operators.

Primary references:

- [docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md](../docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md)
- [docs/plans/2026-03-16-post-reconcile-backlog.md](../docs/plans/2026-03-16-post-reconcile-backlog.md)

## Proposed Solutions

### Option 1: Execute the full hardening plan in order

Pros:
- resolves the standards issue first
- reduces operator confusion systematically
- gives a clean end-to-end test matrix

Cons:
- multi-task effort
- touches both `plug-core` and `plug`

### Option 2: Only patch the standards/security issues

Pros:
- fastest path to interoperability fixes
- smallest code footprint

Cons:
- leaves the bigger operator UX and setup/repair problems unresolved

### Option 3: Only improve UX/status/doctor first

Pros:
- immediate usability gains
- easier daily operation

Cons:
- leaves real downstream OAuth standards/interoperability bugs in place

## Recommended Action

Execute the full hardening plan from [docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md](../docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md) in this order:

1. downstream OAuth standards/privacy/auth-challenge fixes
2. doctor/runtime/auth-state consistency
3. auth recovery UX improvements
4. setup/repair topology awareness
5. transport/auth visibility in status and menu views
6. end-to-end scenario matrix

## Acceptance Criteria

- [x] Task 1 complete: downstream OAuth discovery returns minimal protected card when unauthenticated
- [x] Task 2 complete: downstream OAuth metadata advertises only supported token endpoint auth methods
- [x] Task 3 complete: unauthorized downstream OAuth responses provide standards-appropriate discovery cues
- [x] Task 4 complete: `doctor` distinguishes daemon-in-use from true port conflict
- [x] Task 5 complete: `doctor` separates cold reachability from daemon-observed health and auth state
- [x] Task 6 complete: `status`, `auth status`, and server views expose explicit auth recovery categories
- [x] Task 7 complete: token storage mode warnings are deterministic and actionable
- [x] Task 8 complete: setup supports explicit client topology choice instead of assuming stdio bridge
- [x] Task 9 complete: repair preserves client-specific transport choices
- [x] Task 10 complete: status/menu views surface transport and auth topology clearly
- [x] Task 11 complete: integration tests cover mixed auth and topology scenarios end to end
- [x] Final verification complete: `cargo test` passes
- [x] Final verification complete: `cargo build --release` passes
- [x] Final verification complete: `plug status`, `plug auth status`, and `plug doctor` tell a coherent story on healthy, auth-required, and failed server cases

## Technical Details

Key files expected to change:

- `plug-core/src/http/server.rs`
- `plug-core/src/http/error.rs`
- `plug-core/src/downstream_oauth/mod.rs`
- `plug-core/src/doctor.rs`
- `plug-core/src/oauth.rs`
- `plug/src/commands/auth.rs`
- `plug/src/commands/servers.rs`
- `plug/src/views/overview.rs`
- `plug/src/views/servers.rs`
- `plug/src/ui.rs`
- `plug-core/tests/integration_tests.rs`

## Resources

- [docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md](../docs/plans/2026-03-16-auth-oauth-hardening-ux-plan.md)
- [docs/audit/BRANCH-LINEAGE-2026-03-16.md](../docs/audit/BRANCH-LINEAGE-2026-03-16.md)
- MCP Authorization spec: <https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization>
- OAuth 2.0 Authorization Server Metadata (RFC 8414): <https://www.rfc-editor.org/rfc/rfc8414.html>
- OAuth 2.0 Security BCP (RFC 9700): <https://www.rfc-editor.org/rfc/rfc9700>

## Work Log

### 2026-03-16 - Program tracker created

**By:** Codex

**Actions:**
- Consolidated the full auth/OAuth hardening effort into one top-level todo tracker.
- Mapped the execution order directly to the implementation plan.
- Captured the standards, UX, transport, and diagnostics work in one checklist.
- Noted that Task 1 has already been prototyped in a subagent and should be integrated/reviewed next.

**Learnings:**
- The system is not blocked on a single auth bug; it needs an end-to-end hardening pass.
- The biggest product gap is not raw auth capability, but the mismatch between runtime complexity
  and the CLI/operator UX that surfaces it.

### 2026-03-16 - Downstream OAuth and operator UX hardening slice 1

**By:** Codex

**Actions:**
- Fixed downstream OAuth discovery privacy so unauthenticated OAuth-mode discovery returns the
  minimal protected card instead of full inventory.
- Corrected downstream OAuth metadata so advertised token endpoint auth methods match runtime
  support.
- Enriched downstream OAuth unauthorized responses with richer bearer challenge metadata.
- Improved `doctor` so a running daemon occupying the configured port is treated as expected rather
  than a hard failure.
- Reduced contradiction in `doctor` cold connectivity reporting when the daemon is already running.
- Made downstream OAuth doctor output less overconfident and token-file fallback warnings explicit.
- Changed daemon auth-status fallback from optimistic `Healthy` to `Degraded` when credentials exist
  but no live runtime status is present.
- Updated `plug auth status` to use live daemon auth state when available and emit clearer recovery
  hints.
- Made `repair` preserve existing linked client transport topology instead of flattening everything
  to stdio.
- Surfaced linked client transport in overview and upstream transport/auth in status/server views.

**Commits:**
- `2a936c9` `fix(oauth): harden downstream oauth discovery and metadata`
- `6326554` `fix(doctor): improve auth and runtime diagnostics`
- `0352544` `feat(ux): preserve client transport topology`
- `12b4d86` `feat(setup): prompt for client transport choice`
- `9033da0` `feat(ux): separate auth-required server summary`
- `0ca32cd` `feat(doctor): add live runtime health and auth context`
- `0aba2a7` `fix(doctor): detect running daemon pid path correctly`
- `e9d1de4` `feat(status): add recovery guidance for server states`

**Learnings:**
- The highest-leverage fixes were standards alignment and reducing contradictory operator signals.
- The setup/repair UX still needs deeper topology-aware configuration flows; preserving topology is
  a necessary first step, not the final one.
- We now have the first end-to-end transport/auth visibility layer, but `doctor`, setup, and
  recovery still need more explicit modeling of mixed-fleet scenarios.
- `plug doctor` now includes daemon-observed runtime health/auth context so cold checks and live
  state can be compared in one command, which reduces the biggest contradiction from real usage.

### 2026-03-17 - Client endpoint topology hardening slice

**By:** Codex

**Actions:**
- Promoted the downstream HTTP MCP endpoint to a derived first-class value instead of rebuilding
  `http://localhost:3282/mcp` ad hoc in each command.
- Wired link, repair, and custom client snippets to export the configured HTTP endpoint, including
  `public_base_url` when present.
- Added linked-client parsing that captures both mode and configured endpoint from JSON, TOML, and
  YAML client configs.
- Surfaced linked client mode and endpoint in `plug clients`.
- Surfaced the active downstream HTTP endpoint in `plug status` and overview.
- Added unit coverage for derived downstream endpoint resolution and linked-client config parsing.
- Re-ran the full test suite and release build outside the sandbox so socket/listener-based auth and
  daemon tests could verify correctly.

**Verification:**
- `cargo test`
- `cargo build --release`
- `cargo run --quiet --bin plug -- clients`
- `cargo run --quiet --bin plug -- status`
- `cargo run --quiet --bin plug -- doctor`

**Learnings:**
- Preserving transport alone was not enough; users also need the concrete downstream endpoint to
  understand what a client is actually pointed at.
- `public_base_url` changes the correct export target for HTTP-linked clients and has to flow
  through setup, repair, and status together or the UX becomes misleading again.
- The remaining hardening work is now mostly about broader scenario coverage and server-auth setup
  ergonomics, not basic topology visibility.

### 2026-03-17 - Server auth setup scaffolding slice

**By:** Codex

**Actions:**
- Added explicit upstream auth modeling helpers for remote HTTP/SSE servers.
- Updated interactive server add/edit flows to ask for auth mode instead of silently defaulting to
  unauthenticated remotes.
- Supported three explicit upstream auth paths in the CLI:
  - none
  - bearer token
  - oauth (authorization-code + PKCE) with optional pre-registered client ID and scopes
- Added post-save guidance to run `plug auth login --server <name>` when an upstream is configured
  for OAuth.
- Added unit coverage for scope parsing and remote auth selection application.

**Verification:**
- `cargo test -p plug parse_scope_list_ignores_empty_entries -- --nocapture`
- `cargo test -p plug apply_remote_auth_selection_sets_oauth_fields -- --nocapture`
- `cargo test -p plug current_remote_auth_selection_prefers_oauth_state -- --nocapture`
- `cargo test -p plug`
- `cargo build --release`

**Learnings:**
- Status and doctor can only be clear if configuration entry points force users to declare auth
  intent explicitly instead of back-filling it later by editing TOML.
- The remaining gap is now less about visibility and more about broader scenario coverage and
  richer non-interactive config paths for the same auth choices.

### 2026-03-17 - Server auth guardrails and auth-status test cleanup

**By:** Codex

**Actions:**
- Rejected empty bearer-token submissions in interactive remote server auth setup so the CLI does
  not save a misleading `auth_token = ""` state.
- Added cleanup for the new daemon auth-status tests so temp config directories and seeded
  credential-store entries do not accumulate after test runs.
- Re-ran the focused server-auth and auth-status coverage after tightening those guardrails.

**Verification:**
- `cargo test -p plug commands::servers::tests -- --nocapture`
- `cargo test -p plug daemon::tests::auth_status -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- Interactive auth scaffolding needs validation guardrails, not just more prompts.
- The remaining server-auth gap is primarily about non-interactive/scripted config paths, not basic
  interactive UX anymore.

### 2026-03-17 - End-to-end auth and topology scenario matrix

**By:** Codex

**Actions:**
- Added an integration test that starts one engine with a mixed runtime fleet and verifies the
  resulting server-state distinctions stay explicit across healthy stdio, healthy OAuth,
  auth-required OAuth, and failed upstream cases.
- Added an end-to-end downstream OAuth discovery test that exercises the real HTTP router,
  validates the minimal protected card when unauthenticated, performs a real authorization-code +
  PKCE exchange, and confirms authenticated discovery and MCP requests succeed afterward.
- Re-ran the full test suite and release build after landing the new matrix coverage.

**Verification:**
- `cargo test -p plug-core test_engine_mixed_auth_fleet_reports_distinct_server_states -- --nocapture`
- `cargo test -p plug-core test_downstream_oauth_protected_discovery_card_end_to_end -- --nocapture`
- `cargo test -- --nocapture`
- `cargo build --release`

**Learnings:**
- The highest-value missing coverage was not another parser test; it was proving the runtime can
  hold multiple auth states at once without collapsing them into a simpler story.
- Downstream OAuth discovery is now covered at both the HTTP-server test boundary and the real
  routed integration boundary, which makes the standards-facing behavior much less inference-driven.
- The main remaining auth/config work is now about richer configuration paths and doctor-specific
  end-to-end scenarios, not basic runtime correctness.

### 2026-03-17 - Non-interactive upstream auth configuration slice

**By:** Codex

**Actions:**
- Added explicit `plug server add` flags for remote auth intent:
  - `--auth none|bearer|oauth`
  - `--bearer-token`
  - `--oauth-client-id`
  - `--oauth-scopes`
- Made the non-interactive add flow infer auth intent from bearer/oauth-specific flags while
  rejecting contradictory combinations.
- Kept remote auth flags scoped to HTTP/SSE upstreams so scripted stdio setup cannot silently
  accept meaningless auth arguments.
- Added focused unit coverage for bearer inference, oauth inference, and conflicting-flag
  rejection.

**Verification:**
- `cargo test -p plug commands::servers::tests -- --nocapture`
- `cargo test -p plug tests::serve_command -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- The product was still better in the interactive path than in the scripted CLI path; that gap is
  now materially smaller.
- The remaining server-auth UX gap is mostly about non-interactive edit/update flows, not new-add
  workflows.

### 2026-03-17 - Doctor interpretation clarity slice

**By:** Codex

**Actions:**
- Added a small synthesis layer on top of the existing doctor checks so `plug doctor` can explain
  the difference between:
  - cold connectivity problems with a still-healthy daemon
  - runtime failures despite basic reachability
  - auth attention needed even when the runtime is broadly healthy
- Kept the interpretation logic pure and testable instead of baking more heuristics directly into
  the rendering path.
- Added focused tests for the new interpretation cases and re-ran the full `plug` suite.

**Verification:**
- `cargo test -p plug commands::misc::tests -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- The raw checks were already better than before, but users still had to mentally reconcile them.
- The biggest remaining diagnostics gap is now fixture depth, not wording: we still need broader
  end-to-end doctor/runtime scenarios if we want the same confidence level as the auth/topology
  integration matrix.

### 2026-03-17 - Scripted upstream edit parity slice

**By:** Codex

**Actions:**
- Extended `plug server edit` with non-interactive field updates for:
  - `--command`
  - `--args`
  - `--url`
  - `--auth`
  - `--bearer-token`
  - `--oauth-client-id`
  - `--oauth-scopes`
- Reused the same auth-intent inference and conflicting-flag validation as `server add` so scripted
  maintenance does not behave differently from scripted creation.
- Kept remote auth and URL flags scoped to HTTP/SSE servers and rejected those flags on stdio
  servers to avoid misleading partial updates.

**Verification:**
- `cargo test -p plug commands::servers::tests -- --nocapture`
- `cargo test -p plug tests::serve_command -- --nocapture`
- `cargo test -p plug views::servers -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- The product no longer has a sharp divide between “you can script server creation” and “you must
  click through prompts to maintain it.”
- The remaining scripted-config gap is narrower now: transport-shape changes and deeper server
  mutation flows still need deliberate UX design rather than more raw flags.

### 2026-03-16 - Client topology fidelity slice

**By:** Codex

**Actions:**
- Extended client-link parsing to preserve the actual linked HTTP endpoint, not just `stdio` vs
  `http`.
- Made `repair` reuse an already-linked client endpoint when one is present, instead of always
  regenerating HTTP clients against the current default endpoint.
- Kept export generation endpoint-aware so custom/public URLs survive regeneration across JSON,
  TOML, YAML, and VS Code-style shapes.
- Expanded the client inventory to show both linked mode and linked endpoint, which makes mixed
  local-vs-remote fleets easier to reason about.
- Added focused tests for explicit HTTP export URLs and for linked-client endpoint parsing across
  JSON, TOML, and YAML formats.
- Added export→parse round-trip tests for JSON, TOML, and YAML client shapes so endpoint fidelity
  is proven across the actual repair/export path rather than parser-only coverage.

**Verification:**
- `cargo test -p plug-core export_ -- --nocapture`
- `cargo test -p plug clients::tests -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- Preserving transport mode alone is not enough; endpoint fidelity matters because users may link
  some clients to a remote/public URL and others to a local daemon.
- The right UX model is “what is this client pointed at?” rather than only “is it stdio or http?”.
- This closes a real repair/setup footgun, but the broader mixed-scenario integration matrix still
  needs explicit end-to-end coverage.

### 2026-03-16 - Topology-aware client export and inventory slice

**By:** Codex

**Actions:**
- Made client export/link/custom-config HTTP snippets derive from the configured downstream endpoint
  instead of hard-coding `http://localhost:3282/mcp`.
- Reused the current linked client endpoint during `plug repair` so existing remote/public HTTP
  client configs are refreshed without being flattened back to localhost.
- Added client-config parsing helpers that recover both linked transport and linked HTTP endpoint
  across JSON, TOML, and YAML client config shapes.
- Surfaced linked client mode and endpoint in `plug clients`.
- Surfaced the active downstream HTTP endpoint in overview/status so operators can see what HTTP
  clients should actually use.
- Added focused tests for configured endpoint derivation and linked client config parsing.

**Verification:**
- `cargo test -p plug configured_http_export_url -- --nocapture`
- `cargo test -p plug-core export_http_uses_explicit_url_when_provided -- --nocapture`
- `cargo build --release`
- `cargo test` in sandbox still fails on existing listener/socket permission-restricted cases
  (`commands::auth` callback tests, HTTPS runtime test, daemon socket IPC restart tests), but the
  new client-topology tests pass.

**Learnings:**
- Preserving transport alone was not enough; repair also needed to preserve the actual exported HTTP
  endpoint or it could still rewrite remote/public client configs incorrectly.
- The client inventory needed endpoint visibility, not just a boolean linked/not-linked state, to
  reduce confusion for mixed local and remote HTTP setups.

### 2026-03-16 - Daemon auth-state scenario coverage

**By:** Codex

**Actions:**
- Added daemon-level tests for the `AuthStatus` IPC surface so the auth categories shown by
  `plug auth status` and the live runtime doctor checks are pinned to explicit scenarios.
- Covered the three fallback rules that matter most to operator clarity:
  - no credentials -> `AuthRequired`
  - credentials present but no runtime status -> `Degraded`
  - runtime `AuthRequired` beats cached credentials
- Fixed two compile blockers in `plug/src/commands/servers.rs` that surfaced when compiling the
  expanded test matrix.
- Re-ran the full workspace test suite and release build after the new coverage landed.

**Verification:**
- `cargo test -p plug daemon::tests::auth_status -- --nocapture`
- `cargo test -p plug -- --nocapture`
- `cargo test -- --nocapture`
- `cargo build --release`

**Learnings:**
- The daemon auth-status seam is where auth truth has to be pinned, because both operator-facing
  auth messaging and the richer doctor runtime context depend on it.
- Runtime state must outrank the mere existence of cached credentials, or the UX slides back into
  the same ambiguous “credentials exist, so maybe things are fine” model we were trying to remove.

### 2026-03-16 - Client inventory graceful fallback

**By:** Codex

**Actions:**
- Changed `plug clients` to keep rendering linked/detected client inventory even when the daemon
  cannot be started.
- Added explicit warning text that live client inspection is unavailable and that the view is
  falling back to config-derived inventory only.
- Kept the live summary truthful by surfacing `unavailable` instead of implying zero live clients.

**Verification:**
- `cargo run --quiet --bin plug -- clients`

**Learnings:**
- Visibility commands should not depend on daemon startup when most of their value comes from static
  config state.
- `live = 0` and `live inspection unavailable` are materially different operator states and need to
  stay separate in the UI.

### 2026-03-16 - Server inventory graceful fallback and recovery parity

**By:** Codex

**Actions:**
- Changed `plug servers` to keep rendering configured server inventory even when the daemon cannot
  be started.
- Added explicit notice when live runtime inspection is unavailable so the user can distinguish
  configured inventory from live health.
- Added recovery guidance to `plug servers` for auth-required, failed, and degraded states so its
  next-step language matches `plug status`.

**Verification:**
- `cargo run --quiet --bin plug -- servers`
- `cargo run --quiet --bin plug -- clients`

**Learnings:**
- Inventory-style commands should degrade to config truth instead of failing outright.
- Recovery guidance needs to follow the user across multiple surfaces; otherwise the same state
  still feels ambiguous depending on which command they happen to run first.

### 2026-03-17 - Doctor runtime summary clarity slice

**By:** Codex

**Actions:**
- Split the live runtime doctor output into a summary line plus a named `runtime_failures` line so
  the daemon-wide view does not collapse into one blunt failure state.
- Kept the summary at warning level when the daemon is running but some servers need attention,
  while preserving a hard failure signal for the specific failed servers.
- Added focused tests proving the summary/failure split and updated the interpretation tests to
  follow the more explicit model.
- Tightened cold HTTP reachability checks so doctor tries all resolved addresses with explicit DNS
  timeout handling instead of trusting the first resolved socket only.

**Verification:**
- `cargo test -p plug commands::misc::tests -- --nocapture`
- `cargo test -p plug -- --nocapture`
- `cargo test -p plug-core doctor -- --nocapture`
- `cargo build --release`

**Learnings:**
- Operators read "the daemon is up" and "these servers are failing" as two different questions, so
  the command surface should encode them separately.
- Reducing confusion sometimes means adding one extra explicit line, not compressing everything
  into one status.
- Cold connectivity checks need their own robustness work too, or they keep undermining the clearer
  runtime/auth UX with avoidable false negatives.

### 2026-03-17 - Upstream target visibility slice

**By:** Codex

**Actions:**
- Added a shared server-target summarizer so operator surfaces can show what each upstream is
  actually pointed at instead of only `stdio` / `http` / `sse`.
- Updated `plug status` to include a `TARGET` column beside upstream transport and auth mode.
- Updated `plug servers` to show the same target information in both live runtime inventory and
  config-only fallback inventory.
- Added focused unit coverage for stdio argument rendering, HTTP URL rendering, and truncation.

**Verification:**
- `cargo test -p plug -- --nocapture`
- `./target/debug/plug servers`
- `./target/debug/plug status`

**Learnings:**
- Transport type without target is still too abstract for real operator reasoning.
- The right question is not just "what kind of upstream is this?" but "what exact thing is this
  server talking to right now?".

### 2026-03-17 - Non-interactive doctor credential slice

**By:** Codex

**Actions:**
- Changed `plug doctor` OAuth token checks to inspect plaintext fallback files only, instead of
  probing the live credential store and triggering macOS Keychain prompts.
- Kept the actionable path by pointing operators to `plug auth status` for live credential state.
- Made cold server-connectivity checks build an explicit concurrent future set so the fanout stays
  deterministic while avoiding the keychain-backed credential path entirely.

**Verification:**
- `cargo test -p plug-core doctor -- --nocapture`
- `cargo test -p plug -- --nocapture`

**Learnings:**
- Diagnostics need to stay non-interactive or they stop being diagnostics and start acting like
  side-effectful auth flows.
- `plug doctor` should report storage mode and cold connectivity, while `plug auth status` remains
  the command that touches live credential state.

### 2026-03-17 - Fallback topology parity slice

**By:** Codex

**Actions:**
- Added shared transport/auth summarizers so live and fallback inventories use the same labels.
- Updated daemon-unavailable `plug servers` fallback output to retain upstream transport, auth, and
  target detail instead of dropping back to bare names.
- Updated daemon-unavailable `plug status` fallback output the same way so runtime loss does not
  also hide the config topology you need to diagnose.
- Added focused unit coverage for the shared transport/auth summarizers.

**Verification:**
- `cargo test -p plug -- --nocapture`
- `./target/debug/plug status`
- `./target/debug/plug servers`

**Learnings:**
- The daemon being down is exactly when config topology matters most, so fallback output has to
  preserve structure rather than simplify it away.
- Shared summarizers are worth it here because inconsistent labels across commands would recreate
  the same confusion under a different banner.

### 2026-03-17 - Non-interactive doctor and upstream target visibility

**By:** Codex

**Actions:**
- Removed keychain-backed credential probing from `plug doctor` so diagnostics no longer risk
  hanging behind macOS Keychain prompts.
- Parallelized cold server reachability checks so slow HTTP/SSE upstreams do not turn doctor into a
  serialized 30-60 second command on larger fleets.
- Added a reusable server-target summarizer and surfaced each upstream target directly in
  `plug status` and `plug servers`.
- Verified that live health lines now show not just transport/auth mode, but also the actual URL or
  command each server is using.

**Verification:**
- `cargo test -p plug commands::misc::tests -- --nocapture`
- `cargo test -p plug views::servers -- --nocapture`
- `cargo test -p plug ui::tests -- --nocapture`
- `cargo test -p plug-core connectivity -- --nocapture`
- `timeout 20s cargo run --quiet --bin plug -- doctor`
- `timeout 15s target/debug/plug status`
- `timeout 15s target/debug/plug servers`

**Learnings:**
- Diagnostic commands should stay non-interactive; touching the keychain from `doctor` makes the
  command less trustworthy, not more complete.
- Transport and auth labels alone are not enough once the system supports multiple remote and
  stdio shapes; operators need the concrete target inline to reason about what they are actually
  inspecting.
