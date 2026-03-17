# Auth / OAuth Hardening and UX Reconciliation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `plug`'s auth, OAuth, config, doctor, setup, repair, and status surfaces reliable and understandable across all supported transport and auth combinations.

**Architecture:** Keep the existing core auth engine, but harden the standards-facing downstream OAuth surface and bring the operator UX up to the same fidelity as the runtime. The implementation should separate transport truth, auth truth, and recovery guidance so status, doctor, and repair stop contradicting each other.

**Tech Stack:** Rust, `plug-core`, `plug`, Streamable HTTP, legacy SSE, stdio, OAuth 2.1 + PKCE, MCP 2025-11-25

---

## Scope

This plan covers three connected workstreams:

1. downstream OAuth standards / interoperability hardening
2. auth and connectivity diagnostics / recovery UX
3. setup / repair / status / menu visibility for transport and auth topology

## Review Inputs

This plan is based on:

- live `plug status` / `plug doctor` output from real usage
- code review findings across auth/OAuth/config/transport flows
- standards alignment concerns around MCP authorization and OAuth metadata

## Success Criteria

- `plug doctor`, `plug status`, and `plug auth status` no longer contradict each other on common auth scenarios
- downstream OAuth discovery and metadata match runtime behavior
- transport/auth topology is visible to operators
- setup and repair flows respect different client and config types instead of flattening everything to `plug connect`
- end-to-end tests cover mixed transport/auth scenarios that currently rely on inference

## Current Status - 2026-03-17

Completed so far:
- downstream OAuth discovery, metadata, and unauthorized challenge hardening
- daemon-aware `doctor` output with separate runtime health/auth context
- explicit auth recovery guidance in `status` and `auth status`
- transport-preserving repair and interactive link/setup transport choice
- downstream HTTP endpoint awareness across export/link/repair/status/clients
- interactive server add/edit auth scaffolding for none, bearer, and oauth upstreams
- non-interactive `server add` auth flags for bearer and oauth remote upstreams
- non-interactive `server edit` auth and URL updates for remote upstreams
- end-to-end scenario coverage for mixed runtime fleet states and downstream OAuth protected discovery
- doctor interpretation that explains live-runtime versus cold-connectivity mismatches explicitly
- doctor cold connectivity checks that avoid keychain prompts and run concurrently across the fleet
- status/server inventory rows that surface each upstream target directly

Still remaining:
- deeper server add/edit auth scaffolding so common HTTP/SSE auth cases are not still hand-authored
- richer non-interactive/server-scripted transport-shape edits beyond auth/URL/command/args
- broader doctor-level scenario coverage that exercises live runtime state versus cold reachability
- final UX cleanup where command surfaces still imply a simpler topology than the runtime actually supports

## Progress Notes

- 2026-03-16: Downstream OAuth discovery/privacy, metadata, and 401 challenge behavior were
  hardened; `doctor`/`status`/`auth status` gained clearer runtime/auth recovery modeling.
- 2026-03-16: Setup/link/repair now preserve client transport topology and expose transport/auth
  shape more clearly.
- 2026-03-16: Client endpoint fidelity is now preserved during repair/export regeneration, and the
  client inventory surfaces linked mode plus endpoint so local-vs-remote usage is visible.
- 2026-03-17: Interactive server add/edit now scaffold remote upstream auth intent directly, but
  the equivalent non-interactive flag surface is still missing and remains follow-up work.
- 2026-03-17: `plug server add` now supports non-interactive auth intent for bearer and oauth
  remotes, which closes the biggest scripted-config gap; `server edit` remains interactive-only.
- 2026-03-17: `plug server edit` now supports scripted auth and field updates for the same remote
  auth choices, so routine maintenance no longer forces prompt-driven editing.
- 2026-03-17: The integration matrix now covers mixed engine fleet states plus downstream OAuth
  protected discovery through the real HTTP router.
- 2026-03-17: `plug doctor` now synthesizes an explicit interpretation when cold connectivity and
  live daemon state disagree, which reduces the biggest remaining diagnostics ambiguity.
- 2026-03-17: `plug doctor` now separates the live runtime summary from named failing servers, so
  "the daemon is up" and "these specific servers are failing" are surfaced as distinct operator
  facts instead of one blunt red summary.
- 2026-03-17: cold HTTP reachability checks now try all resolved addresses with bounded DNS
  timeout handling, which reduces misleading failures on multi-address hosts.
- 2026-03-17: `plug doctor` no longer probes keychain-backed credentials, so diagnostics stay
  non-interactive on macOS instead of hanging behind a Keychain prompt.
- 2026-03-17: `plug status` and `plug servers` now include each server's concrete target (URL or
  command) so operators can immediately see what a health line actually points at.
- Remaining gap: deeper `doctor` command/runtime scenarios still rely more on focused tests than
  full end-to-end command fixtures.

## Workstream A: Downstream OAuth Standards Hardening

### Task 1: Add failing tests for downstream OAuth discovery privacy

**Files:**
- Modify: `plug-core/src/http/server.rs`

**Step 1: Write the failing test**

Add a test that exercises `/.well-known/mcp.json` under downstream OAuth mode and asserts unauthenticated clients receive the minimal protected card, not full server inventory.

**Step 2: Run the focused test**

Run: `cargo test -p plug-core oauth_mode_discovery -- --nocapture`

Expected: FAIL showing the full card is currently leaked.

**Step 3: Fix `get_server_card()` auth gating**

Update the downstream auth check so OAuth mode is treated as protected when the route is externally exposed, instead of inferring protection only from `auth_token`.

**Step 4: Re-run the focused test**

Run: `cargo test -p plug-core oauth_mode_discovery -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/http/server.rs
git commit -m "fix(http): protect oauth discovery card for unauthenticated clients"
```

### Task 2: Make downstream OAuth metadata advertise only supported auth methods

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Inspect: `plug-core/src/downstream_oauth/mod.rs`

**Step 1: Write failing tests for metadata auth methods**

Add tests covering:
- public client case: `none` only
- confidential client case: `client_secret_basic` and/or `client_secret_post`

**Step 2: Run the focused tests**

Run: `cargo test -p plug-core token_endpoint_auth_methods -- --nocapture`

Expected: FAIL because metadata is currently over-advertised.

**Step 3: Fix metadata generation**

Derive `token_endpoint_auth_methods_supported` from actual runtime configuration, not a hard-coded superset.

**Step 4: Re-run focused tests**

Run: `cargo test -p plug-core token_endpoint_auth_methods -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/http/server.rs
git commit -m "fix(oauth): align downstream metadata with supported auth methods"
```

### Task 3: Strengthen 401 challenge behavior for generic OAuth clients

**Files:**
- Modify: `plug-core/src/http/error.rs`
- Modify: `plug-core/src/http/server.rs`

**Step 1: Add failing tests for unauthorized response headers/body**

Cover protected downstream OAuth requests and assert the response includes standards-appropriate discovery cues and metadata pointers.

**Step 2: Run the focused tests**

Run: `cargo test -p plug-core unauthorized_oauth -- --nocapture`

Expected: FAIL

**Step 3: Implement minimal compliant challenge behavior**

Enrich the 401 path so general MCP/OAuth clients can discover the right next step without prior hand-tuning.

**Step 4: Re-run focused tests**

Run: `cargo test -p plug-core unauthorized_oauth -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/http/error.rs plug-core/src/http/server.rs
git commit -m "fix(oauth): improve downstream unauthorized challenge metadata"
```

## Workstream B: Diagnostics and Recovery UX

### Task 4: Make `doctor` distinguish daemon-in-use from port conflict

**Files:**
- Modify: `plug-core/src/doctor.rs`

**Step 1: Add failing tests for healthy daemon using configured port**

Cover the case where the configured port is already bound by the running `plug` daemon and assert the result is pass/info rather than fail.

**Step 2: Run focused tests**

Run: `cargo test -p plug-core port_available -- --nocapture`

Expected: FAIL

**Step 3: Implement daemon-aware port logic**

Detect the healthy self-owned port case and downgrade the result from hard failure.

**Step 4: Re-run focused tests**

Run: `cargo test -p plug-core port_available -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/doctor.rs
git commit -m "fix(doctor): avoid false port failure when daemon is healthy"
```

### Task 5: Split raw reachability from daemon-observed health

**Files:**
- Modify: `plug-core/src/doctor.rs`

**Step 1: Add failing tests for contradictory health/connectivity states**

Cover cases where:
- daemon is healthy but raw TCP connect fails
- stored creds exist but server is still `AuthRequired`

**Step 2: Run focused tests**

Run: `cargo test -p plug-core connectivity -- --nocapture`

Expected: FAIL

**Step 3: Refactor doctor reporting**

Separate:
- cold reachability
- live daemon-observed health
- auth state
- credential storage state

**Step 4: Re-run focused tests**

Run: `cargo test -p plug-core connectivity -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/doctor.rs
git commit -m "refactor(doctor): separate reachability, runtime health, and auth state"
```

### Task 6: Unify auth recovery messaging

**Files:**
- Modify: `plug/src/commands/auth.rs`
- Modify: `plug/src/views/servers.rs`
- Modify: `plug/src/views/overview.rs`
- Modify: `plug/src/ui.rs`

**Step 1: Add failing tests / snapshots for mixed auth states**

Cover:
- token exists but server is `AuthRequired`
- no token exists
- server failed for non-auth reasons

**Step 2: Run focused tests**

Run: `cargo test auth_status -- --nocapture`

Expected: FAIL or missing coverage

**Step 3: Implement explicit auth-state categories**

Surface at least:
- no credentials
- credentials present, unusable
- authenticated and healthy
- auth required with specific next action

**Step 4: Re-run focused tests**

Run: `cargo test auth_status -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug/src/commands/auth.rs plug/src/views/servers.rs plug/src/views/overview.rs plug/src/ui.rs
git commit -m "feat(ux): make auth state and recovery paths explicit"
```

### Task 7: Make token storage warnings deterministic

**Files:**
- Modify: `plug-core/src/oauth.rs`
- Modify: `plug-core/src/doctor.rs`

**Step 1: Add failing tests for storage-mode reporting**

Cover:
- keyring available and used
- keyring unavailable with file fallback
- file fallback explicitly in use

**Step 2: Run focused tests**

Run: `cargo test oauth_tokens -- --nocapture`

Expected: FAIL or incomplete coverage

**Step 3: Tighten reporting**

Replace ambiguous “may be unavailable” wording with deterministic storage-mode output and clearer remediation text.

**Step 4: Re-run focused tests**

Run: `cargo test oauth_tokens -- --nocapture`

Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/oauth.rs plug-core/src/doctor.rs
git commit -m "fix(doctor): report oauth token storage mode deterministically"
```

## Workstream C: Setup / Repair / Topology Visibility

### Task 8: Make setup ask for topology, not assume stdio

**Files:**
- Inspect/Modify: setup and client-config generation code paths
- Modify: any relevant config generation / export / client integration modules

**Step 1: Identify current setup and repair entry points**

Document the exact files and functions that currently flatten clients to `plug connect`.

**Step 2: Add failing tests for client setup choices**

Cover:
- stdio client via `plug connect`
- remote HTTP client
- daemon-backed local client

**Step 3: Implement interactive topology selection**

Setup should ask what the user wants instead of hard-coding the stdio bridge path.

**Step 4: Re-run tests**

Run the focused setup/repair test set.

**Step 5: Commit**

```bash
git add [setup/repair files]
git commit -m "feat(setup): support explicit client topology selection"
```

### Task 9: Make repair preserve client-specific transport choices

**Files:**
- Inspect/Modify: repair command implementation and client config writers

**Step 1: Add failing tests for mixed client configurations**

Cover a machine where some clients use stdio, some HTTP, and some are intentionally remote.

**Step 2: Run tests**

Run the focused repair test set.

Expected: FAIL due to current flattening behavior.

**Step 3: Implement topology-aware repair**

Repair should detect and preserve existing config intent rather than forcing all clients to `plug connect`.

**Step 4: Re-run tests**

Expected: PASS

**Step 5: Commit**

```bash
git add [repair files]
git commit -m "fix(repair): preserve client transport topology"
```

### Task 10: Surface transport and auth topology in status/UI

**Files:**
- Modify: `plug/src/views/overview.rs`
- Modify: `plug/src/views/servers.rs`
- Modify: `plug/src/ui.rs`

**Step 1: Add failing view tests/snapshots**

Cover:
- downstream transport mode
- per-server upstream transport type
- per-server auth mode
- client config type

**Step 2: Run focused tests**

Run view/status-focused tests.

**Step 3: Implement visibility**

Make status/menu views answer these operator questions directly:
- what is using stdio vs HTTP?
- which servers are stdio / HTTP / SSE upstreams?
- which auth mode applies where?
- what public URL / daemon / local endpoint is active?

**Step 4: Re-run focused tests**

Expected: PASS

**Step 5: Commit**

```bash
git add plug/src/views/overview.rs plug/src/views/servers.rs plug/src/ui.rs
git commit -m "feat(status): surface transport and auth topology explicitly"
```

## Final Verification

### Task 11: Add end-to-end scenario coverage

**Files:**
- Modify: `plug-core/tests/integration_tests.rs`

**Step 1: Add scenario matrix tests**

Add coverage for:
- downstream OAuth protected discovery
- token file exists but server remains `AuthRequired`
- raw TCP unreachable while daemon still has healthy routed state
- mixed upstream auth fleet
- mixed client topology repair/setup preservation

**Step 2: Run the full suite**

Run: `cargo test`

Expected: PASS

**Step 3: Release verification**

Run:

```bash
cargo build --release
plug status
plug doctor
```

Verify that status, auth status, and doctor tell one coherent story for at least one healthy OAuth
server, one `AuthRequired` server, and one failed server.

**Step 4: Commit**

```bash
git add plug-core/tests/integration_tests.rs
git commit -m "test(auth): cover mixed auth and topology scenarios end to end"
```

## Recommended Order

1. downstream OAuth discovery privacy
2. metadata auth-method correctness
3. 401 challenge hardening
4. doctor reachability/runtime separation
5. auth-state recovery UX
6. token storage reporting
7. setup/repair topology awareness
8. status/menu transport visibility
9. end-to-end scenario matrix
