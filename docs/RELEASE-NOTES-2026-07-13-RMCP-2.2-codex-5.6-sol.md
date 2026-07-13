# RMCP 2.2 upgrade - Codex 5.6 sol

This release branch upgrades Plug to RMCP 2.2.0, the latest stable release of
the official Rust SDK available on July 13, 2026. Until the branch is merged,
it exists off-main. This is an SDK and compatibility upgrade, not a change to
the MCP version clients see: Plug still negotiates MCP `2025-11-25`.

No configuration migration is required. Existing stdio, Streamable HTTP,
daemon IPC, OAuth, Tasks, resources, prompts, completion, elicitation,
sampling, logging, and notification behavior remains available.

## What improves

RMCP 2.2 includes the SDK's latest `2025-11-25` conformance fixes, stricter
S256 PKCE behavior, safer OAuth token and redirect handling, cancellation
corrections, and Streamable HTTP recovery fixes. Plug now builds directly on
the SDK's spec-aligned content, resource, prompt, task, and elicitation models
instead of RMCP 1.x compatibility types.

Cancellation is also safer at Plug's routing boundary. RMCP 2.2 correctly
models `notifications/cancelled` messages whose `requestId` is absent. Plug
accepts those messages but does not guess which request they refer to, so an
anonymous cancellation cannot stop an unrelated active call.

## What does not change

- The negotiated MCP revision remains `2025-11-25`.
- Client configuration and server definitions do not change.
- All three downstream paths remain supported: stdio, Streamable HTTP, and
  daemon IPC.
- Tasks keep their existing method names and wire shapes.
- Logging, Roots, and Sampling remain enabled for MCP `2025-11-25`, even
  though RMCP marks them deprecated in anticipation of future SEP-2577 work.
- The announced July 28 stateless MCP revision and its newer Tasks extension
  are not included in this upgrade. Downstream stdio and daemon-IPC clients
  requesting that revision are rejected instead of being told Plug supports it.

## Build compatibility

Source builds still require Rust 1.88 or newer. RMCP is pinned exactly to
`2.2.0`. `sse-stream` is pinned exactly to `0.2.4`, the release providing the
API RMCP 2.2 expects, so a fresh locked build cannot resolve the incompatible
older 0.2.2 API.

## Verification

The migration adds direct regression coverage for RMCP 2.2 resource-link JSON
and cancellation without a request id. Existing protocol-version, Tasks,
elicitation, sampling, and daemon IPC suites continue to exercise MCP
`2025-11-25` behavior. The release gate covers the complete workspace tests,
Clippy with warnings denied, formatting, Rust 1.88 compilation, RustSec
advisories, todo-status consistency, and clean diffs.
