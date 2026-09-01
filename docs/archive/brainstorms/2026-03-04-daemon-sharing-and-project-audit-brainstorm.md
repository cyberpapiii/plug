# Brainstorm: Daemon Sharing, Auto-Start, and Project Integrity Audit

**Date:** 2026-03-04
**Status:** Approved
**Author:** Rob Dezendorf + Claude

## What We're Building

Two workstreams, in priority order:

### Workstream 1: Daemon Sharing + Auto-Start (Critical)

Make `plug connect` actually share upstream connections through a single daemon process. Today each `plug connect` spawns independent copies of every server — 8 clients = 80 server processes. This defeats plug's core value proposition.

**Auto-start model:** tmux-style. `plug connect` checks for a running daemon. If none exists, it forks one in the background. Clients proxy MCP tool calls through the daemon's shared connections. No LaunchAgent, no manual start — it just works.

### Workstream 2: Full Integrity Audit + Honest Documentation (Important)

The previous agent team claimed all 5 phases were done. An audit found 12 significant items that are incomplete, stubbed, or entirely missing. Fix everything properly and update PLAN.md to honestly reflect what's done vs. not.

## Why This Approach

Rob runs 3-4 Claude Code + 3-4 Codex instances simultaneously. Without daemon sharing, plug creates N copies of every upstream server, which is worse than not using plug at all. The tmux auto-start model means zero manual steps — the first client to connect starts the daemon, subsequent clients share it.

The audit cleanup is needed because the project's documentation is lying about its state. Features marked as done are not done. This creates compounding surprises (like we already hit with HTTPS, SSRF, and timeouts).

## Key Decisions

1. **tmux model for daemon lifecycle** — `plug connect` auto-starts daemon if not running. No LaunchAgent needed. Daemon stays alive while any client is connected.

2. **IPC protocol extension for MCP proxying** — The daemon IPC currently only supports Status/Reload/Shutdown. It needs a new message type for proxying actual MCP tool calls (and tools/list) through the daemon's shared Engine.

3. **Fix everything properly** — All 12 audit findings get fixed. PLAN.md gets updated to be honest. No more "claimed done but actually stubbed."

4. **Priority order** — Daemon sharing first (unblocks Rob's daily workflow), then audit cleanup (prevents future surprises).

## Audit Findings (Full List)

### Already Fixed (PR #8, merged)
- HTTPS broken (no TLS feature on reqwest)
- SSRF blocking user-configured local servers
- Single 30s timeout for startup + tool calls
- Timeouts tripping circuit breaker

### Critical — Daemon Not Actually Sharing
- `plug connect` spawns independent Engine every time (no daemon detection)
- IPC has no MCP proxy protocol (only Status/Reload/Shutdown)
- Client count hardcoded to 0 in daemon status
- IPC RestartServer returns NOT_IMPLEMENTED despite Engine supporting it

### Incomplete Features (claimed done)
- Notification forwarding (list_changed, progress, cancelled) — entirely missing
- `.localhost` subdomain routing — entirely missing
- Legacy SSE server (/sse endpoint) — entirely missing
- HTTP/2 support — entirely missing
- `resources/read` routing — returns empty
- `prompts/get` routing — returns empty

### Missing Distribution
- No Homebrew tap (README claims it exists)
- No shell installer (README claims it exists)
- No real integration tests (mock server binary exists but is never used)

### Open P2 Issues (from todos/)
- Semaphore acquisition has no timeout (024)
- SecretString display leaks value (025)
- Stdio timeout orphaned responses (026)
- Health check compounds circuit breaker (027)
- Silent startup failures (028)

## Open Questions

None — decisions are clear. Priority: daemon sharing first, then systematic audit cleanup.

## Next Steps

1. `/ce:plan` for Workstream 1 (daemon sharing + tmux auto-start)
2. `/ce:plan` for Workstream 2 (audit cleanup) after Workstream 1 ships
