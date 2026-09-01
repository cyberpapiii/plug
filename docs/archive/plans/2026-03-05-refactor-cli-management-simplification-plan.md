---
title: Refactor CLI management views for simplicity
type: refactor
status: active
date: 2026-03-05
---

# Refactor CLI management views for simplicity

> Historical planning note: This refactor plan is implementation context, not a current-state
> reference. Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for current project state.

## Overview

The recent CLI-first phase succeeded at the product level: `plug` now has management views for clients, servers, and tools; a daemon-first workflow; explicit linking and unlinking; server management commands; and effective tool toggling. The implementation is working, but the code structure is carrying too much incidental complexity.

The main problem is structural concentration in [plug/src/main.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/main.rs), which now owns command definitions, render helpers, terminal formatting, daemon startup behavior, interactive prompt flows, config mutation helpers, and most of the management business logic. This makes otherwise reasonable features look overbuilt, slows future iteration, and increases the chance that the next UX refinement creates regressions in unrelated paths.

This refactor should simplify the implementation without rolling back the user-facing command model. The goal is to keep the current command API and management-view product shape, while aggressively reducing cognitive load, duplication, and YAGNI-driven implementation details.

## Problem Statement

The recent phase created a strong operator experience, but it also created a maintenance hotspot.

Current symptoms:

- `plug/src/main.rs` has become the de facto application layer, view layer, and mutation layer all in one file.
- Rendering concerns and management concerns are intertwined, which makes small UX changes more expensive than they should be.
- Similar management patterns exist in multiple places with slightly different local implementations.
- Tool disabling supports more matching flexibility than the product may actually need.
- The docs now describe the right product direction, but the implementation does not yet reflect that same simplicity internally.

This matters because the project’s stated principle is ruthless minimalism. The current product shape is justified; the implementation shape is not yet aligned with that principle.

## Proposed Solution

Keep the command surface and management-view behavior, but refactor the implementation into a smaller number of clear modules with tighter responsibilities.

At a high level:

- Keep `plug`, `plug clients`, `plug servers`, `plug tools`, `plug doctor`, `plug config`, and the low-level mutation subcommands.
- Extract management-view rendering and interaction out of `main.rs`.
- Extract config mutation helpers for clients, servers, and tools into focused modules.
- Consolidate repeated prompt/menu patterns into a single reusable helper where appropriate.
- Re-evaluate arbitrary wildcard tool-pattern support and narrow it if exact names plus `--server` fully cover real usage.
- Preserve all current behavior unless a simplification clearly improves the product and does not remove a proven user story.

## Research Summary

### Repo Findings

Primary implementation hotspots:

- [plug/src/main.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/main.rs)
- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [plug-core/src/config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs)

Key structural observations:

- `main.rs` currently contains command enums, terminal formatting helpers, auto-start logic, overview/status rendering, management views, prompt handlers, link/unlink flows, server management flows, tool enable/disable flows, import/setup/doctor/config logic.
- The management-view pattern is now real and intentional, as documented in [docs/UX-DESIGN.md](/Users/robdezendorf/Documents/GitHub/plug/docs/UX-DESIGN.md), but the implementation is not yet decomposed to match that product taxonomy.
- Tool disabling is persisted in config through `disabled_tools`, then enforced in the proxy via `is_disabled_tool()` and `wildcard_match()`.

### Institutional Learnings

Relevant existing solution docs:

- [docs/solutions/ui-bugs/management-action-menu-repaint-jitter-cli-20260305.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/ui-bugs/management-action-menu-repaint-jitter-cli-20260305.md)
  The management views should treat `dialoguer` as a prompt library, not a panel framework. Rich framing around live selectors caused repaint instability.
- [docs/solutions/code-quality/phase4-p3-polish-code-review-fixes.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/code-quality/phase4-p3-polish-code-review-fixes.md)
  The project already has precedent for eliminating duplicated config/router construction and simplifying code paths after feature delivery.

### Research Decision

No external research is needed.

Reasoning:

- This is an internal refactor with strong local context.
- The repo already contains the relevant UX intent and the recent bug learnings.
- The risk is maintainability and local complexity, not unfamiliar framework behavior.

## SpecFlow Analysis

### Primary User Flows Affected

1. User runs `plug clients`, `plug servers`, or `plug tools`
2. View renders current state cleanly
3. User optionally performs an in-place management action
4. Config is mutated safely
5. View re-renders with updated state

### Likely Failure Modes

- Refactor breaks interactive loop behavior and users get dropped out of a management view unexpectedly.
- Rendering helpers move but text wrapping, spacing, or terminal-width handling regresses.
- Config mutation logic gets split incorrectly and one path stops persisting changes.
- Tool-disable simplification removes wildcard support before confirming whether it is still needed.
- JSON output paths regress while simplifying text-mode rendering.

### Gaps To Explicitly Cover In The Plan

- Preserve parity between interactive management views and low-level command API.
- Keep daemon auto-start behavior unchanged unless intentionally revisited.
- Verify both text and JSON outputs for each management surface.
- Treat wildcard-pattern narrowing as a deliberate subdecision, not an incidental cleanup.

## Technical Approach

### Architecture

Refactor the CLI around responsibility boundaries that match the current product model.

Proposed module split under `plug/src/`:

- `main.rs`
  Command definitions and top-level dispatch only.
- `ui.rs`
  Shared terminal formatting, width handling, banners, summaries, wrapped rows, prompt theme.
- `views/overview.rs`
  `plug` and shared overview/status summary rendering.
- `views/clients.rs`
  `plug clients` rendering and its interactive management loop.
- `views/servers.rs`
  `plug servers` rendering and its interactive management loop.
- `views/tools.rs`
  `plug tools` rendering and its interactive management loop.
- `commands/clients.rs`
  `link`, `unlink`, client detection/loading helpers.
- `commands/servers.rs`
  `server add|remove|edit|enable|disable`.
- `commands/tools.rs`
  `tools disable|enable|disabled`.
- `commands/config.rs`
  `config open|path|check` and config load/save helpers.
- `runtime.rs`
  daemon connect/start/wait helpers and shared runtime-status helpers.

This is not a new abstraction layer for its own sake. It is a file-level decomposition of responsibilities the product already has.

### Simplification Principles

- Prefer extraction over new generic abstractions.
- Prefer obvious duplication over clever indirection until at least two real call sites clearly benefit.
- Keep `dialoguer` usage minimal and stable.
- Keep the current command model unless a removal clearly reduces user-facing confusion.
- Avoid introducing a “CLI framework” inside the codebase.

### Wildcard Tool Matching Decision

This refactor should explicitly evaluate whether arbitrary wildcard matching should survive.

Decision rule:

- Keep wildcard support only if there is a real, demonstrated usage beyond exact tool names and `--server <id>`.
- Otherwise reduce `disabled_tools` semantics to:
  - exact tool names
  - server-wide expansion generated by the CLI

If wildcard support is retained, document it more clearly and keep the matching implementation contained to one obvious place.

## Implementation Phases

### Phase 1: Baseline And Safety Net

#### Goals

- Freeze current behavior before moving code.
- Add or strengthen tests around the management-view and mutation command paths.

#### Tasks

- Add regression coverage for:
  - `plug clients --output json`
  - `plug servers --output json`
  - `plug tools --output json`
  - `plug link` / `plug unlink`
  - `plug server add|remove|edit|enable|disable`
  - `plug tools disable|enable|disabled`
- Add smoke coverage for management-view loop behavior where practical.
- Identify any current behavior that is intentionally awkward but must be preserved short-term.

#### Success Criteria

- Behavior-critical flows have automated coverage before files move.
- The team can refactor with confidence instead of relying on manual CLI spot checks alone.

### Phase 2: Extract Shared UI And Runtime Helpers

#### Goals

- Reduce `main.rs` size quickly by moving the lowest-risk shared helpers first.

#### Tasks

- Move terminal styling and wrapping helpers into `ui.rs`.
- Move daemon startup/connect/wait helpers into `runtime.rs`.
- Keep public helper APIs small and concrete.
- Update imports without changing observable behavior.

#### Success Criteria

- `main.rs` loses the bulk of non-command helper code.
- Text rendering output remains unchanged.
- Auto-start and daemon reachability behavior remains unchanged.

### Phase 3: Extract Management Commands By Domain

#### Goals

- Move mutation logic out of `main.rs` and align files with user-facing domains.

#### Tasks

- Extract client management flows into `commands/clients.rs`.
- Extract server management flows into `commands/servers.rs`.
- Extract tool management flows into `commands/tools.rs`.
- Extract config load/save/check helpers into `commands/config.rs` or a small `config_io.rs` helper.
- Leave function names boring and explicit rather than building an abstraction-heavy command framework.

#### Success Criteria

- Each domain has one obvious home.
- `main.rs` primarily routes Clap subcommands to domain functions.
- Mutation paths remain scriptable and behaviorally identical.

### Phase 4: Extract Management Views By Domain

#### Goals

- Separate rendering/interaction concerns from mutation logic.

#### Tasks

- Move `plug clients` rendering loop into `views/clients.rs`.
- Move `plug servers` rendering loop into `views/servers.rs`.
- Move `plug tools` rendering loop into `views/tools.rs`.
- Keep the management-view pattern consistent, but do not force excessive generic reuse.
- If a shared action-menu helper is still clearly useful after extraction, add one small helper in `ui.rs`; otherwise keep each view locally explicit.

#### Success Criteria

- Each management surface is easy to find, read, and refine in isolation.
- The implementation reflects the product taxonomy documented in UX docs.
- The action-menu repaint bug stays fixed.

### Phase 5: Narrow YAGNI Areas

#### Goals

- Remove or isolate the broadest overengineering risks.

#### Tasks

- Review arbitrary wildcard support for disabled tools.
- Remove it if exact names plus `--server` cover real usage.
- If retained, keep it isolated and documented.
- Review whether any prompt/menu helper abstraction is only used once and inline it if so.
- Remove stale compatibility wording or dead comments uncovered during extraction.

#### Success Criteria

- No remaining feature exists only “just in case.”
- Matching and mutation semantics are easy to explain in one sentence.

### Phase 6: Docs And Verification Cleanup

#### Goals

- Make the docs and the code tell the same story.

#### Tasks

- Update [README.md](/Users/robdezendorf/Documents/GitHub/plug/README.md) to keep the management-view model primary.
- Update [docs/UX-DESIGN.md](/Users/robdezendorf/Documents/GitHub/plug/docs/UX-DESIGN.md) to describe the simplified internal direction where relevant.
- Add a short note for contributors about where new CLI work should live.
- Run full verification and the canonical reinstall workflow.

#### Success Criteria

- Future CLI work has a clear home.
- The repo story matches the implementation structure.

## Alternative Approaches Considered

### 1. Roll Back Features To Reduce Complexity

Rejected.

The user-facing command model is now mostly right. Removing management views, server commands, or tool toggles would reduce product quality more than it would reduce harmful complexity.

### 2. Keep Everything In `main.rs` But Tidy It

Rejected.

Small cleanups inside a 2,800-line file do not materially solve the maintenance problem.

### 3. Build A More Generic Internal CLI Framework

Rejected.

This would likely worsen the problem by replacing obvious procedural code with reusable abstractions that are harder to trace.

## System-Wide Impact

### Interaction Graph

- `plug clients` view:
  - render linked/detected/live state
  - prompt for action
  - invoke `link` or `unlink`
  - mutate config on disk
  - return to view and rerender
- `plug servers` view:
  - fetch runtime/config state
  - prompt for action
  - invoke add/edit/remove/enable/disable
  - mutate config on disk
  - return to view and rerender
- `plug tools` view:
  - fetch effective tools, disabled patterns
  - prompt for action
  - mutate config on disk
  - effective tool surface changes on next runtime refresh/listing

### Error & Failure Propagation

- Config read/write failures should remain surfaced as direct CLI errors.
- Daemon IPC failures should continue to fall back cleanly where already supported.
- Interactive flows must not partially mutate config and then silently swallow errors.
- Moving code must not separate validation from persistence in a way that makes partial failure harder to reason about.

### State Lifecycle Risks

- Client linking/unlinking edits real client config files; simplification must not accidentally widen what gets touched.
- Server add/edit/remove changes the canonical `plug` config; backup/overwrite behavior must stay predictable.
- Tool enable/disable changes persisted config state that is later enforced by runtime proxy logic.
- If wildcard support changes, existing config entries may need compatibility handling or a clear migration story.

### API Surface Parity

The following interfaces must stay aligned:

- high-level management views
- low-level direct commands
- JSON output modes for agent/script use
- runtime enforcement of disabled tools

### Integration Test Scenarios

- Add a server, then confirm `plug servers` and `plug tools` reflect it.
- Disable tools, then confirm the effective tool list is actually filtered.
- Link and unlink clients, then confirm `plug clients` reflects the expected states.
- Run management commands with JSON output and confirm schema stability.
- Confirm interactive views still re-enter their loops correctly after a mutation.

## Acceptance Criteria

### Functional Requirements

- [ ] `plug/src/main.rs` is reduced to command definitions and dispatch-oriented logic.
- [ ] Shared terminal formatting and prompt theming live outside `main.rs`.
- [ ] Client, server, and tool mutation logic each live in focused domain modules.
- [ ] Client, server, and tool management views each live in focused view modules.
- [ ] Current management-view behavior remains intact after extraction.
- [ ] JSON outputs remain supported for view commands and direct subcommands.
- [ ] Any retained wildcard tool-pattern behavior is explicitly justified and documented.

### Non-Functional Requirements

- [ ] The refactor reduces cognitive load without introducing a new abstraction-heavy framework.
- [ ] The action-menu repaint bug does not regress.
- [ ] Existing daemon-first UX behavior remains stable unless intentionally changed.
- [ ] The simplified code structure makes future CLI work easier to place and review.

### Quality Gates

- [ ] `cargo check --workspace` passes.
- [ ] Relevant tests pass, including tool-filtering and CLI command-path coverage.
- [ ] `./scripts/dev-reinstall.sh --quick` succeeds.
- [ ] Manual smoke tests confirm `plug`, `plug clients`, `plug servers`, `plug tools`, `plug doctor`, and `plug config check` still behave correctly.

## Success Metrics

- `plug/src/main.rs` drops substantially from its current size.
- New contributors can identify the correct home for a change without reading the whole CLI file.
- CLI UX refinements can be made without touching unrelated command logic.
- The number of repeated local patterns in management prompts and views is reduced.

## Dependencies & Risks

### Dependencies

- Existing CLI behavior must be understood and preserved before moving code.
- Current tests may need expansion before safe extraction.

### Risks

- Accidental behavior drift during file moves.
- Over-correcting into too many tiny modules.
- Removing wildcard support too early if there is hidden real usage.
- Breaking text layout or JSON output while simplifying shared helpers.

### Mitigations

- Add behavioral coverage first.
- Move code in phases rather than rewriting all at once.
- Keep modules coarse and domain-based.
- Treat wildcard narrowing as an explicit decision point with verification.

## Resource Requirements

This is a medium-sized refactor best handled in one focused implementation pass with verification at each phase. No new infrastructure is required.

## Future Considerations

- If the CLI continues growing, consider a small contributor-facing architecture note for the CLI layer.
- If management views eventually become richer than `dialoguer` can comfortably support, that should trigger a deliberate TUI decision rather than more prompt-library stretching.

## Documentation Plan

Update:

- [README.md](/Users/robdezendorf/Documents/GitHub/plug/README.md)
- [docs/UX-DESIGN.md](/Users/robdezendorf/Documents/GitHub/plug/docs/UX-DESIGN.md)
- optionally a short contributor note if the extracted structure is non-obvious

## Sources & References

### Internal References

- [plug/src/main.rs](/Users/robdezendorf/Documents/GitHub/plug/plug/src/main.rs)
- [plug-core/src/proxy/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/proxy/mod.rs)
- [plug-core/src/config/mod.rs](/Users/robdezendorf/Documents/GitHub/plug/plug-core/src/config/mod.rs)
- [README.md](/Users/robdezendorf/Documents/GitHub/plug/README.md)
- [docs/UX-DESIGN.md](/Users/robdezendorf/Documents/GitHub/plug/docs/UX-DESIGN.md)

### Institutional Learnings

- [docs/solutions/ui-bugs/management-action-menu-repaint-jitter-cli-20260305.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/ui-bugs/management-action-menu-repaint-jitter-cli-20260305.md)
- [docs/solutions/code-quality/phase4-p3-polish-code-review-fixes.md](/Users/robdezendorf/Documents/GitHub/plug/docs/solutions/code-quality/phase4-p3-polish-code-review-fixes.md)

### Recent Related Work

- `67e5af0` `feat(cli): reposition plug around guided CLI workflows`
- `3c79630` `feat(cli): add interactive management views`
- `519a3b1` `feat(cli): add server shortcuts for tool toggles`
- `22b9683` `style(cli): unify management view UX`
- `500751b` `fix(cli): stabilize management action menus`
