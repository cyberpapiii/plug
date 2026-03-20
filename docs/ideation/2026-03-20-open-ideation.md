---
date: 2026-03-20
topic: open
focus: open-ended repo-wide ideation synthesis
---

# Ideation: Open Repo-Wide Improvement Synthesis

This artifact merges two independent ideation passes into one ranked baseline.

## Codebase Context

- `plug` is a Rust workspace with three main crates:
  - `plug` for the CLI/operator surface
  - `plug-core` for runtime, proxying, HTTP, auth, reload, and transport logic
  - `plug-test-harness` for integration helpers
- The current-state docs say the major roadmap is complete on `main`, so the best next ideas are not broad protocol expansion. The highest-leverage directions are proof, operator diagnosis, maintenance simplification, and truth-drift prevention.
- Runtime truth is intentionally explicit and spread across the daemon, HTTP session inventory, reload/health loops, auth state, and operator commands such as `status`, `doctor`, `clients`, and `auth status`.
- The biggest obvious maintenance hotspot in code is `plug-core/src/proxy/mod.rs`, which is large and carries many concerns.
- The repo already has meaningful test coverage, including `plug-core/tests/integration_tests.rs` and many focused async tests in `plug-core/src/http/server.rs`, `plug/src/daemon.rs`, `plug/src/runtime.rs`, and other modules. The gap is better framed as scenario-level proof and repeatable compatibility verification, not an absence of tests.
- The docs surface is intentionally rich but large: about 150 markdown files under `docs/`. Cleanup should preserve compound knowledge and truth taxonomy rather than treat most docs as disposable noise.
- `docs/solutions/` now contains meaningful institutional knowledge. There was no recent ideation document to resume.

## Ranked Ideas

### 1. Incident Bundle / Runtime Trace (`plug trace`)
**Description:** Add a command that captures a sanitized, shareable runtime bundle: recent daemon logs, `status`, `doctor`, `auth status`, live-session inventory, runtime topology, and recent reload/auth events for one reproducible debugging snapshot.
**Rationale:** After the second ideation team's feasibility pass, this now looks like the strongest near-term bet. Most of the diagnostic surface already exists; the work is mainly composition, log/event capture, and packaging. The repo's next pain is increasingly "what happened?" and "what state was the system in?" rather than missing core MCP capability. The engine event bus is already rich, but recent events are broadcast-only rather than queryable after the fact.
**Downsides:** Secret redaction and bundle scope need a high bar. If poorly scoped, this could become noisy and hard to trust.
**Confidence:** 90%
**Complexity:** Low-Medium
**Status:** Unexplored

### 2. Extract `proxy/mod.rs` Into Per-Concern Modules
**Description:** Break `plug-core/src/proxy/mod.rs` into a small set of focused modules around routing snapshots, call correlation, notification fanout, reverse-request bridging, and client-facing protocol handling.
**Rationale:** This was one of the strongest grounded ideas from the other ideation pass and survives validation cleanly. The file is large, central, and likely to remain a maintenance bottleneck. This is the clearest code-health improvement with immediate payoff for future correctness work.
**Downsides:** Mechanical refactors can create churn and temporary instability if the slice boundaries are poor. On its own, this does not improve the product unless it is done in service of clarity and easier follow-on changes.
**Confidence:** 85%
**Complexity:** Medium-Low
**Status:** Unexplored

### 3. Scenario Verification and Failure-Class Tests
**Description:** Add a curated set of scenario-level checks for the recurring failure classes the project keeps rediscovering: mixed IPC/HTTP parity, notification fanout, reload races, OAuth recovery, and remote-client compatibility. This can start as targeted tests using the current harness and grow into a `plug verify` operator surface if it proves useful.
**Rationale:** This keeps the best part of both ideation passes. The repo already has meaningful integration and async test coverage, so the opportunity is not "invent a test framework." The opportunity is to encode the specific failure classes that matter to this product and make them easy to rerun and trust.
**Downsides:** If it tries to become a general framework too early, it will sprawl. The value comes from tight curation around real bug classes.
**Confidence:** 80%
**Complexity:** Medium
**Status:** Unexplored

### 4. Causal Diagnosis Surface (`plug explain`)
**Description:** Add an explainer command that answers why a server or client is in a given state, for example failed, degraded, auth-required, linked-but-not-live, or partial inventory, without forcing the user to mentally join multiple operator surfaces.
**Rationale:** The follow-up exploration made the gap more concrete: there are real operator scenarios where users must cross-reference several commands to explain disappearing tools, auth-to-health cascades, or client-specific behavior. The existing model already contains useful ingredients such as server health, consecutive-failure tracking, and circuit-breaker state; phase one could focus on surfacing those coherently for `plug explain server <name>`.
**Downsides:** If it becomes a thin wrapper around existing output, it adds surface area without reducing operator effort. It needs to express causality, not just restate state.
**Confidence:** 75%
**Complexity:** Medium
**Status:** Unexplored

### 5. Truth Guard (`xtask` + CI Drift Checks)
**Description:** Add an automated truth-pass helper that checks roadmap-relevant current-state docs for drift against `main`, validates post-merge hygiene expectations, and warns when the current snapshot baseline has fallen behind the actual code.
**Rationale:** This is now strong enough to stand on its own instead of being folded into docs cleanup. The repo already has explicit truth rules and a post-merge truth pass; the mechanics are structured enough for automation. During this ideation session, the current snapshot baseline commit referenced in the truth docs lagged `HEAD` in the working copy, which is exactly the class of drift this would catch early.
**Downsides:** A heavy-handed version could create brittle CI or noisy failures. The automation needs to enforce only the truth contract that actually matters.
**Confidence:** 80%
**Complexity:** Low-Medium
**Status:** Unexplored

### 6. Docs Truth-Tiering and Archive Pass
**Description:** Classify the documentation surface into clear truth tiers, archive or relabel historical planning and brainstorm material, and preserve `docs/solutions/` as compound knowledge instead of treating it as clutter.
**Rationale:** This retains the valid signal from the "delete 100+" idea while respecting how this repo actually works. There are about 150 markdown documents under `docs/`, and the repo relies on docs as operational memory. The win is not mass deletion; it is making the trust hierarchy obvious and reducing agent/operator confusion about what counts as current truth.
**Downsides:** If done without a clear taxonomy, it can produce more naming churn than clarity.
**Confidence:** 75%
**Complexity:** Low
**Status:** Unexplored

### 7. `plug upgrade` Auto-Restart Polish
**Description:** Add a tiny quality-of-life path that restarts or refreshes the relevant local background service state after reinstall or upgrade so the binary on disk and the long-lived process do not drift.
**Rationale:** This is tactical rather than repo-shaping, but it kept surviving scrutiny because it is cheap and removes repeat friction from real usage. It should stay below the structural and operator-facing ideas, but it is a credible quick win.
**Downsides:** Easy to overfit to one local workflow or one platform's service model.
**Confidence:** 90%
**Complexity:** Very Low
**Status:** Unexplored

## Rejection Summary

| # | Idea | Reason Rejected |
|---|------|-----------------|
| 1 | Full daemon/serve unification as stated | The rationale was stale because daemon-owned HTTP already exists on `main` in shared-runtime mode. The remaining problem is narrower clarity and polish, not a from-scratch architectural merge. |
| 2 | Generation-tagged health/refresh task reconciliation | Plausible correctness follow-up, but not yet strong enough as a headline direction without a reproduced failure mode. |
| 3 | Finish request-ID correlated reverse-request routing | Important if a concrete concurrency gap remains, but the codebase already has substantial request/session correlation logic and tests. Better treated as a scoped follow-up than a top-level bet. |
| 4 | Fully live runtime reconfiguration | Still attractive future scope, but too risky and architecture-heavy relative to the current post-roadmap bar. |
| 5 | Bulk-delete 100+ docs | Too blunt for a repo that intentionally uses plans, audits, solutions, and truth docs as operating memory. Archive and truth-tier instead. |
| 6 | Purely tactical install-service polish as the main bet | Useful, but it should remain a quick win below the stronger operator, verification, and truth-management ideas. |

## Session Log

- 2026-03-20: Initial open-ended ideation pass completed. One direct pass produced 5 survivors and a second independent team pass produced 7 survivors. The two sets were merged, re-validated against the repo, synthesized into 12 shortlisted candidates, and filtered to 6 final survivors.
- 2026-03-20: Follow-up refinement incorporated a third synthesis pass from the second team, plus local validation of `plug trace`, `plug explain`, and Truth Guard feasibility. Ranking changed materially: `plug trace` moved to #1, scenario verification was reframed as targeted failure-class coverage with optional future `plug verify` packaging, Truth Guard split from docs cleanup, and the runtime-topology simplification idea dropped out of the survivor set.
