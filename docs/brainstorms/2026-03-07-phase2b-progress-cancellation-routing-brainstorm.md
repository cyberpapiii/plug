# Brainstorm: Phase 2B Progress and Cancellation Routing

**Date**: 2026-03-07
**Status**: Ready for planning

## What We're Building

The next implementation tranche after Phase 2A notification infrastructure: preserve downstream progress/cancellation identity on tool calls, forward cancellation from downstream clients to upstream servers, and relay upstream progress notifications back to the correct downstream stdio or HTTP client.

This tranche is intentionally narrower than “all remaining Phase 2.” It focuses on:

- downstream `notifications/cancelled` handling
- upstream `notifications/progress` handling
- `progressToken` preservation/passthrough
- request/session mapping strong enough to route those signals coherently

It does **not** include resources/prompts forwarding, pagination, or broader capability synthesis yet.

## Why This Approach

Phase 2A established the exact substrate this work depends on:

- upstream server notifications are no longer dropped
- `ToolRouter` owns a dedicated protocol-notification bus
- stdio and HTTP both have downstream notification delivery paths
- `DownstreamCallContext` preserves transport + request identity for active calls

The next highest-value missing protocol behavior is now the bidirectional control plane:

- downstream users should be able to cancel long-running tool calls
- upstream progress notifications should reach the correct downstream caller
- the correlation layer should become usable routing state, not just retained call metadata

Without this tranche, Phase 2A is only half the story. We can signal list changes globally, but we still cannot route per-request progress or cancellation correctly.

## Key Decisions

- **Scope narrowly around tool-call control flow.**
  Do not broaden this into resources/prompts or pagination.

- **Treat cancellation as routing, not transport shutdown.**
  This work should forward `notifications/cancelled` to the correct upstream request. It should not invent new local cancellation semantics beyond what the protocol already defines.

- **Preserve downstream `progressToken` rather than inventing a new one.**
  If the downstream request supplies a token, pass it upstream and use it as the primary progress routing key.

- **Support both request-ID-based and progress-token-based correlation.**
  Cancellation is keyed by request ID. Progress is keyed by progress token. The correlation layer needs both.

- **Keep transport parity exact.**
  Both stdio and HTTP downstreams should receive `notifications/progress` and be able to send `notifications/cancelled`.

- **Keep notification routing separate from `EngineEvent`.**
  Per-request progress/cancelled routing belongs on the protocol-notification path established in Phase 2A, not on the observability bus.

## Resolved Questions

- **Should this start only after Phase 2A is merged?** Yes
- **Should progress/cancellation be bundled with resources/prompts?** No
- **Is the current `DownstreamCallContext` enough on its own?** No
- **Should `progressToken` be synthesized by plug when absent?** No, not in this tranche

## Open Questions

- What is the smallest active-call record that cleanly supports both request-ID-based cancellation and progress-token routing?
- Should upstream progress fan-out remain transport-agnostic via `ProtocolNotification`, or should progress use direct targeted delivery while list-changed stays broadcast?
- How should HTTP handle progress/cancelled when the SSE stream is not currently attached to the session?

## Next

Write a focused plan for:

1. active-call record expansion for request ID + progress token
2. downstream cancellation forwarding for stdio and HTTP
3. upstream progress relay to the correct downstream stdio or HTTP client

Everything else stays deferred until this request-scoped routing works.
