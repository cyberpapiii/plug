---
title: fix: OAuth reauth registration and recovery
type: fix
status: completed
date: 2026-04-21
---

# fix: OAuth reauth registration and recovery

## Overview

Fix two related OAuth recovery defects in `plug`:

- repeated re-auth for dynamically registered OAuth servers can create duplicate provider-side `plug` integrations because the login flow re-registers a new client or reuses only `client_id` without preserving redirect compatibility
- successful auth recovery can restart a server without restoring its OAuth refresh loop, leaving the server healthy for a while but no longer auto-refreshing

The end state should make re-auth feel idempotent and durable: reauth should reuse a compatible registration when possible, fall back honestly when it is not, and always restore the runtime background ownership needed for future token refresh.

## Problem Frame

Recent review and live investigation surfaced two distinct but adjacent failures in the OAuth lifecycle:

- `plug auth login` / `plug auth complete` now attempt to reuse a persisted dynamic `client_id`, but they still derive fresh redirect URIs (`http://localhost:{ephemeral_port}/callback` for browser login and `http://localhost:0/callback` for manual completion). rmcp dynamic registration binds `redirect_uris` to the value used during registration, so `client_id` reuse alone is not a safe proxy for registration reuse.
- when an OAuth server reaches `AuthRequired`, its refresh loop exits by design. Recovery paths (`plug auth login`, `plug auth complete`, `plug auth inject`) call `restart_server()`, which historically restarted the upstream but did not respawn the per-server refresh loop. A full daemon restart repaired the symptom because `Engine::start()` recreates those loops.

This work is a focused follow-up to the broader auth/runtime hardening tranche in `docs/plans/2026-04-16-001-fix-auth-runtime-stability-followups-plan.md`. It should close the remaining “reauth works but does not stay fixed” gap without reopening the larger daemon/HTTP architecture.

## Requirements Trace

- R1. Prevent repeated OAuth reauth on dynamically registered servers from creating duplicate provider-side `plug` integrations when an existing registration is still reusable.
- R2. Never reuse a dynamic OAuth registration against an incompatible redirect URI or incompatible auth-flow contract.
- R3. Ensure successful auth recovery (`auth login`, `auth complete`, `auth inject`, and restart-driven recovery) restores the OAuth refresh loop for that server.
- R4. Preserve configured `oauth_client_id` behavior and current “synthetic `injected` identity is not refreshable” safety semantics unless a real reusable registration exists.
- R5. Add regression coverage for registration reuse, redirect-compatibility gating, and restart-driven refresh-loop restoration.

## Scope Boundaries

- No redesign of rmcp’s OAuth APIs or replacement of `AuthorizationManager`.
- No attempt to delete or reconcile already-created provider-side integrations in Todoist, Krisp, Notion, or other providers.
- No expansion into the separate `plug start` readiness/launcher race beyond documenting that it remains a separate concern.

### Deferred to Separate Tasks

- Converting legacy `client_id == "injected"` credentials into reusable dynamic registrations without a new browser-based login
- Provider-specific cleanup guidance for duplicate historical integrations already created in user accounts
- Broader daemon startup/readiness hardening beyond the refresh-loop lifecycle seam

## Context & Research

### Relevant Code and Patterns

- `plug/src/commands/auth.rs` owns browser login, manual completion, and dynamic client registration decisions.
- `plug-core/src/oauth.rs` already centralizes credential snapshots, injected identity rules, and refresh behavior; any reusable registration metadata should live alongside OAuth persistence, not as CLI-only state.
- `plug-core/src/engine.rs` owns refresh-loop generations and currently respawns them on `Engine::start()` and `spawn_background_tasks_for_server()`, not on every restart path.
- `plug-core/tests/integration_tests.rs` already contains end-to-end OAuth provider fixtures suitable for asserting registration/refresh behavior without inventing a new test harness.

### Institutional Learnings

- `docs/solutions/integration-issues/2026-03-18-injected-oauth-refreshability.md`
  - refreshability must be derived from a real OAuth client identity, not merely from the presence of a refresh token
- `docs/solutions/integration-issues/2026-03-18-oauth-credential-snapshot-unification.md`
  - one canonical persisted snapshot path should drive runtime truth instead of duplicated heuristics
- `docs/solutions/integration-issues/2026-03-18-reload-truth-followup-hardening.md`
  - auth-status and auth-inject surfaces should align with live daemon truth instead of drifting across separate paths

### External References

- rmcp 1.1.0 dynamic registration semantics: `register_client(...)` binds `redirect_uris` to the supplied redirect URI and returns an `OAuthClientConfig`, not a richer registration-management object

## Key Technical Decisions

- Introduce a reusable OAuth registration concept that is richer than `client_id` alone.
  - Rationale: the review finding is correct — a dynamic registration is only reusable if the callback contract remains compatible.
- Keep browser login and manual completion on one shared registration-resolution path.
  - Rationale: `auth login` and `auth complete` must not drift into separate heuristics or they will recreate the same bug from different entry points.
- Treat refresh-loop restoration as restart lifecycle ownership, not as a side effect of auth persistence.
  - Rationale: the core bug is that recovered servers can become healthy without regaining their background refresh watcher.
- Prefer a stable redirect-contract strategy over ad hoc `client_id` reuse.
  - Rationale: if the callback URI is not stable or explicitly persisted as compatible, provider-side client reuse is unsafe.

## Open Questions

### Resolved During Planning

- Should this extend the existing April 16 auth/runtime plan?
  - No. The existing plan is broader and already mid-execution; this follow-up deserves a focused fix plan so its review findings and trade-offs stay legible.
- Should this rely on external research?
  - No. The relevant behavior is already visible in local rmcp source inspection and the repo’s OAuth plans/solutions.

### Deferred to Implementation

- Exact persisted schema for reusable dynamic registration metadata.
  - Implementation should choose the lightest durable shape that covers client identity plus redirect compatibility without inventing a full OAuth registry abstraction.
- Exact redirect contract for reused dynamic clients.
  - Implementation should decide whether this is a stable localhost callback, an explicitly persisted callback URI, or a compatibility-gated fallback, but it must be one deterministic contract shared by `auth login` and `auth complete`.

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```text
reauth recovery truth

configured client?
  yes -> use configured oauth_client_id path
  no  -> resolve persisted reusable registration
           requires: real client identity + redirect contract compatibility
           incompatible -> do not reuse silently
                          choose deterministic fallback (new registration or explicit warning)

successful auth recovery
  save credentials/registration snapshot
  -> restart_server(server)
  -> restart path must respawn refresh loop generation for oauth servers
  -> future expiry still handled without daemon restart
```

## Implementation Units

- [x] **Unit 1: Define reusable dynamic registration state**

**Goal:** Establish the durable OAuth registration state needed to distinguish “reusable dynamic client” from “mere persisted `client_id`”.

**Requirements:** R1, R2, R4

**Dependencies:** None

**Files:**
- Modify: `plug-core/src/oauth.rs`
- Modify: `plug-core/src/lib.rs`
- Test: `plug-core/src/oauth.rs`

**Approach:**
- Introduce a small persisted representation for reusable dynamic registrations that can express at least:
  - the provider-issued client identity
  - the redirect-contract shape that registration was created against
  - whether the registration is safe to reuse for future auth flows
- Keep this state adjacent to OAuth credential persistence so CLI, daemon, and refresh logic all consult the same source of truth.
- Explicitly exclude synthetic `injected` identities from reusable-registration qualification.

**Patterns to follow:**
- credential snapshot ownership in `plug-core/src/oauth.rs`
- persistence/truth consolidation patterns from `docs/solutions/integration-issues/2026-03-18-oauth-credential-snapshot-unification.md`

**Test scenarios:**
- Happy path: a configured OAuth client remains reusable without consulting dynamic-registration state.
- Happy path: a persisted dynamic registration with a compatible redirect contract is surfaced as reusable.
- Edge case: a stored `client_id == "injected"` is rejected as non-reusable even when a refresh token exists.
- Edge case: missing redirect-compatibility metadata demotes a persisted dynamic registration to non-reusable instead of guessing.
- Error path: malformed persisted registration data is ignored without breaking normal credential loading.

**Verification:**
- OAuth persistence has one explicit way to answer “is this registration reusable for reauth?”
- The new state can be loaded without changing configured-client behavior.

- [x] **Unit 2: Unify reauth registration resolution across login and complete**

**Goal:** Make `plug auth login` and `plug auth complete` resolve configured clients, reusable dynamic registrations, and new registrations through one consistent decision path.

**Requirements:** R1, R2, R4, R5

**Dependencies:** Unit 1

**Files:**
- Modify: `plug/src/commands/auth.rs`
- Test: `plug/src/commands/auth.rs`
- Test: `plug-core/tests/integration_tests.rs`

**Approach:**
- Replace the current `client_id`-only reuse helper with a registration-aware resolution helper.
- Ensure both browser login and manual completion use the same redirect-contract rules instead of deriving different callback identities opportunistically.
- If a stored dynamic registration is incompatible with the requested flow, do not silently reuse it. Either:
  - select a deterministic compatible contract, or
  - explicitly fall back to a new registration/warning path
  but never pretend the old registration is safe when it is not.

**Execution note:** Start with a failing regression for the reviewed bug: persisted dynamic `client_id` + fresh redirect URI must not be treated as a safe reuse case unless compatibility is explicit.

**Patterns to follow:**
- current auth command structure in `plug/src/commands/auth.rs`
- dynamic registration persistence intent documented in `docs/plans/2026-03-09-feat-oauth-upstream-auth-plan.md`

**Test scenarios:**
- Happy path: a second browser-based reauth for a dynamically registered server reuses the existing provider registration without creating a new one.
- Happy path: manual `auth complete` resolves the same reusable registration logic as browser login.
- Edge case: incompatible redirect-contract metadata prevents reuse and chooses the explicit fallback path.
- Edge case: configured `oauth_client_id` continues to bypass dynamic registration logic entirely.
- Error path: registration fallback failure returns a clear error instead of silently degrading into a broken reused client.
- Integration: mock OAuth provider sees one stable client registration across repeated reauth attempts for the same compatible flow.

**Verification:**
- Reauth no longer depends on raw persisted `client_id` alone.
- Browser and manual OAuth flows share one consistent registration-resolution rule.

- [x] **Unit 3: Restore refresh-loop ownership after auth recovery**

**Goal:** Ensure successful OAuth recovery recreates the per-server refresh loop so recovered servers stay healthy through the next token-expiry cycle.

**Requirements:** R3, R5

**Dependencies:** None

**Files:**
- Modify: `plug-core/src/engine.rs`
- Test: `plug-core/src/engine.rs`
- Test: `plug-core/tests/integration_tests.rs`

**Approach:**
- Make successful restart-based recovery for OAuth servers re-establish refresh-loop ownership.
- Keep generation-based deduplication authoritative so restart recovery supersedes old loops instead of multiplying them.
- Leave the health-check lifecycle alone unless implementation evidence shows it is also missing; the known proven gap is the refresh loop.

**Patterns to follow:**
- `spawn_refresh_loop_for_server(...)` generation semantics in `plug-core/src/engine.rs`
- existing reconnect/restart ownership split in `Engine::restart_server()` vs `Engine::reconnect_server()`

**Test scenarios:**
- Happy path: restarting an OAuth server increments its refresh-loop generation and leaves the server healthy.
- Edge case: repeated restart calls supersede earlier refresh generations instead of leaving multiple live loops.
- Edge case: non-OAuth restart does not create a refresh loop.
- Error path: restart failure leaves the prior refresh-loop generation unchanged.
- Integration: recovering an OAuth server from `AuthRequired` through the supported restart path restores future auto-refresh behavior without a full daemon restart.

**Verification:**
- Restart-driven recovery re-establishes refresh ownership for OAuth servers.
- Tests prove the generation moves forward on successful restart.

- [x] **Unit 4: Tighten operator guidance for non-reusable registrations**

**Goal:** Surface honest operator guidance when a stored OAuth identity cannot be reused safely, especially for legacy synthetic/incompatible registrations.

**Requirements:** R2, R4, R5

**Dependencies:** Units 1–2

**Files:**
- Modify: `plug/src/commands/auth.rs`
- Modify: `plug-core/src/oauth.rs`
- Test: `plug/src/commands/auth.rs`

**Approach:**
- Distinguish between:
  - reusable configured client
  - reusable dynamic registration
  - synthetic/incompatible registration requiring a new login contract
- Reuse the existing redaction-safe observability posture: explain the state without printing secrets or authorization URLs into tracing.
- Keep `krisp`-style legacy injected registrations honest: one proper browser login may still be required before future reauth becomes idempotent.

**Patterns to follow:**
- operator messaging patterns in `plug/src/commands/auth.rs`
- refreshability learnings in `docs/solutions/integration-issues/2026-03-18-injected-oauth-refreshability.md`

**Test scenarios:**
- Happy path: reusable dynamic registration surfaces a “reusing existing registration” message.
- Edge case: incompatible persisted registration surfaces an explicit fallback/warning path instead of a misleading reuse message.
- Edge case: injected registration with refresh token reports that reauth is still required to establish a reusable OAuth identity.
- Error path: fallback registration failure preserves the accurate operator explanation.

**Verification:**
- CLI output makes it obvious why `plug` is or is not reusing a prior OAuth registration.
- No new auth-path messaging leaks secrets or raw authorization URLs into logs.

## System-Wide Impact

- **Interaction graph:** `auth login`, `auth complete`, credential persistence, daemon restart IPC, and refresh-loop lifecycle must all agree on what constitutes a reusable OAuth identity.
- **Error propagation:** redirect incompatibility must fail honestly at client-resolution time rather than surfacing later as opaque provider auth failures.
- **State lifecycle risks:** persisted credential state and reusable-registration state must not drift from one another; a new registration should replace prior dynamic-registration truth atomically enough that the next reauth sees one coherent answer.
- **API surface parity:** browser-based login and manual completion must use the same registration-resolution logic.
- **Integration coverage:** provider-side dynamic registration behavior and restart-driven refresh-loop ownership both require integration tests, not just unit coverage.
- **Unchanged invariants:** configured `oauth_client_id` servers keep their current direct-client behavior, and synthetic `injected` identities remain non-reusable until a real OAuth client identity is established.

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Reauth fix still duplicates integrations for legacy synthetic registrations | Preserve explicit fallback/warning behavior and document that one clean browser login is the migration path |
| Redirect-contract strategy adds new localhost binding complexity | Keep the contract helper centralized and cover both browser and manual flows with integration tests |
| Refresh-loop respawn fix introduces duplicate background tasks | Reuse the existing generation counter and add restart-focused regression coverage |
| This work gets conflated with the separate `plug start` readiness issue | Keep launcher/readiness explicitly out of scope in this plan and avoid mixing fixes unless new evidence forces it |

## Documentation / Operational Notes

- Existing provider-side duplicate integrations are not cleaned up by this work; the plan only prevents creating new duplicates once a reusable registration exists.
- Legacy `krisp` state may still require one real browser-based login before the account moves onto the reusable-registration path.

## Sources & References

- Related plan: `docs/plans/2026-04-16-001-fix-auth-runtime-stability-followups-plan.md`
- Related plan: `docs/plans/2026-03-09-feat-oauth-upstream-auth-plan.md`
- Related learning: `docs/solutions/integration-issues/2026-03-18-injected-oauth-refreshability.md`
- Related learning: `docs/solutions/integration-issues/2026-03-18-oauth-credential-snapshot-unification.md`
- Related learning: `docs/solutions/integration-issues/2026-03-18-reload-truth-followup-hardening.md`
- Related code: `plug/src/commands/auth.rs`
- Related code: `plug-core/src/oauth.rs`
- Related code: `plug-core/src/engine.rs`
