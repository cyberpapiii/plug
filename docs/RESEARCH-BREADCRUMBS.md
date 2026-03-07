# Research Breadcrumbs

This file now tracks the questions that are still genuinely open after the merged Phase 1-3 work.

## Current Open Questions

### R1: What is the smallest honest post-`v0.2.0` stateless tranche?

The `SessionStore` seam and stateless notes now exist, but the first real stateless implementation
slice is still undefined. The next question is where to start without overcommitting:

- stateless discovery only
- stateless request validation path
- full stateless downstream HTTP handling

### R2: How far should meta-tool security go?

`plug` now has meta-tool mode and tool-definition drift detection, but it does not yet implement
heavier security patterns such as quarantine or approval flows for newly changed tools.

### R3: What is the right next recovery-proof slice after daemon continuity?

Daemon continuity is now covered end to end. The next recovery question is whether to prioritize:

- upstream restart recovery proof
- mixed transport continuity
- more aggressive failure choreography under load

### R4: Which post-June-2026 MCP feature should `plug` tackle first?

Tasks and related future-facing MCP features are still deferred. The next roadmap decision should be
driven by actual client adoption rather than speculative implementation.

## Resolved Questions

These are no longer open in the main codebase:

- rmcp client/server proxy composition
- notification forwarding feasibility
- progress/cancellation routing
- resources/prompts forwarding
- pagination
- meta-tool mode viability
- daemon continuity at the IPC boundary
- where the future session abstraction seam belongs
