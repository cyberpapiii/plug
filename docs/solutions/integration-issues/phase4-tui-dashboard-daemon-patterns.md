---
title: "Phase 4 TUI Dashboard and Daemon Mode Patterns"
category: integration-issues
tags: [rust, ratatui, tui, daemon, ipc, unix-socket, broadcast, arcswap, engine, security]
module: TUI/Daemon
symptom: "Multiple patterns and learnings from Phase 4 implementation"
root_cause: "Architectural decisions and implementation patterns for TUI rendering, daemon IPC, security, and event-driven state management"
date: 2026-03-04
---

# Phase 4 TUI Dashboard and Daemon Mode Patterns

## Problem

Phase 4 introduced two major subsystems — a live TUI dashboard and a background daemon — that both need to interact with the core multiplexer engine. This creates several intersecting challenges:

1. The TUI must render a live view of engine state (servers, tools, clients, circuit breakers) without blocking the async runtime or causing unnecessary redraws.
2. The daemon must accept commands from CLI clients over a Unix socket with proper authentication, framing, and platform-specific security.
3. The broadcast event bus must fan out events efficiently to multiple TUI tabs and widgets without excessive cloning.
4. Ownership boundaries between the async Engine and the synchronous TUI rendering loop must be carefully managed to avoid holding references across await points.

## Investigation

### Engine Struct as Unified Runtime

The Engine struct owns all core components and provides a query-only public API:

```rust
pub struct Engine {
    server_manager: ServerManager,
    tool_router: ToolRouter,
    config: Arc<ArcSwap<Config>>,
    cancel: CancellationToken,
    tasks: TaskTracker,
    event_tx: broadcast::Sender<EngineEvent>,
}
```

All fields are private. External consumers (TUI, CLI, daemon handler) interact through methods like `engine.snapshot()`, `engine.server_status(name)`, `engine.tool_count()`.

For transferring state to the TUI (which cannot hold an `&Engine` across async boundaries), `EngineSnapshot` provides a clone-able, owned representation:

```rust
pub struct EngineSnapshot {
    pub servers: Vec<ServerState>,
    pub tools: Vec<ToolEntry>,
    pub clients: Vec<ClientInfo>,
    pub circuit_states: HashMap<String, CircuitState>,
}
```

### Broadcast Event Bus with Arc<str>

The event bus uses `tokio::sync::broadcast` with 11 event variants. String fields use `Arc<str>` instead of `String` to make broadcast fan-out O(1) per clone instead of O(n) where n is the string length:

```rust
#[derive(Clone, Debug)]
pub enum EngineEvent {
    ServerHealthChanged { name: Arc<str>, status: HealthStatus },
    ToolCallStarted { tool: Arc<str>, client: Arc<str>, request_id: u64 },
    // ... all string fields use Arc<str>
}
```

When the broadcast channel has multiple subscribers (e.g., TUI main loop, TUI status bar widget, metrics collector), each `send()` clones the event for every subscriber. With `Arc<str>`, this clone is a pointer copy plus an atomic increment rather than a heap allocation and memcpy.

### TUI Event Batching and Dirty-Flag Rendering

The TUI drains all pending events before rendering, and only renders when something actually changed:

```rust
// In the TUI event loop:
loop {
    tokio::select! {
        event = event_rx.recv() => {
            match event {
                Ok(e) => {
                    app.apply_event(e);
                    while let Ok(e) = event_rx.try_recv() {
                        app.apply_event(e);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    app.full_refresh(engine.snapshot());
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    if app.dirty {
        terminal.draw(|f| app.render(f))?;
        app.dirty = false;
    }
}
```

### Confirmed Action Pipeline

The TUI `App` cannot hold `&Engine` because the render loop crosses async boundaries. Instead, a `PendingAction` enum bridges the gap:

```rust
pub enum PendingAction {
    RestartServer(String),
    // ...
}
```

User presses key → `App::handle_input()` sets `PendingAction` → event loop picks it up where it has `&mut Engine` access.

### Length-Prefixed JSON IPC

Daemon-to-CLI communication uses length-prefixed JSON (4-byte big-endian u32 + payload). NDJSON breaks when JSON payloads contain embedded newlines in string values.

```rust
const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024; // 4MB
```

The 4MB check happens before `vec![0u8; len as usize]` allocation, preventing memory exhaustion from malicious length prefixes.

### Auth Token with Constant-Time Comparison

256-bit token from OsRng, hex-encoded, constant-time comparison via `subtle::ConstantTimeEq`. Token file created with 0600 permissions.

### Socket Path Security

Never use `/tmp` — use platform-specific user-owned directories via the `directories` crate (not `dirs` — different crate, different API). Set 0700 on runtime directory.

### PID File Locking

`fs2::FileExt::try_lock_exclusive()` for advisory locking. PID written AFTER socket bind (socket bind is the real exclusion mechanism). Lock auto-releases when process exits, even on crash.

### Broadcast Lagged Recovery

When the TUI receiver falls behind, reconcile from Engine's authoritative state rather than trying to replay missed events:

```rust
Err(broadcast::error::RecvError::Lagged(n)) => {
    app.full_refresh(engine.snapshot());
}
```

## Root Cause

These patterns arise from fundamental tensions:
- **Async vs. sync boundary**: TUI render is sync, engine is async → PendingAction indirection
- **Fan-out cost**: Broadcast clones every message for every subscriber → Arc<str> essential
- **Security in userspace**: No root → file permissions and user-owned directories
- **Correctness under load**: Event-driven UIs must handle event loss → full-state reconciliation

## Solution

| Concern | Pattern | Why |
|---------|---------|-----|
| Engine access | Private fields + query API + EngineSnapshot | Prevents mutation from UI code |
| Event strings | `Arc<str>` | O(1) broadcast clone |
| TUI rendering | Dirty flag + event batching | No wasted redraws |
| TUI layout | Pure function over `Rect` | Testable without terminal |
| TUI actions | PendingAction enum | Bridges sync/async boundary |
| IPC framing | Length-prefixed JSON | Handles embedded newlines |
| Auth | 256-bit token + constant-time compare | Prevents timing attacks |
| Socket security | User-owned directories, 0600 perms | Prevents symlink attacks |
| PID locking | fs2 advisory lock + socket bind | Crash-safe exclusion |
| Event loss | Full refresh from snapshot | Always correct reconciliation |
| Visual feedback | Flash counter with tick decay | Brief highlight on state change |

## Prevention

1. **Always use `Arc<str>` for string fields in any type that passes through a broadcast channel.**
2. **Never place `/tmp` in any socket or credential path.** Use `directories` crate.
3. **When designing event-driven UIs, always implement a full-state reconciliation path.** Snapshot refresh > event replay.
4. **Use length-prefixed framing for any IPC that carries structured data.** NDJSON only safe when no embedded newlines.
5. **Keep TUI App state separate from Engine state.** App is a view-model, not authoritative state.
6. **Test layout logic as pure functions.** `compute_layout(Rect) -> LayoutMode` needs zero terminal dependencies.
7. **For daemon auth tokens, always use constant-time comparison.** Defense in depth matters.
8. **Write PID files after resource acquisition, not before.** Socket bind is the real exclusion.
9. **Use `directories` crate, not `dirs`.** Different crates with different APIs.

## Related

- `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md` — ArcSwap, CancellationToken patterns
- `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md` — DashMap vs ArcSwap split, lock-free circuit breakers
- `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md` — rmcp API gotchas
- `docs/ARCHITECTURE.md` — Component design, concurrency model
- `docs/PLAN.md` — Phase 4 implementation plan
