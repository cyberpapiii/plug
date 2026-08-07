---
title: Keep Shipping While Modern Clients Gated - Plan
type: feat
date: 2026-08-07
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-plan-bootstrap
execution: code
---

# Keep Shipping While Modern Clients Gated - Plan

## Goal Capsule

- **Objective:** Keep shipping Plug value for today's legacy clients while modern MCP `2026-07-28` stays opt-in and default-off; close the deferred PR #60 polish residuals and make official modern conformance evidence honest via a suite-aligned fixture path.
- **Authority:** `docs/PROJECT-STATE-SNAPSHOT.md` and current `main` win over historical plans. Modern gates (`http.modern_downstream_enabled`, `modern_upstream_enabled`) remain default `false`.
- **Execution profile:** Small, independently mergeable units on the legacy path plus gated modern evidence prep. Prefer characterization/regression proof over new product surface.
- **Stop conditions:** Do not flip modern defaults. Do not claim official modern certification. Do not expand into fully live runtime reconfiguration or full cross-transport daemon persistence in this tranche.
- **Tail ownership:** Implementation, tests, operator-doc note, and inventory/conformance honesty.

---

## Product Contract

### Summary

Operators and agents keep getting reliability and observability improvements on the production legacy path. Modern readiness work lands only as optional evidence tooling and gated fixtures so Plug can flip modern later without a scramble.

### Problem Frame

Roadmap blockers on `main` are closed. Remaining value is optional polish and modern prep. Official modern npm suite (`0.2.0-alpha.10`) fails empty-multiplexer catalogs because it expects fixed `test_*` fixtures. PR #60 deferred metrics e2e + RAII recording and an operator note on `degraded_since` vs health/availability. Reload/SSE still has small duplication. Clients are not ready for modern-by-default.

### Requirements

- R1. Modern protocol gates remain default-off; no hard cutover.
- R2. Add a suite-aligned fixture upstream (or harness mode) that can satisfy official modern `test_*` catalog rows when an operator opts into official-modern evidence.
- R3. Record official modern evidence honestly: pass rows, expected-fail rows, and inventory vocabulary (`proven-local` / `unavailable-external` / new explicit expected-fail or observed baseline) without implying stable certification.
- R4. Close PR #60 metrics residual: RAII-style call recording where manual `record_call` arms miss drop/error paths, plus an end-to-end proof that live tool calls update status JSON metrics.
- R5. Add an operator-facing note clarifying `ServerHealth` vs `Availability` vs `degraded_since` (tool-call degradation clock).
- R6. Remove one clear reload/SSE helper duplication (`collect_sse_events` or equivalent) without behavior change.
- R7. Every unit must help today's legacy clients or improve gated modern evidence honesty; no new advertised modern capabilities.

### Scope Boundaries

**In scope**

- Fixture upstream / mock-server mode for official modern suite content rows
- Conformance docs + inventory updates for honest modern evidence
- Metrics RAII + e2e status JSON proof
- Operator note (CONCEPTS and/or operator guide)
- Small SSE helper dedupe

**Out of scope**

- Flipping modern gates default-on
- `subscriptions/listen`, mixed-era MRTR, task+MRTR advertisement
- Fully live runtime reconfiguration
- Full HTTP/SSE session survival across daemon death
- Stable official-suite certification claims while suite remains alpha

### Actors and Flows

- Operator runs optional `official-modern-server` against disposable Plug with modern gate on + fixture upstream attached.
- Operator reads status JSON / docs and correctly interprets health vs availability vs metrics clocks.
- Agent/implementer lands units without changing default wire era.

### Acceptance Examples

- With fixture upstream and modern gate on, official modern list/call/read/get rows that previously failed for missing `test_*` names pass or are explicitly baselined as expected-fail with reason.
- After a successful mock tool call through the real status path, `plug status --output json` (or equivalent harness) shows non-zero call metrics for that upstream.
- Docs state clearly that `degraded_since` is tool-call/circuit-driven and is not the same clock as catalog `Availability::Degraded`.
- Default config still has modern gates off; workspace tests remain green without requiring network npm suite.

---

## Planning Contract

### Key Technical Decisions

- **KTD1 (session-settled):** Modern gates stay default-off until real production clients speak 2026. Provenance: user-directed / user-approved. Rejected: flip defaults now. Reason: installed clients still negotiate `2025-11-25`.
- **KTD2 (session-settled):** No hard cutover to modern-only. Provenance: user-approved. Rejected: remove legacy path. Reason: breaks current clients.
- **KTD3 (session-settled):** This tranche excludes fully live reconfiguration and full cross-transport daemon persistence. Provenance: user-approved scope cut. Rejected: do entire optional backlog now. Reason: size/risk; keep shipping small.
- **KTD4:** Prefer extending `plug-test-harness` mock-server (or a dedicated fixture binary) with a `--official-modern-fixture` (name TBD) mode that advertises the suite's `test_*` tools/resources/prompts, rather than teaching Plug to invent fake catalog entries without an upstream. Reason: multiplexer truth stays upstream-backed.
- **KTD5:** Official modern suite remains operator-opt-in via `scripts/check-mcp-conformance.sh`; never a default CI gate while package is alpha.
- **KTD6:** Metrics RAII should mirror the existing `ActiveCallGuard` pattern: record latency/error on drop unless disarmed after successful `record_call`, so cancel/error paths cannot skip metrics.

### Assumptions

- Official suite fixture names remain those observed in the 2026-08-07 audit (`test_simple_text`, `test://static-text`, etc.); implementer must re-read the suite's current fixture list before coding.
- `collect_sse_events` duplication between `http/sse.rs` and `http/server.rs` is safe to collapse behind one helper without changing event semantics.
- Metrics e2e can run in-process (engine + router + status serialization) without requiring a live daemon socket if daemon path is heavier; prefer the shallowest path that still exercises `record_call` → status JSON.

### Technical Design

1. **Fixture mode:** Add fixture catalog to mock MCP server; document how to attach it under a disposable Plug config for official-modern runs.
2. **Evidence honesty:** Update `docs/testing/MCP-CONFORMANCE.md` and inventory rows after a recorded opt-in run; keep alpha caveat.
3. **Metrics RAII:** Introduce a small guard around upstream call accounting in `server`/`proxy` record sites; add e2e/integration assertion on status JSON.
4. **Docs:** Short CONCEPTS or operator-doc clarification for the three clocks.
5. **SSE dedupe:** One helper, both call sites, behavior-preserving tests green.

### Sequencing

U1 fixture → U2 conformance docs/inventory (needs U1 for real run) → U3 metrics RAII+e2e → U4 operator note → U5 SSE dedupe. U4/U5 can parallelize with U3 after U1 starts.

### Patterns to Follow

- `scripts/check-mcp-conformance.sh` + `docs/testing/MCP-CONFORMANCE.md` status vocabulary
- `plug-test-harness/src/bin/mock-server.rs` lifecycle flags
- `ActiveCallGuard` in `plug-core/src/proxy/mod.rs` for RAII shape
- `UpstreamMetrics` / `record_call` in `plug-core/src/server/mod.rs`
- `Availability` / `ServerHealth` in `plug-core/src/types.rs` and `CONCEPTS.md`

### Risks

- Suite fixture names drift between alpha builds — pin package version already in script; re-verify fixture list.
- Over-scoping continuity/live-reload — explicitly out of scope.
- Metrics RAII double-count if both manual and drop record — disarm pattern required.

---

## Implementation Units

### U1. Official-modern fixture upstream mode

**Goal:** Provide an upstream catalog that satisfies official modern suite `test_*` content rows when opted in.

**Files:** `plug-test-harness/src/bin/mock-server.rs`; possibly `docs/testing/MCP-CONFORMANCE.md`; optional tiny helper script under `scripts/` for disposable serve+fixture wiring (only if it reduces operator error).

**Approach:** Add a fixture mode that registers the suite's required tools/resources/prompts/templates with stable names/URIs/content. Keep default mock behavior unchanged.

**Test scenarios:**

- Fixture mode lists expected tool/resource/prompt names.
- Default mock mode does not expose fixture-only names.
- One content row (tool call or resource read) returns suite-expected shape.

**Verification:** unit/integration tests in harness or plug-core as appropriate; no default CI dependency on npm suite.

### U2. Honest official-modern evidence baseline

**Goal:** Document and inventory official modern results with fixture attached; no silent certification.

**Files:** `docs/testing/MCP-CONFORMANCE.md`; `docs/testing/mcp-compatibility-inventory.tsv`; optionally refresh `docs/audit/2026-08-07-mcp-2026-07-28-production-readiness-audit.md` or add a short follow-up note.

**Approach:** After U1, run operator-opt-in official-modern against disposable endpoint; update rows from `unavailable-external` / known empty-catalog fails to accurate statuses. Keep alpha caveat and gates-default-off posture.

**Test scenarios:**

- `scripts/check-mcp-conformance.sh inventory` still passes schema/status vocab.
- `self-test` / `local` unchanged green.

**Verification:** conformance script inventory + self-test.

### U3. Metrics RAII + status JSON e2e

**Goal:** Close PR #60 deferred metrics residual.

**Files:** `plug-core/src/server/mod.rs`; `plug-core/src/proxy/mod.rs` (call sites); tests under `plug-core` and/or `plug/src/views/overview.rs` patterns; new or extended integration test.

**Approach:** RAII guard records call/error/latency unless disarmed after explicit success recording. Add e2e that performs a real tool call and asserts metrics fields in status JSON for that server id.

**Test scenarios:**

- Successful call increments call count and updates last latency.
- Error/cancel path still records (no silent skip).
- No double-count when success path disarms guard.
- Status JSON schema remains stable for idle servers (zero-filled).

**Verification:** `cargo test` selectors covering metrics + status JSON.

### U4. Operator note: health vs availability vs degraded_since

**Goal:** Remove operator ambiguity called out in CONCEPTS/PLAN residuals.

**Files:** `CONCEPTS.md`; optionally a short subsection in an existing operator-facing doc if one already covers status JSON (do not invent a large new guide).

**Approach:** Clarify three clocks/signals with one concrete example each. No schema renames.

**Test scenarios:** docs-only — review for accuracy against `types.rs` field meanings; no runtime test required.

**Verification:** manual consistency check vs `ServerHealth`, `Availability`, `degraded_since` code comments/fields.

### U5. SSE helper dedupe (behavior-preserving)

**Goal:** Remove `collect_sse_events` duplication between HTTP SSE modules.

**Files:** `plug-core/src/http/sse.rs`; `plug-core/src/http/server.rs`; existing SSE tests.

**Approach:** Keep one shared helper; delete or thin the duplicate. No wire/format change.

**Test scenarios:** existing SSE/notification tests remain green; if a thin unit test exists for collection, keep it pointed at the shared helper.

**Verification:** existing HTTP/SSE tests.

---

## Verification Contract

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --lib`
- Focused: metrics/status selectors; mock-server fixture tests; `scripts/check-mcp-conformance.sh inventory` and `self-test` (and `local` if inventory selectors change)
- Official modern npm suite remains opt-in evidence only

## Definition of Done

- All units merged or explicitly deferred with reason in residual record
- Modern gates still default-off on `main`
- No claim of stable official modern certification
- PR #60 metrics + operator-note residuals addressed
- Workspace gates green

## Work Relationships

- **Owns:** keep-shipping polish + modern evidence honesty while gates off
- **Separately planned later:** flip modern defaults after real-client proof; `subscriptions/listen` / mixed-era MRTR / task+MRTR; fully live reconfiguration; full cross-transport daemon continuity
