# Technical Review: Charm-Style UX Overhaul

## Architectural Soundness
**Feedback:** The decision to adopt The Elm Architecture (TEA) for Ratatui is structurally sound. However, retrofitting an immediate-mode GUI with a strict TEA pattern (Model-Update-View) can be challenging if the existing `App` struct is heavily intertwined with side-effects (e.g., direct network calls).
**Blind Spot:** The plan mentions recalculating `Rect` layouts during the event loop to handle mouse events. This violates the DRY principle and TEA by putting layout logic in the "Update" phase.
**Concrete Improvement:** Instead of recalculating `Rect`s, the `draw` (View) function should write its computed `Rect` boundaries back into the `Model` (or a dedicated `ViewState` struct). When a mouse event occurs in the `Update` phase, it simply checks against this cached layout state.

## Performance Implications
**Feedback:** Adding padding and rounded borders has negligible performance impact in Ratatui. However, enabling Mouse Capture generates a significantly higher volume of terminal events (movements, hovers, clicks) compared to keyboard-only input.
**Blind Spot:** If the TUI attempts to re-render the entire screen on every single mouse movement event, CPU usage will spike.
**Concrete Improvement:** Implement event debouncing or filtering. Only trigger a full re-render (`app.dirty = true`) when a mouse event actually changes state (e.g., a click that changes a tab, or a hover that changes highlighting), ignoring raw movement events that don't affect the view.

## UX Edge Cases & Maintainability
**Feedback:** The `plug setup` interactive wizard is a massive QoL improvement. Defaulting `plug` to `plug tui` makes sense for discoverability.
**Blind Spot 1:** Mouse support in terminals via SSH, tmux, or older terminal emulators (like Windows Console) can be flaky. If a user clicks and it doesn't work, the experience degrades rapidly.
**Concrete Improvement:** Always maintain 100% keyboard parity. The Tab bar must be navigable via `Tab`/`Shift-Tab` or `←/→` arrows. Do not hide the keyboard shortcuts entirely; display them subtly next to the tabs (e.g., `[1] Dashboard`).
**Blind Spot 2:** `plug setup` modifying `~/.claude.json` directly could corrupt the user's existing config if not handled safely.
**Concrete Improvement:** The setup wizard must take a backup of any client config file before modifying it (e.g., `~/.claude.json.bak`), and the plan should explicitly state this as an Acceptance Criterion.

## Summary
The plan is strong and directly addresses the core UX issues. To make it production-ready:
1. Cache layout `Rect`s during `draw` instead of recalculating during events.
2. Filter noisy mouse movement events to preserve CPU.
3. Ensure absolute keyboard parity as a fallback for terminal mouse issues.
4. Mandate config file backups in the `plug setup` wizard.
