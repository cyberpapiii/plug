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
- [ ] Task 5 complete: `doctor` separates cold reachability from daemon-observed health and auth state
- [x] Task 6 complete: `status`, `auth status`, and server views expose explicit auth recovery categories
- [x] Task 7 complete: token storage mode warnings are deterministic and actionable
- [x] Task 8 complete: setup supports explicit client topology choice instead of assuming stdio bridge
- [x] Task 9 complete: repair preserves client-specific transport choices
- [x] Task 10 complete: status/menu views surface transport and auth topology clearly
- [ ] Task 11 complete: integration tests cover mixed auth and topology scenarios end to end
- [ ] Final verification complete: `cargo test` passes
- [ ] Final verification complete: `cargo build --release` passes
- [ ] Final verification complete: `plug status`, `plug auth status`, and `plug doctor` tell a coherent story on healthy, auth-required, and failed server cases

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

**Learnings:**
- The highest-leverage fixes were standards alignment and reducing contradictory operator signals.
- The setup/repair UX still needs deeper topology-aware configuration flows; preserving topology is
  a necessary first step, not the final one.
- We now have the first end-to-end transport/auth visibility layer, but `doctor`, setup, and
  recovery still need more explicit modeling of mixed-fleet scenarios.
