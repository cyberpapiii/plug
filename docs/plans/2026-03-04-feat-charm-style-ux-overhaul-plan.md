---
title: "feat: Charm-Style UX Overhaul"
type: feat
status: active
date: 2026-03-04
origin: docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md
---

# feat: Charm-Style UX Overhaul

## Enhancement Summary

**Deepened on:** 2026-03-04
**Sections enhanced:** 5
**Research agents used:** WebSearch (Ratatui Best Practices, Charm Bracelet aesthetics)

### Key Improvements
1. **Elm Architecture Pattern:** Adopted a clear Model-Update-View separation for handling Ratatui state and events.
2. **Contextual Layouts:** Added specific recommendations for `Tabs` and `Padding` to achieve the "Lip Gloss" aesthetic.
3. **Event Routing:** Outlined a concrete strategy for handling Crossterm mouse events and mapping them to `Rect` intersections.

### New Considerations Discovered
- Mouse handling in immediate-mode GUIs like Ratatui requires recalculating `Rect` layouts during the event loop to accurately detect clicks.
- Generous use of `Block::padding` and `BorderType::Rounded` are critical for achieving the softer, modern "Charm" look.

---

## Overview

The `plug` CLI currently exposes a disjointed "power-user" UX based around the Unix philosophy. Commands like `plug connect`, `plug serve`, and `plug tui` are separated, and the TUI relies on hidden, vim-style navigation keys (`1/2/3`, `j/k`, `Tab`) and plain, unpadded UI components.

This feature transforms the UX to follow the "Charm Bracelet" philosophy (similar to Bubble Tea/Lip Gloss) by adding an interactive `setup` concierge, defaulting the CLI to the TUI dashboard, introducing a visible Tab Bar for navigation, padding/softening the UI, and adding contextual footers.

### Research Insights

**Best Practices:**
- Treat the TUI as a first-class application using The Elm Architecture (Model-Update-View).
- Use `ratatui::widgets::Tabs` to provide explicit navigation cues rather than hidden shortcuts.

## Problem Statement / Motivation

A "normie" user onboarding to `plug` shouldn't have to read the `--help` to figure out how to configure their clients or navigate the dashboard. We want the user experience to be beautiful, intuitive, and highly proactive in fixing issues (e.g. actionable health states via the `doctor` command).

## Proposed Solution

1. **Default to TUI:** Running `plug` with no arguments will automatically launch `plug tui`.
2. **`plug setup` command:** A new interactive wizard that scans `~/.claude.json`, `~/.cursor/mcp.json`, etc., and asks the user "I found Cursor. Should I link Plug to it? [Y/n]".
3. **Charm-style TUI refactor:**
   - Soften `Theme` with adaptive palettes and rounded borders.
   - Introduce generous margins/padding around `Block`s.
   - Replace the static status bar with **Contextual Footers** (e.g., when focused on "Servers", show `[r] restart [enter] fix`).
   - Create a visible **Tab Bar** replacing the `1/2/3` and `t/l/d` mode switching.
4. **Interactive `plug doctor`:** Give the TUI a way to execute fixes for degraded servers directly from the dashboard.

### Research Insights

**Implementation Details:**
```rust
// Achieving the Charm look with Blocks
use ratatui::widgets::{Block, Borders, BorderType, Padding};

let block = Block::default()
    .borders(Borders::ALL)
    .border_type(BorderType::Rounded)
    .padding(Padding::new(1, 1, 1, 1)) // Left, Right, Top, Bottom padding
    .title(" Context ");
```

## Technical Considerations

- **Crossterm Mouse Support:** We must enable mouse capture in `ratatui::init()` so the Tab Bar and `[ FIX ]` buttons can be clicked.
- **TUI Architecture:** `app.rs` will need to track the active Tab instead of a raw `focused_panel` integer. 
- **Ratatui Constraints:** Adding padding will decrease the available space for lists, so we might need to adjust the `compute_layout` logic.

### Research Insights

**Edge Cases (Mouse Handling):**
- Ratatui is immediate-mode, meaning `Rect`s are calculated during `draw`. To handle a mouse click, you must either store the `Rect`s during the render pass or mathematically recalculate them in your event loop. 
- Use `crossterm::event::EnableMouseCapture` and check intersections:
  `rect.intersects(Rect::new(mouse_event.column, mouse_event.row, 1, 1))`

## System-Wide Impact

- **Interaction graph**: `plug` -> `main.rs` matches `None` args and defaults to `cmd_tui`. TUI `app.rs` dispatches `PendingAction`s based on contextual keybindings.
- **State lifecycle risks**: Enabling mouse capture requires ensuring it is cleanly disabled on panic/shutdown so the terminal doesn't get stuck.
- **API surface parity**: `plug import` and `plug export` remain for scripting, but `plug setup` wraps them interactively.

## Acceptance Criteria

- [ ] Running `plug` (no args) opens the dashboard.
- [ ] `plug setup` asks interactive Yes/No questions to configure clients.
- [ ] TUI uses rounded borders and spacing (padding).
- [ ] TUI displays a visible Tab Bar for navigation (Dashboard, Tools, Logs, Doctor).
- [ ] TUI displays contextual keybindings in the footer depending on the active Tab.
- [ ] Mouse clicks on tabs correctly switch views.

## Success Metrics

- Users can go from zero to configured without reading documentation.
- The TUI feels spacious, responsive, and obvious to navigate.

## Dependencies & Risks

- Ratatui's mouse handling can be tricky to map to specific screen coordinates (Rects).
- Creating interactive prompts in CLI requires a library like `dialoguer` or manual stdout/stdin handling in `plug setup`.

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md](docs/brainstorms/2026-03-04-charm-style-ux-overhaul-brainstorm.md)
- **Ratatui Documentation:** https://ratatui.rs/
