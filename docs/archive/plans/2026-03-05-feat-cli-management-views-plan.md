---
title: feat: CLI management views
type: feat
status: active
date: 2026-03-05
---

# feat: CLI management views

> Historical planning note: This file is implementation history, not a canonical current-state
> reference. Use `docs/PROJECT-STATE-SNAPSHOT.md` and `docs/PLAN.md` for current project state.

## Goal

Reshape `plug` around a cleaner operator model without losing the lower-level command API used by scripts, agents, and direct power-user workflows.

## Problem

The current CLI is materially better than before, but the main management jobs are still blurred:

- client state is hidden behind counts and the `link` flow
- `status` and `servers` overlap too much
- `doctor` risks overlapping with future config validation
- server and tool management are not yet first-class jobs

## Product Model

Two layers should coexist:

1. Command API
   Narrow, precise, scriptable commands that agents and power users can call directly.

2. Management views
   Condensed operator surfaces with progressive disclosure and interactive workflows.

## Scope For This Pass

### Primary additions

- Add `plug clients`
  - show linked, detected, and live clients
  - keep JSON output available

- Add `plug unlink`
  - explicit inverse of `plug link`
  - support direct targets and `--all`

- Add `plug config check`
  - narrow, deterministic config validation
  - separate from `plug doctor`

### Clarifying behavior

- Keep `plug config` as the “open config” default
- Keep `plug status`, `plug servers`, and `plug tools` for now
- Do not attempt the full taxonomy collapse in this pass

## Future follow-ups

- `plug server add|remove|edit`
- `plug tools enable|disable|disabled`
- stronger `plug clients` interactive management loop
- reframe `plug status` as overview/runtime only
- reduce or repurpose overlap between `status` and `servers`

## Acceptance Criteria

- [ ] `plug clients` exists and shows meaningful client state
- [ ] live client data comes from daemon session state, not just counts
- [ ] `plug unlink` exists and removes plug integration from selected clients
- [ ] `plug config check` exists and is clearly narrower than `plug doctor`
- [ ] `plug --help` and README remain consistent with the current product story
- [ ] text and JSON outputs both work for the new commands
