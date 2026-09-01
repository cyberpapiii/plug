---
title: Plug Hardening Execution Pass
type: refactor
status: active
date: 2026-05-17
origin: docs/audit-2026-05-17.md
---

# Plug Hardening Execution Pass

## Summary

Execute the completed MCP/Rust/security audit against Plug with an open-source launch bar: clean dependency risk, close multiplexor correctness gaps, align the public protocol surface where current SEPs are settled, fix distribution paths, and refresh external-user documentation. The work should proceed as a sequence of reviewable commits/PR-sized tranches with phase gates recorded in `docs/hardening-log.md` and completion status reflected back into `docs/audit-2026-05-17.md`.

---

## Problem Frame

`docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` accurately describe the earlier roadmap as complete on `main`, but the May 2026 audit raised a higher launch bar. Plug is now production-critical daily infrastructure for multiple agent clients and is moving from operationally solo to external open-source readiness, so audit deferrals based only on lack of users no longer hold.

---

## Assumptions

*This plan was authored in LFG pipeline mode from the user's explicit hardening prompt. It records implementation assumptions so downstream review can challenge them without blocking execution on another planning round.*

- Phase 1 low-risk work already landed before the LFG correction in commit `222646d`; this plan treats it as the first completed tranche and keeps subsequent work sequenced from Phase 2 onward.
- The current branch is `main`; commits may be kept as a commit series if PRs are not being used for this local execution pass.
- The distribution namespace decision remains a stop-and-ask point exactly as requested: when implementation reaches Phase 5 item 15, work pauses for the owner choice between creating/migrating to `plug-mcp` or rewriting docs/package metadata to the actual repo location.
- Live smoke coverage against every advertised GUI client may require manual/operator participation. Automated smoke should still verify the daemon and at least two active clients when available, and any untestable client path must be recorded in `docs/hardening-log.md`.

---

## Requirements

- R1. Remove or document all direct-dependency RustSec risk, with `cargo deny check advisories` clean except for explicitly accepted, recorded residuals.
- R2. Keep existing client integrations wire-compatible unless a breaking choice is explicitly owner-approved or guarded behind a compatibility flag.
- R3. Replace stale protocol workarounds that are no longer load-bearing while preserving multiplexor-owned protocol control surfaces.
- R4. Bring core Rust dependencies and `rmcp` forward within stable, non-RC ranges without adopting `rmcp`'s Streamable HTTP server stack in this pass.
- R5. Add or strengthen tests for capability synthesis, reverse-request routing, session handling, transport masking, and OAuth behavior across both upstream and downstream planes.
- R6. Implement launch-relevant multiplexor correctness features previously deferred only because Plug appeared operationally solo.
- R7. Align public HTTP/auth/discovery surfaces with Final or Accepted MCP SEPs where alignment matters for gateways, while avoiding draft-only public extensions.
- R8. Make distribution instructions and release artifacts truthful, reproducible, and non-conflicting for external users.
- R9. Refresh external-user docs for installation, client setup, operations, security reporting, and contribution workflow.
- R10. Keep `docs/audit-2026-05-17.md` and `docs/hardening-log.md` as the durable accountability trail for shipped, deferred, or re-scoped audit items.

---

## Scope Boundaries

- Do not adopt unstable/RC dependencies.
- Do not adopt `rmcp`'s Streamable HTTP server stack; transport redesign remains separate future work.
- Do not invent public Plug-specific protocol extensions where a Final or Accepted SEP exists.
- Do not remove multiplexor-owned surfaces such as lazy bridge, capability synthesis, task ownership, artifact spillover, or IPC protocol without a separate architectural decision.
- Do not treat stale plans, branch summaries, or worktree state as current truth; verify materially important claims against `main`.
- Do not silently defer audit items. Every deferral needs a reason, unblocking condition, owner, and re-review date in `docs/audit-2026-05-17.md`.

---

## Context & Research

### Relevant Code and Patterns

- `plug-core/src/http/server.rs` owns downstream Streamable HTTP, SSE fanout, session handling, protocol-version validation, and several HTTP operator/discovery endpoints.
- `plug-core/src/proxy/mod.rs` owns capability synthesis, request routing, reverse-request handling, task wrapping, transport masking, and tool metadata enrichment.
- `plug-core/src/server/mod.rs` owns upstream lifecycle and transport selection.
- `plug/src/ipc_proxy.rs` and `plug/src/daemon.rs` own daemon IPC, stdio client adaptation, capability masking, and reverse-request forwarding.
- `plug-core/src/oauth.rs` and `plug-core/src/downstream_oauth/mod.rs` own upstream and downstream OAuth behavior.
- `plug-core/src/transport/sse_client.rs` owns legacy SSE upstream support.
- `plug-core/src/export.rs`, `plug-core/src/import.rs`, `plug-core/src/doctor.rs`, and `plug/src/commands/clients.rs` are the known YAML call sites for replacing `serde_yml`.
- `plug-test-harness` and `plug-core/tests/integration_tests.rs` are the main cross-transport regression surfaces.

### Institutional Learnings

- `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` are current-state claims, but `main` remains the only source of done-now truth.
- `docs/audit-2026-05-17.md` is the execution source for gap rows, Section 7 `rmcp` sequencing, Section 8 protocol-version cleanup, and Section 9 public-surface re-weighting that this plan supersedes.
- `docs/plans/2026-04-23-001-feat-lazy-tool-discovery-v2-plan.md` and the lazy discovery follow-up plan show the preferred style for preserving multiplexor-specific control while improving interoperability.

### External References

- MCP spec and SEP citations are already collected in `docs/audit-2026-05-17.md`; implementation should refresh only where the audit explicitly says a status may be draft or ambiguous.
- `rmcp` release notes and docs are summarized in Section 7 of `docs/audit-2026-05-17.md`; implementation should follow that concrete diff rather than re-spiking the whole SDK.

---

## Key Technical Decisions

- Treat the audit as executable scope, not advisory backlog: the launch bar re-weights items deferred solely because there were no external users.
- Preserve compatibility first: where a spec-alignment improvement changes client-visible wire behavior, prefer accepting both old and new forms during a transition unless the owner approves a break.
- Keep transport redesign out of the `rmcp` bump: upgrade types and local protocol glue first, then consider server-stack adoption in a separate future plan.
- Use characterization tests before deleting or replacing compatibility code, especially YAML serialization, protocol-version handling, IPC routing, SSE session replay, and auth metadata.
- Keep every phase gate explicit: full workspace tests, `cargo deny`, clippy, smoke coverage, audit status, and hardening log updates.

---

## Open Questions

### Resolved During Planning

- Is Plug still a solo tool for launch decisions? No. This plan uses the user's corrected framing: production-critical, daily-use, pre-launch public infrastructure.
- Should optional items remain optional solely because external users do not exist yet? No. Items deferred only for that reason move into launch scope.
- Should draft SEP features be built now? No. Track draft DPoP and workload identity work, but do not create public extensions before the spec settles.

### Deferred to Implementation

- Which maintained YAML crate preserves Plug-authored config behavior best? Inventory and tests decide between `serde_norway` and `serde_yaml_ng`; stop only if the result remains genuinely ambiguous.
- Which exact `rmcp` call sites need refactoring after the bump? Section 7 gives the expected list, but the compiler and tests decide the final edit set.
- Which GUI client smoke paths can be exercised non-interactively? Record gaps honestly in `docs/hardening-log.md`.
- Which release namespace should Plug use? Stop at Phase 5 item 15 for owner input.

---

## Implementation Units

### U1. Phase 1 Reconciliation

**Goal:** Preserve the already-landed low-risk tranche as the first hardening commit and ensure the audit/log record reflects it.

**Requirements:** R1, R3, R10

**Dependencies:** None

**Files:**
- Modify: `docs/audit-2026-05-17.md`
- Modify: `docs/hardening-log.md`
- Test: `plug-core/src/http/server.rs`
- Test: `plug-core/tests/integration_tests.rs`

**Approach:**
- Keep commit `222646d` as the Phase 1 record.
- Treat the protocol-version rewrite cleanup, stable patch bumps, and cleared `instant`/`rustls-pemfile` advisories as done.
- Carry `serde_yml` forward as the remaining direct advisory into U2.

**Patterns to follow:**
- Section 8 of `docs/audit-2026-05-17.md`
- Existing HTTP initialize tests in `plug-core/src/http/server.rs`

**Test scenarios:**
- Regression: initialize response still advertises the intended protocol version without body rewriting.
- Integration: full workspace tests pass after dependency patch bumps.
- Advisory: `cargo deny check advisories` reports only explicitly planned remaining issues.

**Verification:**
- Phase 1 status and residuals are recorded in `docs/hardening-log.md` and `docs/audit-2026-05-17.md`.

### U2. Replace `serde_yml`

**Goal:** Remove the direct `serde_yml` RustSec advisory with the maintained YAML implementation closest to Plug's current config/import/export behavior.

**Requirements:** R1, R4, R8

**Dependencies:** U1

**Files:**
- Modify: `Cargo.toml`
- Modify: `plug/Cargo.toml`
- Modify: `plug-core/Cargo.toml`
- Modify: `plug/src/commands/clients.rs`
- Modify: `plug-core/src/doctor.rs`
- Modify: `plug-core/src/export.rs`
- Modify: `plug-core/src/import.rs`
- Test: `plug/src/commands/clients.rs`
- Test: `plug-core/src/export.rs`
- Test: `plug-core/src/import.rs`
- Test: `plug-core/src/doctor.rs`

**Approach:**
- Inventory every `serde_yml` use before editing.
- Compare `serde_norway` and `serde_yaml_ng` against current Plug-authored YAML cases.
- Prefer `serde_norway` if behavior is equivalent because the RustSec advisory points to it as a maintained successor.
- Add round-trip/merge tests for client config and Goose import/export shapes that users may author.

**Execution note:** Characterization-first. Tests should lock current YAML behavior before broad call-site replacement.

**Patterns to follow:**
- Existing YAML tests in `plug/src/commands/clients.rs`
- Goose import/export code paths in `plug-core/src/export.rs` and `plug-core/src/import.rs`

**Test scenarios:**
- Happy path: linked-client YAML snippets merge without losing unrelated keys.
- Edge case: invalid YAML continues to surface a user-facing validation error.
- Integration: Goose YAML export remains parseable by the selected crate and import remains compatible with existing extension lists.
- Advisory: `cargo deny check advisories` no longer reports `serde_yml`.

**Verification:**
- `serde_yml` no longer appears in manifests, code, or `cargo deny check advisories`.

### U3. Upgrade `rmcp` to the Current Stable Target

**Goal:** Upgrade from `rmcp 1.5.0` to the concrete target identified by the audit, preserving Plug's multiplexor-owned protocol surfaces.

**Requirements:** R2, R4, R5

**Dependencies:** U2

**Files:**
- Modify: `Cargo.toml`
- Modify: `plug-core/src/proxy/mod.rs`
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/server/mod.rs`
- Modify: `plug-core/src/oauth.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Modify: `plug-core/src/transport/sse_client.rs`
- Modify: `plug/src/ipc_proxy.rs`
- Modify: `plug/src/daemon.rs`
- Modify: `plug-test-harness/**`
- Test: `plug-core/tests/integration_tests.rs`
- Test: `plug-test-harness/**`

**Approach:**
- Follow the Section 7 five-step recommendation exactly.
- Update SDK types and compiler-driven call sites without adopting `rmcp`'s Streamable HTTP server.
- Add parity tests listed in Section 7 for protocol negotiation, reverse requests, session handling, OAuth, and task behavior.
- Keep local routing/capability synthesis code when it carries multiplexor-specific policy the SDK cannot own.

**Test scenarios:**
- Integration: initialize/capability negotiation remains stable for stdio, HTTP, and IPC clients.
- Integration: sampling and elicitation reverse requests still route to the owning downstream session.
- Integration: upstream OAuth refresh and downstream OAuth challenge/discovery behavior remain compatible.
- Error path: unsupported protocol versions continue to fail with the expected downstream error.

**Verification:**
- Workspace tests, clippy, and smoke tests pass on the upgraded SDK without server-stack adoption.

### U4. SSE Resumability

**Goal:** Add bounded downstream SSE replay keyed by `Last-Event-ID` for gateway notifications that matter to reconnecting clients.

**Requirements:** R2, R5, R6, R7

**Dependencies:** U3

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/http/sse.rs` if present
- Modify: `plug-core/src/session/**`
- Modify: `plug-core/src/proxy/mod.rs`
- Test: `plug-core/src/http/server.rs`
- Test: `plug-core/tests/integration_tests.rs`

**Approach:**
- Implement a bounded per-session event buffer for list-changed, progress, resource update, and reverse-request notifications.
- Use `Last-Event-ID` to replay only events newer than the client cursor.
- Document stateful replay assumptions and where accepted/final stateless SEP work would affect a future redesign.

**Test scenarios:**
- Happy path: reconnect with a known event id replays missed notifications in order.
- Edge case: reconnect with an expired id resumes without unbounded replay and surfaces the documented behavior.
- Integration: reverse-request notification delivery is replay-safe without double-routing completed requests.
- Error path: unknown session or malformed `Last-Event-ID` does not panic or leak another session's events.

**Verification:**
- Reconnect behavior is proven with HTTP/SSE tests and no unbounded memory growth path is introduced.

### U5. Resource Subscribe Parity Over Daemon IPC

**Goal:** Bring daemon IPC resource subscribe/unsubscribe behavior to parity with stdio and HTTP, including targeted `ResourceUpdated` delivery.

**Requirements:** R2, R5, R6

**Dependencies:** U3

**Files:**
- Modify: `plug/src/ipc_proxy.rs`
- Modify: `plug/src/daemon.rs`
- Modify: `plug-core/src/proxy/mod.rs`
- Modify: `plug-core/src/server/mod.rs`
- Test: `plug-core/tests/integration_tests.rs`
- Test: `plug/src/ipc_proxy.rs`
- Test: `plug/src/daemon.rs`

**Approach:**
- Extend daemon IPC protocol messages for subscribe/unsubscribe and resource update push delivery.
- Keep capability masking honest during transition.
- Ensure subscriptions rebind or prune correctly when routes refresh.

**Test scenarios:**
- Happy path: IPC downstream subscription receives only relevant `ResourceUpdated` notifications.
- Edge case: unsubscribed or disconnected IPC client stops receiving resource updates.
- Integration: capability synthesis advertises subscribe support only when IPC parity is available.
- Error path: upstream subscribe failure propagates to the requesting client without corrupting local subscription state.

**Verification:**
- IPC resource subscribe parity is represented in tests and current-state docs.

### U6. Operator Trust and Risk Inventory

**Goal:** Expose machine-readable source/trust metadata and distinguish upstream-declared risk from Plug-inferred risk.

**Requirements:** R5, R6, R9, R10

**Dependencies:** U3

**Files:**
- Modify: `plug-core/src/proxy/mod.rs`
- Modify: `plug-core/src/enrichment.rs` if present
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug/src/commands/tools.rs`
- Modify: `plug/src/commands/servers.rs`
- Modify: `plug/src/views/**`
- Test: `plug-core/tests/integration_tests.rs`
- Test: `plug/src/commands/**`

**Approach:**
- Add structured source/trust metadata per server/tool to operator JSON surfaces.
- Preserve upstream annotation fields as declared and annotate Plug-inferred values separately.
- Avoid presenting inferred destructive/open-world risk as upstream truth.

**Test scenarios:**
- Happy path: operator JSON includes source and trust metadata for routed tools.
- Edge case: missing upstream annotations produce conservative Plug inference labeled as inference.
- Integration: CLI views render the same distinction without breaking pinned JSON contracts unexpectedly.
- Error path: malformed upstream metadata is ignored or quarantined without breaking tool listing.

**Verification:**
- Pinned operator JSON tests cover the new fields and compatibility story.

### U7. Trace Correlation and SEP-2243 Headers

**Goal:** Carry OpenTelemetry-compatible trace/span IDs across the proxy boundary and align HTTP method/name observability headers with SEP-2243.

**Requirements:** R5, R6, R7

**Dependencies:** U3

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/proxy/mod.rs`
- Modify: `plug-core/src/server/mod.rs`
- Modify: `plug-core/src/oauth.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Test: `plug-core/tests/integration_tests.rs`
- Test: `plug-core/src/http/server.rs`

**Approach:**
- Add trace context propagation through downstream request, router, upstream call, retry/reconnect, and auth refresh spans.
- Emit and validate `Mcp-Method` and `Mcp-Name` where applicable, accepting absent headers for backward compatibility.
- Use structured tracing fields rather than ad hoc log strings.

**Test scenarios:**
- Happy path: downstream HTTP request produces correlated logs/spans through upstream routing.
- Edge case: missing or unknown header values do not reject legacy clients unless the spec requires it for that path.
- Integration: OAuth refresh/reconnect work retains the request/server correlation fields.
- Error path: upstream failure logs include method/name/server context without leaking credentials.

**Verification:**
- Trace fields and SEP-2243 header handling are covered by tests and operator docs.

### U8. Server Card and Auth SEP Alignment

**Goal:** Align public discovery/auth surfaces with settled MCP guidance without implementing draft-only DPoP or workload identity extensions.

**Requirements:** R2, R7, R9

**Dependencies:** U3, U7

**Files:**
- Modify: `plug-core/src/http/server.rs`
- Modify: `plug-core/src/downstream_oauth/mod.rs`
- Modify: `plug-core/src/oauth.rs`
- Modify: `plug/src/config/**`
- Test: `plug-core/src/http/server.rs`
- Test: `plug-core/tests/integration_tests.rs`

**Approach:**
- Align `/.well-known/mcp.json` with the Server Card WG draft and SEP-2127 without adding Plug-only public fields.
- Implement straightforward support for SEP-985 protected resource metadata, SEP-991 URL client metadata documents, SEP-1046 machine-to-machine client credentials, and SEP-2207 refresh-token guidance.
- Track SEP-1932 and SEP-1933 in docs only while draft.

**Test scenarios:**
- Happy path: server card and OAuth metadata endpoints return spec-aligned fields for configured HTTP/HTTPS service.
- Edge case: loopback/non-loopback auth behavior stays privacy-preserving and backward-compatible.
- Integration: URL client metadata and M2M credentials work with downstream OAuth discovery.
- Error path: invalid client metadata URL or token request produces explicit OAuth errors.

**Verification:**
- Auth and discovery conformance tests pass without custom public protocol additions.

### U9. Opt-In Stdio Upstream Sandboxing

**Goal:** Add an opt-in sandbox mode for third-party stdio upstreams without changing default behavior.

**Requirements:** R2, R6, R9

**Dependencies:** U3

**Files:**
- Modify: `plug-core/src/config/**`
- Modify: `plug-core/src/server/mod.rs`
- Modify: `plug-core/src/transport/**`
- Create: `plug-core/src/sandbox.rs` if a dedicated module is warranted
- Test: `plug-core/tests/integration_tests.rs`
- Test: `plug-core/src/config/**`

**Approach:**
- Add config for filesystem allowlist, network allowlist, and process limits where the host platform can enforce them reliably.
- Keep sandbox disabled by default.
- Fail closed when sandbox config is invalid, and document platform-specific limitations.

**Test scenarios:**
- Happy path: sandboxed stdio upstream starts with allowed command/filesystem/network settings.
- Edge case: unsupported platform or unsupported sandbox knob reports a clear diagnostic.
- Error path: disallowed path or network setting prevents launch before spawning the upstream.
- Integration: unsandboxed existing stdio configs behave unchanged.

**Verification:**
- Sandbox mode is covered by config and launch tests and documented as opt-in.

### U10. Distribution Surface

**Goal:** Make public install and release paths truthful and reproducible.

**Requirements:** R8, R9, R10

**Dependencies:** U2, U3

**Files:**
- Modify: `README.md`
- Modify: `dist-workspace.toml`
- Modify: `Cargo.toml`
- Modify: `plug/Cargo.toml`
- Modify: release workflow files under `.github/**`
- Modify: installer scripts if present
- Test: release/install smoke scripts if present

**Approach:**
- Stop for the owner decision on repository namespace before editing public URLs.
- Resolve the crates.io `plug` name conflict by choosing a non-conflicting package/install path or documenting an alternate distribution path.
- Fix or remove `get.plug.sh` references depending on endpoint viability.
- Verify Homebrew tap generation and cargo-dist release artifacts install and run on advertised platforms.

**Test scenarios:**
- Happy path: documented cargo/Homebrew/shell install path installs the intended Plug binary.
- Edge case: existing users with old local binary names receive clear migration instructions.
- Integration: cargo-dist test release produces artifacts matching README claims.
- Error path: failed installer endpoint is removed or clearly documented rather than advertised as working.

**Verification:**
- README install commands and release metadata match a tested distribution path.

### U11. External User Documentation

**Goal:** Bring user/operator/contributor docs up to the open-source launch bar.

**Requirements:** R8, R9, R10

**Dependencies:** U4, U5, U6, U7, U8, U9, U10

**Files:**
- Modify: `README.md`
- Modify: `docs/MCP-SPEC.md`
- Create or modify: `docs/USERS.md`
- Create: `docs/operator-guide.md`
- Create: `SECURITY.md`
- Create: `CONTRIBUTING.md`
- Modify: `docs/PROJECT-STATE-SNAPSHOT.md`
- Modify: `docs/PLAN.md`

**Approach:**
- Verify every install command, client setup section, and advertised capability against current `main`.
- Reconcile `docs/MCP-SPEC.md` with the protocol-version stance and SEP-aligned surfaces.
- Add operator guidance for TLS, OAuth on both planes, observability, sandboxing, and production diagnostics.
- Add security disclosure and contributor expectations before public launch.

**Test scenarios:**
- Documentation: every command block that can be run locally is smoke-tested or explicitly marked as illustrative.
- Documentation: client setup instructions map to actual supported clients in `README.md` and `docs/USERS.md`.
- Documentation: security and contribution docs contain concrete reporting/testing expectations.

**Verification:**
- Current-state docs no longer imply the older roadmap bar is the full public-launch bar.

### U12. Phase Gates, Review, and Release Accountability

**Goal:** Keep each tranche shippable and reviewable with tests, audit status, and hardening log updates.

**Requirements:** R1, R2, R5, R10

**Dependencies:** Every implementation unit

**Files:**
- Modify: `docs/audit-2026-05-17.md`
- Modify: `docs/hardening-log.md`
- Test: workspace-wide tests

**Approach:**
- After each phase, run `cargo test --workspace -- --test-threads=1`, `cargo deny check advisories`, `cargo clippy --workspace -- -D warnings`, and available manual smoke checks.
- Record shipped work, tests added, deferrals, surprises, and any new bugs/design issues in `docs/hardening-log.md`.
- Mark relevant audit rows Done, Re-scoped, or Deferred-with-reason.

**Test scenarios:**
- Gate: workspace tests pass before moving to the next phase.
- Gate: advisory checks are clean or accepted residuals are documented.
- Gate: clippy passes with warnings denied.
- Gate: at least two real active clients are smoke-tested when available; inaccessible client paths are recorded.

**Verification:**
- No phase transition occurs without a hardening-log entry and audit status update.

---

## System-Wide Impact

- **Interaction graph:** Downstream stdio, downstream HTTP/SSE, daemon IPC, upstream stdio/HTTP/SSE, OAuth refresh workers, operator CLI/API surfaces, release tooling, and docs all participate in this hardening pass.
- **Error propagation:** Protocol/auth/session errors must remain client-visible and transport-appropriate; operator-only diagnostics should not leak credentials or internal tokens.
- **State lifecycle risks:** SSE replay buffers, IPC subscriptions, OAuth refresh state, trace IDs, sandbox lifecycle, and artifact/session cleanup all need bounded ownership and cleanup.
- **API surface parity:** Features implemented for HTTP or stdio should be checked against daemon IPC before advertising full capability parity.
- **Integration coverage:** Unit tests are not enough for this pass; cross-transport integration tests and daemon/client smoke checks are required phase gates.
- **Unchanged invariants:** Existing lazy discovery, capability synthesis, task ownership, artifact spillover, and IPC protocol remain Plug-owned control surfaces unless a separate public API decision changes them.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| A dependency upgrade subtly changes MCP wire behavior | Characterization tests before the bump, compiler-driven edits, and cross-transport regression tests after the bump |
| SSE replay duplicates reverse requests or leaks events across sessions | Per-session buffers, event ownership checks, and reconnect tests for reverse-request notifications |
| IPC subscribe parity changes capability masks for existing clients | Test masking before and after IPC support; keep transitional behavior explicit |
| SEP interpretation affects public API | Stop when the SEP has multiple reasonable interpretations and the choice affects public API |
| Distribution namespace requires owner/account action | Stop at Phase 5 item 15 for the requested owner decision |
| Manual client smoke is not fully automatable | Record exact available clients, exercised paths, and untested client paths in `docs/hardening-log.md` |
| Large scope causes unrelated churn | Keep commits/PR-sized tranches aligned to implementation units and avoid broad refactors outside the audit |

---

## Documentation / Operational Notes

- `docs/hardening-log.md` is the running execution narrative and should be updated at every phase gate.
- `docs/audit-2026-05-17.md` remains the authoritative audit matrix and must reflect Done/Re-scoped/Deferred-with-reason for every touched gap row.
- `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` need a final truth pass so they represent the launch bar after hardening, not only the older roadmap completion state.

---

## Sources & References

- Origin document: `docs/audit-2026-05-17.md`
- Current truth docs: `docs/PROJECT-STATE-SNAPSHOT.md`, `docs/PLAN.md`, `docs/TRUTH-RULES.md`
- Workflow adapter: `AGENTS.md`
- Prior lazy discovery plan: `docs/plans/2026-04-23-001-feat-lazy-tool-discovery-v2-plan.md`
