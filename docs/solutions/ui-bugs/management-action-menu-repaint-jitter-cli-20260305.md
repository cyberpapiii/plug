---
module: System
date: 2026-03-05
problem_type: ui_bug
component: tooling
symptoms:
  - "The action menu moved upward on screen while changing the selected row in `plug clients`"
  - "Adding visual separators around the action area caused the selector to smear or stack text while repainting"
  - "The management views felt unstable even though the underlying actions still worked"
root_cause: wrong_api
resolution_type: code_fix
severity: medium
tags: [cli, dialoguer, terminal-ui, management-views, repaint, menu]
---

# Troubleshooting: Management Action Menu Repaint Jitter In CLI Views

## Problem

The new management views for `plug clients`, `plug servers`, and `plug tools` rendered correctly at first, but the interactive action picker behaved badly once the selector moved. The menu appeared to jump upward, and extra visual framing made the repaint artifacts more obvious.

This was not a daemon or state issue. It was a terminal rendering problem in the action picker layer.

## Environment

- Module: System
- Affected Component: CLI tooling
- Date: 2026-03-05

## Symptoms

- Moving the selector in `plug clients` made the action menu appear to shift upward on the terminal
- Adding a `Controls` boundary above the picker caused the menu to smear as `dialoguer` repainted the active row
- Rich menu rows with padded labels and dimmed descriptions were visually unstable during redraw
- The issue reproduced in the real installed `plug` binary, not just `cargo run`

## What Didn't Work

**Attempted Solution 1:** Add a visible `Controls` block above the action picker
- **Why it failed:** `dialoguer::Select` redraws the prompt area in place. Extra printed chrome immediately above the live selector amplified repaint glitches instead of containing them.

**Attempted Solution 2:** Keep the action picker but embed descriptive helper text directly into every selectable row using styled strings
- **Why it failed:** The richer rows made the redraw surface more complex. The menu became visually unstable as the selector moved, likely due to width measurement and repaint behavior in the terminal UI library.

**Attempted Solution 3:** Add static `Actions` documentation above the picker and keep the interactive menu below it
- **Why it failed:** This did not fix repaint behavior and also made the screen redundant. The user had to read the same action information twice in two different formats.

## Solution

The stable fix was to simplify the live selector back down to plain, short option labels and remove decorative framing around the interactive region.

**Code changes**:

```rust
// Before (unstable):
let options = [
    format!("{:<18} {}", "Done", style("Exit this management view").dim()),
    format!(
        "{:<18} {}",
        "Link clients",
        style("Add plug to one or more client configs").dim()
    ),
    format!(
        "{:<18} {}",
        "Unlink clients",
        style("Remove plug from selected client configs").dim()
    ),
];

let selection = Select::with_theme(&cli_prompt_theme())
    .with_prompt("Choose action")
    .items(&options)
    .default(0)
    .interact_opt()?;
```

```rust
// After (stable):
let options = ["Done", "Link clients", "Unlink clients"];

let selection = Select::with_theme(&cli_prompt_theme())
    .with_prompt("Choose action")
    .items(options)
    .default(0)
    .interact_opt()?;
```

The same simplification was applied to:

- `prompt_client_actions()`
- `prompt_server_actions()`
- `prompt_tool_actions()`

The extra `Controls` framing function was also removed entirely.

## Why This Works

The root problem was not the business logic. It was the mismatch between the desired “rich management panel” presentation and what `dialoguer::Select` reliably supports during repaint.

`dialoguer` is good at:

1. Rendering a prompt
2. Highlighting one active row
3. Repainting a compact option list in place

It is not a full layout engine. Once the action rows started carrying styled/descriptive text and extra framing was printed directly above the live selector, the repaint region became fragile. The terminal UI looked like it was moving even though the library was just redrawing the active row over a more complicated printed surface.

Simplifying the options back to plain labels reduced the redraw surface to the core behavior `dialoguer` handles well. Removing the extra framing removed another source of repaint artifacts. The management views kept their structure, but the live interactive area became minimal and stable.

## Prevention

- Keep `dialoguer::Select` rows short and plain unless there is strong evidence richer rows repaint cleanly
- Do not print decorative framing directly adjacent to a live selector unless it has been tested in a real PTY
- Treat `dialoguer` as a prompt library, not a general-purpose panel renderer
- Prefer stable interaction over richer copy in live terminal menus
- If a management view needs fully styled panels or background regions, that is a sign the interface is crossing into true TUI territory

## Related Issues

- See also: [phase4-tui-dashboard-daemon-patterns.md](../integration-issues/phase4-tui-dashboard-daemon-patterns.md)
- See also: [rmcp-sdk-integration-patterns-plug-20260303.md](../integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md)
