# Current Plan

This document tracks the current product state and the next remaining work after the merged Phase
1-3 tranches.

## Current State

`plug` has completed the major stabilization and protocol-surface work that used to sit behind the
old `v0.1` plan:

- stabilization and truth fixes
- notification forwarding
- progress and cancellation routing
- resources/prompts forwarding
- pagination
- capability synthesis
- meta-tool mode
- end-to-end transport coverage
- daemon continuity recovery
- session-store abstraction seam and stateless design prep

## What Exists Today

The current product shape is:

- `plug connect` for stdio downstream clients
- `plug serve` for Streamable HTTP downstream clients, with optional HTTPS via configured cert/key paths
- shared upstream routing through `Engine`, `ServerManager`, and `ToolRouter`
- daemon-backed local sharing with reconnecting IPC proxy sessions
- targeted notification fan-out to stdio and HTTP
- meta-tool mode as an opt-in reduced discovery surface

## Remaining Work Before `v0.2.0`

The main remaining release-closeout work is documentation and release hygiene:

- bring the tracked operating docs in sync with the merged code
- update the risk register to current remaining risks
- reduce the research breadcrumb list to the still-open questions
- choose and create the `v0.2.0` tag after merge

## Post-`v0.2.0` Work

Likely next roadmap areas after the release boundary:

- additional upstream restart / recovery proof
- deeper stateless downstream design or implementation
- broader ecosystem-forward work such as Tasks support once the spec direction settles
