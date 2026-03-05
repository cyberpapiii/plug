---
title: "feat: CLI Menu-First Repositioning"
type: feat
status: active
date: 2026-03-05
origin: docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md
---

# feat: CLI Menu-First Repositioning

## Overview

`plug` has already pivoted away from the TUI-first direction. The current product value is in the core multiplexer, daemon/runtime behavior, onboarding flows, import/export, doctoring, and tool presentation. The CLI now needs to become the primary product surface instead of feeling like a temporary control panel for a deferred dashboard.

This plan intentionally supersedes the TUI-forward parts of the earlier UX brainstorm and related docs. The near-term product is:

- a strong guided CLI for humans
- structured JSON output for agents
- a clear distinction between user-facing commands and plumbing

The deferred TUI should not shape the current command model, help text, or README promises.

## Problem Statement / Motivation

The repo currently has three conflicting stories:

1. Docs still describe `fanout` and a default TUI/dashboard model.
2. The binary exposes a flat `plug` CLI centered on setup/status/tools/servers plus transport internals.
3. The shipped code has already moved toward CLI-first UX polish, but the command naming, no-args behavior, and help structure still reflect a transition state.

This mismatch makes the product feel less elegant than it is. The goal is to make the CLI feel intentional, truthful, and job-oriented now, without waiting for a future TUI.

## Chosen Approach

Adopt a **CLI/menu-first** product surface with three rules:

1. **`plug` with no args must be useful**
   - It should act like a home screen or lightweight menu, not a raw help dump.
   - It should guide the user toward setup, linking, or status depending on current state.

2. **Top-level commands must map to user jobs**
   - Core human flows stay prominent: `setup`, `status`, `doctor`, `servers`, `tools`, `repair`, `config`.
   - Plumbing commands remain available but are visually demoted: `connect`, `serve`, `stop`, `reload`.

3. **Docs must tell the truth about today’s product**
   - Stop promising a default dashboard/TUI.
   - Stop documenting command shapes that do not exist.
   - Reframe the product as “excellent backend + guided CLI” for this phase.

## Proposed Changes

### 1. No-Args Experience

- [x] Running `plug` with no subcommand prints a compact overview instead of help.
- [x] The overview adapts to state:
  - no config: recommend `plug setup`
  - config present but likely not linked: recommend `plug link`
  - daemon healthy: show concise status summary
- [x] The overview ends with clear next actions.

### 2. Command Surface Cleanup

- [x] Introduce `plug link` as the primary human-facing client-linking flow.
- [x] Keep `plug export` as compatibility if needed, but make `link` the language used in help/docs.
- [x] Organize help output by user intent:
  - Get Started
  - Inspect
  - Maintain
  - Internal
- [x] Clarify command descriptions so `connect` and `serve` read as plumbing, not the main product.

### 3. Non-Interactive Parity

- [x] Add `--yes` for `setup`.
- [x] Add target selection support for `link` without requiring prompts.
- [x] Ensure JSON output remains available where it already exists.
- [x] Avoid blocking the release on full agent parity for every interactive branch; prioritize the main flows.

### 4. Documentation Repositioning

- [x] Update `README.md` commands and quick start to match the actual CLI.
- [x] Update `docs/VISION.md` to use `plug`, not `fanout`, and remove immediate TUI assumptions.
- [x] Update `docs/UX-DESIGN.md` to describe the CLI-first phase and defer TUI specifics.
- [x] Keep long-term TUI ideas as future direction, not present behavior.

## Technical Notes

- Prefer a small delta to the existing clap layout instead of a wholesale CLI rewrite.
- If clap subcommand requirements need loosening, keep the command model simple and stable.
- Reuse existing `status`, `setup`, `repair`, `export`, and config-loading logic rather than creating a parallel “menu engine”.
- `plug link` should likely delegate to the current export flow with renamed entrypoints.

## Acceptance Criteria

- [x] `plug --help` reads as a product menu, not a transport command dump.
- [x] Running `plug` without args is useful and state-aware.
- [x] Human-facing terminology consistently prefers “link clients” over “export config”.
- [x] README command examples match real commands.
- [x] Vision/UX docs no longer claim a default TUI or `fanout` binary.
- [x] Existing core commands still function after the refactor.

## Risks / Tradeoffs

- Renaming commands too aggressively could create churn; aliases may be needed.
- Non-interactive parity can expand scope quickly if every branch of every prompt flow is covered.
- Documentation cleanup is high leverage, but only if it stays tightly aligned with the actual code in this same change.

## Sources

- **Origin brainstorm:** [docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md](docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md)
- [README.md](README.md)
- [docs/VISION.md](docs/VISION.md)
- [docs/UX-DESIGN.md](docs/UX-DESIGN.md)
- [plug/src/main.rs](plug/src/main.rs)
