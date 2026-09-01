---
title: fix: Resolve runtime saturation and remaining MCP naming inconsistencies
type: fix
status: completed
date: 2026-03-22
---

# fix: Resolve runtime saturation and remaining MCP naming inconsistencies

## Overview

`plug` still has three important classes of follow-up work after canonical display-title normalization:

1. a live daemon/runtime saturation issue that can cause tool inventories to disappear or become partial
2. a mixed naming model that is internally coherent but not globally uniform across all routed tools
3. stale historical naming docs and incomplete regression coverage for naming edge cases

This plan addresses those remaining issues without regressing the stable prefixed wire-name contract already shipped on `main`.

## Problem Statement / Motivation

The highest-impact remaining problem is operational: on this machine, the shared daemon can become unhealthy under accumulated `plug connect` sessions and start rejecting IPC connections with `max connections reached`, causing runtime inspection and tool inventory to fail. Separately, even with the display-title canonicalization fix, `plug` still exposes a mixed catalog shape (`Todoist__...`, `Slack__...`, `Gmail__...`, `GoogleDocs__...`) that may be acceptable by design but has never been explicitly resolved as a product decision.

The result is that users can still see:

- disappearing or partial tool surfaces when the daemon/runtime is saturated
- a catalog that is display-consistent for metadata-aware clients but still structurally mixed
- older docs and plan files that describe outdated naming behavior

## Proposed Solution

Address the remaining work in four bounded tracks:

1. **Runtime stabilization**
   Diagnose and fix the daemon/session leak or connection lifecycle issue causing repeated `IPC connection rejected: max connections reached`.

2. **Naming policy decision and implementation**
   Keep the current mixed server-prefix vs sub-service-prefix model and document it explicitly as current behavior.

3. **Regression coverage**
   Add targeted tests for collision-safe display naming, session/IPC saturation behavior where feasible, and chosen naming-policy guarantees.

4. **Documentation reconciliation**
   Update the remaining stale naming design/history docs or clearly mark them as historical so the current behavior is not obscured by outdated planning artifacts.

## Technical Considerations

- Preserve stable wire `name` semantics for existing client integrations unless the naming-policy phase explicitly decides otherwise.
- Treat runtime stabilization as the top priority because disappearing tools are a functional defect, not a cosmetic one.
- Keep display-title normalization intact while exploring broader naming-policy changes.
- Distinguish clearly between:
  - runtime/tool availability defects
  - display metadata behavior
  - structural naming-policy choices

## System-Wide Impact

- **Interaction graph**: downstream clients connect through `plug connect` and/or the shared daemon socket; runtime inspection, auth status, client inventory, and tool inventory all depend on healthy daemon/IPC session management. A failure in session lifecycle can surface as missing tools even when upstream servers are healthy.
- **Error propagation**: daemon IPC rejection cascades into `plug status`, `plug tools`, and client-visible inventory failures. This should be traced from session admission through runtime inspection requests and client disconnect cleanup.
- **State lifecycle risks**: leaked or unclosed socket/session state can prevent new runtime inspection requests and leave inventory snapshots stale.
- **API surface parity**: naming-policy changes affect tools, docs, tests, and potentially any client-compat guidance that references specific prefixes.
- **Integration test scenarios**:
  - many downstream `plug connect` sessions over time should not exhaust daemon IPC capacity
  - runtime inspection should remain available while clients are connected
  - naming snapshots should remain consistent after chosen policy changes

## Research Summary

### Local references

- Runtime failure messaging:
  - [overview.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/views/overview.rs)
  - [tools.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/views/tools.rs)
- Daemon IPC rejection logging:
  - [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)
- Tool-group routing and mixed prefix behavior:
  - [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
  - [tool_naming.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tool_naming.rs)
  - [config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs)
- Current runtime evidence on this machine:
  - `~/Library/Logs/plug/serve-stderr.log` contains repeated `IPC connection rejected: max connections reached`

### Current known truths

- Wire names are already consistently prefixed.
- Display-title conflicts from top-level `title` vs `annotations.title` have been addressed.
- The mixed prefix strategy is still unresolved as a product decision.
- Historical docs still contain outdated naming examples and “future work” statements for features that now exist.

## Implementation Units

### Unit 1: Diagnose and fix daemon/session lifecycle saturation

- Goal: eliminate the conditions that produce repeated `max connections reached` and make runtime inspection/tool inventory unavailable.
- Files:
  - `plug/src/daemon.rs`
  - `plug/src/runtime.rs`
  - `plug/src/ipc_proxy.rs`
  - any supporting session/IPC types under `plug-core/src/ipc.rs`
- Patterns to follow:
  - existing daemon admission and cleanup paths
  - current runtime availability and IPC status reporting
- Approach:
  - trace how downstream `plug connect` sessions are admitted, tracked, and released
  - determine whether stale sessions, missing cleanup, or overly strict connection caps are responsible
  - add logging and tests around connect/disconnect lifecycle if needed
  - fix cleanup and/or connection accounting so runtime inspection remains healthy under normal multi-client usage
- Verification:
  - repeated connection cycles do not trigger `max connections reached`
  - `plug status` and `plug tools --output json` remain available with active clients

### Unit 2: Make an explicit naming-policy decision

- Goal: decide whether the current mixed prefix model is intentional product behavior or technical debt to remove.
- Files:
  - `plug-core/src/proxy/mod.rs`
  - `plug-core/src/tool_naming.rs`
  - `plug-core/src/config/mod.rs`
  - `docs/CLIENT-COMPAT.md`
  - `README.md`
- Approach:
  - compare two options:
    - keep mixed model: server-prefixed tools plus workspace sub-service decomposition
    - unify model: one consistent prefixing strategy across all routed tools
  - record the decision in docs before implementing any broad naming changes
  - if unifying, stage changes behind a narrow compatibility plan and explicit test updates
- Verification:
  - chosen policy is documented
  - code, tests, and docs all align with the chosen policy

### Unit 3: Strengthen collision and naming regression coverage

- Goal: ensure remaining naming edge cases are covered by tests.
- Files:
  - `plug-core/src/proxy/mod.rs`
  - `plug-core/src/tool_naming.rs`
  - `plug-core/tests/integration_tests.rs`
- Approach:
  - add a real proxy test for collision-driven fallback where display title should match the final non-colliding name
  - add tests for known casing exceptions and any chosen naming-policy invariants
  - if runtime stabilization exposes session-accounting helpers, add focused lifecycle tests there as well
- Verification:
  - targeted `plug-core` tests cover collision, casing, and naming-policy behavior

### Unit 4: Reconcile stale naming docs and historical planning artifacts

- Goal: remove or clearly mark misleading documentation around old naming behavior.
- Files:
  - `docs/MCP-SPEC.md`
  - `docs/CLIENT-COMPAT.md`
  - `README.md`
  - `docs/plans/2026-03-05-tool-naming-design.md`
  - `docs/plans/2026-03-05-tool-naming-plan.md`
- Approach:
  - update current truth docs to describe the shipped naming model and client behavior accurately
  - mark older plan/design docs as historical when they no longer reflect current code
  - explicitly document what `plug` can and cannot control across clients
- Verification:
  - no current-facing doc describes outdated lowercase `fanout` naming or unimplemented `tool_groups` behavior as present/future truth

## Acceptance Criteria

- [ ] Repeated client/session churn no longer causes daemon IPC saturation in normal use
- [ ] `plug status` and `plug tools --output json` remain usable while clients are connected
- [ ] A naming-policy decision is documented and reflected in code or explicitly deferred with rationale
- [ ] Collision/casing/naming regression coverage is strengthened
- [ ] Remaining stale naming docs are reconciled or clearly marked historical

## Success Metrics

- Runtime inspection remains available during normal multi-client usage.
- Users do not observe disappearing tool inventories caused by daemon saturation.
- Repo documentation presents one coherent description of the naming model.
- Naming-related regressions become easier to catch in tests before release.

## Dependencies & Risks

- Runtime saturation may involve behavior outside the immediate naming code paths and may require deeper daemon/session investigation than currently visible from the CLI.
- A unified naming-policy change could be compatibility-sensitive for clients or scripts that rely on current wire names.
- Historical docs may be intentionally preserved for archaeology, so reconciliation should prefer explicit historical labeling over destructive removal.

## Sources & References

- Runtime status messaging:
  - [overview.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/views/overview.rs)
  - [tools.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/views/tools.rs)
- Daemon session saturation logging:
  - [daemon.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/daemon.rs)
  - `~/Library/Logs/plug/serve-stderr.log`
- Current naming implementation:
  - [mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
  - [tool_naming.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/tool_naming.rs)
  - [config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs)
- Current naming/truth docs:
  - [README.md](/Users/robdezendorf/Documents/GitHub/plug/README.md)
  - [MCP-SPEC.md](/Users/robdezendorf/Documents/GitHub/plug/docs/MCP-SPEC.md)
