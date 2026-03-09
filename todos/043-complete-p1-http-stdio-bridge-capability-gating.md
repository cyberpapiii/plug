---
status: complete
priority: p1
issue_id: "043"
tags: [code-review, security, architecture, agent-native]
dependencies: []
---

# HttpBridge and StdioBridge lack capability gating for reverse requests

## Problem Statement

The `DaemonBridge` explicitly checks `client_registry.capabilities()` for elicitation/sampling support before forwarding reverse requests, returning immediate clear errors. The `HttpBridge` and `StdioBridge` perform no such check:

- **HttpBridge**: Sends the reverse request via SSE regardless of client capabilities. If the HTTP client did not advertise elicitation support, the request hangs indefinitely (elicitation has `None` timeout) or times out after 60s (sampling). No error is returned to the upstream server during the wait.
- **StdioBridge**: Calls `peer.create_elicitation()` / `peer.create_message()` directly. rmcp will return a method-not-found error, but the error message is less clear than the DaemonBridge's explicit gate.

This asymmetry means the same agent client gets three different failure behaviors for the same root cause depending on which transport it uses.

Flagged independently by: security-sentinel (HIGH), architecture-strategist, agent-native-reviewer (CRITICAL), code-simplicity-reviewer (observation).

## Findings

- `plug-core/src/http/server.rs:64-113`: HttpBridge has no capability check before SSE send
- `plug-core/src/proxy/mod.rs:2607-2637`: StdioBridge has no capability check, delegates to rmcp
- `plug/src/daemon.rs:548-603`: DaemonBridge has correct explicit gating pattern
- `plug-core/src/http/server.rs:635-638`: HTTP InitializeRequest handler only stores roots capability, discards elicitation/sampling
- HTTP `pending_client_requests` entry leaks if client never responds (no capability check + no timeout)

## Proposed Solutions

### Option A: Add gating to each bridge individually (Recommended)

For HttpBridge: store `ClientCapabilities` per session in HttpState (alongside `roots_capable_sessions`), check in bridge methods before sending.

For StdioBridge: store capabilities from `InitializeRequestParams` in `ProxyHandler`, check in bridge methods.

- Pros: Minimal architectural change, follows DaemonBridge's proven pattern
- Cons: Gating logic duplicated across 3 bridges
- Effort: Small
- Risk: Low

### Option B: Move gating into ToolRouter (centralized)

Add capability reporting to bridge registration. `register_downstream_bridge()` accepts capabilities alongside the bridge. `create_elicitation_from_upstream()` / `create_message_from_upstream()` check capabilities before dispatching.

- Pros: Single check point, consistent behavior across all transports
- Cons: Requires storing capabilities in ToolRouter alongside bridge references
- Effort: Medium
- Risk: Low

## Recommended Action

_To be filled during triage_

## Technical Details

- **Affected files**: `plug-core/src/http/server.rs`, `plug-core/src/proxy/mod.rs`
- **Components**: HttpBridge, StdioBridge, ToolRouter
- **New state needed**: Per-session or per-bridge capability storage

## Acceptance Criteria

- [ ] HttpBridge rejects elicitation if client did not advertise `capabilities.elicitation`
- [ ] HttpBridge rejects sampling if client did not advertise `capabilities.sampling`
- [ ] StdioBridge performs equivalent check (or delegates to centralized check)
- [ ] Error messages match DaemonBridge format: "client {id} does not support {capability}"
- [ ] Test: HTTP client without elicitation capability triggers tool that sends elicitation, gets clean error

## Work Log

| Date | Action | Learnings |
|------|--------|-----------|
| 2026-03-08 | Created from CE review (6 agents) | 4 of 6 agents independently flagged this as the highest-priority finding |

## Resources

- DaemonBridge gating pattern: `plug/src/daemon.rs:548-558`
- Institutional learning: `docs/solutions/integration-issues/resource-subscribe-forwarding-lifecycle-20260307.md` (three-transport parity checklist)
