# Brainstorm: Post-v0.2 Upstream Restart Recovery Proof

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The first post-`v0.2.0` tranche is a focused proof of upstream restart recovery for stdio-backed
servers.

This tranche focuses on:

- proving that a real upstream stdio server crash is recoverable
- verifying the runtime reconnects and restores usable tool traffic
- doing this through an end-to-end test rather than another helper-only unit test

This tranche does **not** include:

- broader mixed-transport continuity
- stateless downstream implementation
- Tasks support
- tool quarantine / approval flows

## Why This Approach

The merged Phase 3 work already proved:

- downstream daemon continuity
- end-to-end stdio and HTTP proxy paths
- notification/progress/cancellation/resource/prompt parity

The next missing proof is on the upstream side. `plug` already has both reactive reconnect in the
tool-call path and proactive reconnect in the health path, but there is still no strong test that a
real upstream stdio crash/restart is actually survivable.

That makes upstream restart recovery the smallest honest next runtime tranche.

## Key Decisions

- **Treat this as a proof tranche first.**
  Start with end-to-end evidence and only change runtime code if the test exposes a real gap.

- **Use a wrapper-script harness around the existing mock MCP server.**
  That is the simplest way to force a first-run crash and a second-run healthy restart.

- **Prefer reactive reconnect proof over a larger proactive-health choreography first.**
  The reactive path is smaller, easier to make deterministic, and still proves the core recovery
  story.

- **Keep the acceptance bar simple: tool traffic becomes usable again without manual intervention.**

## Resolved Questions

- **Should this be a new feature tranche or a proof tranche?** Proof tranche
- **Should this use the existing mock server harness?** Yes
- **Should this start with reactive reconnect rather than broader recovery choreography?** Yes

## Open Questions

None. The scope is narrow enough to proceed directly to planning.

## Next

Write a focused plan for:

1. a crash-then-restart wrapper around `mock-mcp-server`
2. one end-to-end reactive upstream recovery test
3. runtime fixes only if the proof exposes a real gap
