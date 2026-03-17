# Operator Recovery UX Polish Plan

Date: 2026-03-17

Purpose: tighten the operator-facing CLI surfaces now that auth hardening, session parity, and
daemon-owned HTTP runtime work are on `main`.

## Problem

`plug` now exposes more truthful runtime/auth/topology state, but the operator experience still
requires too much cross-referencing between commands.

The remaining friction is mostly about:

- understanding what path a client/server is actually using
- distinguishing linked config from currently live runtime behavior
- understanding whether status is live daemon truth or fallback inference
- getting one deterministic next action instead of several plausible ones

## Goals

1. Make transport/auth/runtime state readable without joining multiple screens mentally.
2. Make recovery guidance deterministic and short.
3. Make live-vs-fallback scope explicit everywhere it matters.
4. Reduce table/layout noise where detailed topology overwhelms the primary state.

## Desired User Outcomes

After this phase, an operator should be able to answer quickly:

- Is this client linked via stdio or HTTP?
- Is it currently live, and over which transport?
- Is this auth state live daemon truth or just stored-credential fallback?
- If something is wrong, what exact command should I run next?

## Execution Order

### 1. Clients screen polish

Outcome:
- configured client rows show linked mode and live mode clearly
- long endpoint/config details no longer dominate the table
- HTTP-linked vs HTTP-live vs stdio-linked distinctions are obvious

Likely files:
- `plug/src/commands/clients.rs`
- `plug/src/views/clients.rs`

### 2. Status and overview polish

Outcome:
- service/runtime summary explains linked topology vs live runtime separately
- recovery lines are grouped by action, not by implementation detail
- live inventory labels stay short and stable

Likely files:
- `plug/src/views/overview.rs`
- `plug/src/ui.rs`

### 3. Auth status polish

Outcome:
- live daemon vs stored-credentials fallback is unmistakable
- auth states use short, operator-friendly categories
- next action is always attached to the state

Likely files:
- `plug/src/commands/auth.rs`

### 4. Doctor recovery polish

Outcome:
- `doctor` stops feeling like a dump of checks and becomes a recovery guide
- live service issues, auth issues, and config issues are visually separated
- repeated/redundant next actions are consolidated

Likely files:
- `plug/src/commands/misc.rs`

### 5. Final copy/layout consistency pass

Outcome:
- `clients`, `status`, `auth status`, and `doctor` share one operator vocabulary
- short labels and prose are consistent
- JSON contract remains additive and stable

Likely files:
- `plug/src/views/clients.rs`
- `plug/src/views/overview.rs`
- `plug/src/commands/auth.rs`
- `plug/src/commands/misc.rs`

## Acceptance Criteria

- transport/auth/live state is readable from `plug clients` without joining two sections mentally
- `plug status` distinguishes linked configuration from live runtime state cleanly
- `plug auth status` clearly labels live vs fallback source and always provides one next action
- `plug doctor` groups follow-up actions coherently instead of scattering them across checks
- text output is materially simpler without losing key topology/auth information
- focused view/command tests pass

## Verification

- `cargo test -p plug views::clients -- --nocapture`
- `cargo test -p plug views::overview -- --nocapture`
- `cargo test -p plug commands::auth::tests -- --nocapture`
- `cargo test -p plug commands::misc::tests -- --nocapture`
- `cargo test -p plug -- --nocapture`
- live smoke:
  - `plug clients`
  - `plug status`
  - `plug auth status`
  - `plug doctor`
