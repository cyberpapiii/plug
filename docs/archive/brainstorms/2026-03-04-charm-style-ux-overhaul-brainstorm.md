---
title: "brainstorm: Charm-Style UX Overhaul"
date: 2026-03-04
---

# Brainstorm: Charm-Style UX Overhaul

## Problem Statement
The current plug CLI and TUI are powerful but very confusing for "normie" users. It relies on Unix philosophy with disjointed commands (`plug connect`, `plug serve`, `plug tui`) and non-obvious vim-style keybindings (`1/2/3`, `j/k`, `t/l/d`). We want an intuitive, "Charm Bracelet" style experience (like Bubble Tea/Lip Gloss) focused on visual guidance and immediate usability.

## Key Decisions & Rationale
1. **Default Command**: Running `plug` without arguments should launch the TUI (Dashboard) instead of showing the help text.
2. **Onboarding / Setup**: A new `plug setup` (or interactive `plug init`) command that acts as a "Concierge", auto-detecting client configs and prompting to merge them.
3. **Tabbed Navigation**: Move away from hidden panel cycling. Introduce a persistent, visible Tab Bar (Dashboard | Tools | Logs | Doctor).
4. **Visual Styling**: Adopt Lip Gloss styling principles in Ratatui: rounded borders, generous padding/margins, adaptive and softer color palettes.
5. **Contextual Help**: Replace the global `?` help with contextual footers that show exactly what keys do in the current panel.
6. **Actionable Health**: Instead of a passive "Degraded" text, provide interactive actions (e.g. `[ FIX ]` button that integrates with the `doctor` command).

## Chosen Approach
We will refactor the Ratatui implementation in `plug/src/tui/` to support the new styling and layout. We'll update the CLI router in `plug/src/main.rs` to default to `cmd_tui` if no arguments are provided. We will introduce `tui-input` or similar logic if needed, but start with structural improvements.

## Open Questions
- How do we handle mouse clicks on the Tab Bar and interactive buttons in Ratatui? (Needs `Crossterm` mouse capture enabled).
- Should `plug setup` completely replace `plug import` and `plug export`, or just wrap them interactively? (Wrap them interactively to preserve scriptability).
