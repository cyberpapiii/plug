# Research Breadcrumbs

This file now tracks the questions that are still genuinely open after the merged Phase 1-3 work.

## Current Open Questions

No open research questions block the current roadmap or the current production-ready bar.

## Future Exploration Only

These may matter for a future roadmap, but they are not blockers for the current product:

### F1: What is the smallest honest post-`v0.2.0` stateless tranche?

The `SessionStore` seam and stateless notes now exist, but the first real stateless implementation
slice is still undefined.

### F2: How far should meta-tool security go?

`plug` now has meta-tool mode and tool-definition drift detection, but it does not yet implement
heavier security patterns such as quarantine or approval flows for newly changed tools.

### F3: What is the right next recovery-proof slice after daemon continuity?

Daemon continuity is now covered end to end. Future work could deepen recovery proof beyond the
current stdio-over-IPC path.

### F4: Which post-June-2026 MCP feature should `plug` tackle first?

Tasks and related future-facing MCP features remain future roadmap questions, not current blockers.

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
