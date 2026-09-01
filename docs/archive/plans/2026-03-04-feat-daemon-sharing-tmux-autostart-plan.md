---
title: "feat: Daemon Connection Sharing with tmux-style Auto-Start"
type: feat
status: completed
date: 2026-03-04
origin: docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md
---

# feat: Daemon Connection Sharing with tmux-style Auto-Start

## Overview

Make `plug connect` share upstream MCP server connections through a single daemon process. Today each `plug connect` spawns an independent Engine with independent copies of every upstream server — 8 clients = 80 server processes. This defeats plug's core value proposition of N:1 multiplexing.

The daemon already exists (`plug serve --daemon`) with Unix socket IPC, PID locking, auth tokens, and signal handling. What's missing is: (1) `plug connect` doesn't detect or proxy through it, and (2) the IPC protocol has no MCP message types.

After this work, `plug connect` will auto-start a daemon on first use (tmux model), proxy all MCP traffic through it, and share upstream connections across all clients.

## Problem Statement / Motivation

Rob runs 3-4 Claude Code + 3-4 Codex instances simultaneously. Without daemon sharing:
- Each `plug connect` spawns every upstream server independently
- 8 clients x 10 servers = 80 server processes (vs. 10 with sharing)
- Memory/CPU waste scales linearly with client count
- Some MCP servers (Slack, Workspace) have rate limits that get hit faster with N independent connections
- This makes plug **worse** than not using plug at all

The daemon infrastructure is 80% built but the critical 20% — actually routing MCP traffic through it — was never implemented. (See brainstorm: docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md)

## Proposed Solution

### tmux-style Auto-Start

```
plug connect
  │
  ├── Check: is daemon running? (socket exists + responds to Status)
  │   ├── YES → proxy MCP through daemon via IPC
  │   └── NO  → fork daemon in background, wait for readiness, then proxy
  │
  └── Client disconnects → daemon stays alive (other clients may be connected)
```

No `LaunchAgent`, no `systemd`, no manual `plug serve --daemon` required. The first `plug connect` starts the daemon. Subsequent clients share it. Daemon stays alive while any client is connected (plus a configurable grace period).

### IPC Protocol Extension

Add MCP message proxying to the existing length-prefixed JSON IPC protocol:

```
Client (stdio) ←→ plug connect (IPC proxy) ←→ daemon (Engine) ←→ upstream servers
```

New IPC variants:
- `McpRequest` — wraps any MCP JSON-RPC request (tools/list, tools/call, etc.)
- `McpResponse` — wraps the daemon's MCP JSON-RPC response
- `Register` — client announces itself (for client count tracking + type detection)
- `Deregister` — client disconnects cleanly

## Technical Approach

### Architecture

```
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│ Claude Code 1│  │ Claude Code 2│  │   Codex 1    │
│   (stdio)    │  │   (stdio)    │  │   (stdio)    │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       ▼                 ▼                 ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│plug connect 1│  │plug connect 2│  │plug connect 3│
│ (IPC proxy)  │  │ (IPC proxy)  │  │ (IPC proxy)  │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       └────────────┬────┴────────┬────────┘
                    │  Unix Socket │
                    ▼              ▼
              ┌────────────────────────┐
              │     plug daemon        │
              │  ┌──────────────────┐  │
              │  │     Engine       │  │
              │  │  ┌────────────┐  │  │
              │  │  │ServerManager│  │  │
              │  │  │ (shared)   │  │  │
              │  │  └────────────┘  │  │
              │  │  ┌────────────┐  │  │
              │  │  │ ToolRouter │  │  │
              │  │  │ (shared)   │  │  │
              │  │  └────────────┘  │  │
              │  └──────────────────┘  │
              └────────────────────────┘
                         │
          ┌──────────┬───┴───┬──────────┐
          ▼          ▼       ▼          ▼
      ┌───────┐ ┌───────┐ ┌───────┐ ┌───────┐
      │ Slack │ │Notion │ │ Supabase│ │ etc  │
      └───────┘ └───────┘ └───────┘ └───────┘
```

### Implementation Phases

#### Phase 1: IPC Protocol Extension (`plug-core/src/ipc.rs`)

Add MCP proxying variants to the existing IPC protocol.

**New IPC types:**

```rust
// plug-core/src/ipc.rs

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    // ... existing variants (Status, RestartServer, Reload, Shutdown) ...

    /// Register a new client session with the daemon.
    Register {
        /// Client type string from MCP initialize (e.g., "claude-code", "cursor")
        client_info: Option<String>,
    },

    /// Deregister a client session (clean disconnect).
    Deregister {
        session_id: String,
    },

    /// Proxy an MCP JSON-RPC request through the daemon's Engine.
    /// The daemon executes this against its shared ToolRouter and returns the result.
    McpRequest {
        session_id: String,
        /// Raw MCP JSON-RPC request body (we pass through, not interpret)
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcResponse {
    // ... existing variants (Status, Ok, Error) ...

    /// Registration acknowledgement with assigned session ID.
    Registered {
        session_id: String,
        /// Auth token for this session (scoped, not the daemon master token)
        session_token: String,
    },

    /// MCP JSON-RPC response from the daemon's Engine.
    McpResponse {
        payload: serde_json::Value,
    },
}
```

**Key design decisions:**
- `McpRequest` carries raw `serde_json::Value` — the daemon interprets and routes it, not the client proxy
- `Register` does NOT require the daemon master auth token — any process that can connect to the socket can register (socket permissions provide access control, matching tmux's model)
- `session_id` is assigned by the daemon (UUID), not the client
- `Deregister` uses the session_id for identification

**Files to modify:**
- `plug-core/src/ipc.rs` — add new variants, update `requires_auth()` and `extract_auth_token()`
- `plug-core/src/ipc.rs` tests — add round-trip tests for new variants

**Acceptance criteria:**
- [x]`IpcRequest::Register`, `Deregister`, `McpRequest` variants exist
- [x]`IpcResponse::Registered`, `McpResponse` variants exist
- [x]All new variants serialize/deserialize correctly (round-trip tests)
- [x]`Register` and `McpRequest` do NOT require daemon master auth token
- [x]`requires_auth()` returns false for Register/McpRequest (socket ACL is sufficient)
- [x]Debug impl redacts any sensitive fields

#### Phase 2: Daemon-side MCP Dispatch (`plug/src/daemon.rs`)

Handle the new IPC variants in the daemon's `dispatch_request` function.

**Client tracking:**

```rust
// plug/src/daemon.rs — new state in ConnectionContext or daemon-level

/// Tracks connected proxy clients for the daemon.
struct ClientRegistry {
    /// session_id → client metadata
    clients: DashMap<String, ClientSession>,
}

struct ClientSession {
    client_info: Option<String>,
    connected_at: Instant,
}
```

**MCP dispatch flow:**

1. `Register` → generate UUID session_id, insert into `ClientRegistry`, return `Registered { session_id }`
2. `Deregister` → remove from `ClientRegistry`
3. `McpRequest` → parse the JSON-RPC payload, route through the daemon's `ToolRouter`:
   - `tools/list` → call `tool_router.list_tools_for_client(client_type)`
   - `tools/call` → call `tool_router.route_tool_call(name, args)` (the existing proxy path)
   - Other methods → return appropriate response or error
4. Fix `clients: 0` hardcoded in `dispatch_request` → use `client_registry.clients.len()`
5. Fix `RestartServer` returning `NOT_IMPLEMENTED` → call `engine.restart_server(server_id)`

**Critical: MCP request handling must go through ToolRouter, not directly to ServerManager.** ToolRouter handles prefixing, health gates, circuit breakers, semaphores, and timeouts. This is the same path that `ProxyHandler` uses for direct stdio connections.

**Files to modify:**
- `plug/src/daemon.rs` — add `ClientRegistry`, update `dispatch_request`, fix `clients: 0`, fix `RestartServer`

**Acceptance criteria:**
- [x]`Register` creates a session and returns `Registered` with UUID
- [x]`Deregister` removes the session
- [x]`McpRequest` routes through `ToolRouter` for `tools/call`
- [x]`McpRequest` returns tool list for `tools/list`
- [x]`Status` returns actual client count from `ClientRegistry`
- [x]`RestartServer` calls `engine.restart_server()` instead of returning NOT_IMPLEMENTED
- [x]Clean disconnect (EOF) auto-deregisters the client
- [x]Connection drop (no Deregister) auto-deregisters via Drop/cleanup

#### Phase 3: IPC Proxy Client (`plug connect` rewrite) (`plug/src/main.rs`)

Replace `cmd_connect`'s independent Engine with an IPC proxy that forwards MCP traffic through the daemon.

**New flow for `cmd_connect`:**

```rust
async fn cmd_connect(config_path: Option<&PathBuf>) -> anyhow::Result<()> {
    // 1. Try to connect to existing daemon
    let daemon_stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            // 2. No daemon running — auto-start one
            auto_start_daemon(config_path).await?;
            // 3. Wait for daemon to be ready (poll socket with backoff)
            wait_for_daemon_ready().await?
        }
    };

    // 4. Register with daemon
    let session = register_with_daemon(&daemon_stream).await?;

    // 5. Bridge: stdio (MCP client) ←→ IPC (daemon)
    //    - Read JSON-RPC from stdin → wrap in McpRequest → send to daemon
    //    - Read McpResponse from daemon → unwrap → write to stdout
    run_ipc_bridge(daemon_stream, session).await?;

    // 6. Deregister on clean exit
    deregister_from_daemon(&daemon_stream, &session).await.ok();
    Ok(())
}
```

**The IPC bridge (`IpcProxyHandler`):**

Instead of running `ProxyHandler` (which talks directly to upstream servers), implement a new `IpcProxyHandler` that implements rmcp's `ServerHandler` trait but forwards everything over IPC:

```rust
/// MCP server handler that proxies all requests through the daemon via IPC.
struct IpcProxyHandler {
    ipc_reader: Mutex<OwnedReadHalf>,
    ipc_writer: Mutex<OwnedWriteHalf>,
    session_id: String,
}

#[rmcp::tool(tool_box)]
impl ServerHandler for IpcProxyHandler {
    async fn list_tools(&self) -> Vec<Tool> {
        // Send McpRequest{tools/list} via IPC, parse McpResponse
    }

    async fn call_tool(&self, name: String, args: Value) -> CallToolResult {
        // Send McpRequest{tools/call} via IPC, parse McpResponse
    }
}
```

This approach means the stdio transport layer (rmcp) handles JSON-RPC framing on the client side, while `IpcProxyHandler` translates to/from IPC messages. The daemon's `ToolRouter` does all the real work.

**Auto-start mechanism:**

```rust
async fn auto_start_daemon(config_path: Option<&PathBuf>) -> anyhow::Result<()> {
    // Fork a new process: `plug serve --daemon [--config path]`
    let mut cmd = tokio::process::Command::new(std::env::current_exe()?);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }
    // Detach: redirect stdin/stdout/stderr to /dev/null, new session
    cmd.stdin(std::process::Stdio::null())
       .stdout(std::process::Stdio::null())
       .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe { cmd.pre_exec(|| { libc::setsid(); Ok(()) }); }

    cmd.spawn()?;
    Ok(())
}

async fn wait_for_daemon_ready() -> anyhow::Result<tokio::net::UnixStream> {
    // Exponential backoff: 10ms, 20ms, 40ms, ... up to 5s total
    let mut delay = Duration::from_millis(10);
    let deadline = Instant::now() + Duration::from_secs(5);

    while Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await {
            // Verify daemon is ready by sending Status
            // (socket may accept connections before Engine is fully started)
            return Ok(stream);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(500));
    }
    anyhow::bail!("daemon failed to start within 5 seconds")
}
```

**Files to modify:**
- `plug/src/main.rs` — rewrite `cmd_connect`, add `auto_start_daemon`, `wait_for_daemon_ready`
- New file: `plug/src/ipc_proxy.rs` — `IpcProxyHandler` implementing `ServerHandler`

**Acceptance criteria:**
- [x]`plug connect` with no daemon running auto-starts daemon and connects
- [x]`plug connect` with daemon running connects without starting another
- [x]Multiple `plug connect` instances share the same daemon
- [x]`tools/list` returns the same tools through proxy as it would directly
- [x]`tools/call` executes through the daemon's shared upstream connections
- [x]Client disconnect sends `Deregister` (clean exit)
- [x]SIGINT/SIGTERM sends `Deregister` before exit
- [x]Auto-started daemon survives client disconnect (stays alive for other clients)

#### Phase 4: Client Type Detection Through Proxy

When a client connects via stdio, rmcp sends an `InitializeRequest` with `clientInfo.name`. Today `ProxyHandler` extracts this for client-aware tool filtering. Through the IPC proxy, we need to forward this info.

**Approach:** The `IpcProxyHandler` intercepts the MCP `initialize` request, extracts `clientInfo.name`, and sends it as part of the `Register` IPC message (the `client_info` field). The daemon uses this to apply per-client tool limits when handling `tools/list`.

**Files to modify:**
- `plug/src/ipc_proxy.rs` — extract `clientInfo` from initialize request
- `plug/src/daemon.rs` — store client type in `ClientSession`, use for tool filtering

**Acceptance criteria:**
- [x]Client type is forwarded to daemon during registration
- [x]Daemon applies correct tool limits per client (Cursor 40, Windsurf 100, etc.)
- [x]Unknown client types get the full tool list

#### Phase 5: Daemon Lifecycle Management

**Grace period shutdown:**

The daemon should stay alive for a configurable grace period after the last client disconnects, to avoid restart overhead if a client reconnects quickly.

```rust
// In daemon event loop, after client deregisters:
if client_registry.clients.is_empty() {
    // Start grace period timer (default: 60s)
    let grace = config.daemon_grace_period_secs;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(grace)).await;
        if client_registry.clients.is_empty() {
            cancel.cancel(); // Shut down daemon
        }
    });
}
```

**New config field:**

```toml
# config.toml
daemon_grace_period_secs = 60  # default: 60, set to 0 for immediate shutdown
```

**Files to modify:**
- `plug-core/src/config/mod.rs` — add `daemon_grace_period_secs` with default 60
- `plug/src/daemon.rs` — implement grace period shutdown logic

**Acceptance criteria:**
- [x]Daemon stays alive for grace period after last client disconnects
- [x]New client connecting during grace period cancels the shutdown timer
- [x]`daemon_grace_period_secs = 0` means immediate shutdown when last client leaves
- [x]Default (60s) works without config change

## Alternative Approaches Considered

### 1. HTTP proxy instead of IPC extension

Have `plug connect` proxy through the daemon's HTTP server (port 3282) instead of the Unix socket.

**Rejected because:**
- HTTP adds unnecessary overhead for local IPC (connection setup, headers, HTTP framing)
- Unix socket provides access control via file permissions (no auth token needed for registration)
- The IPC protocol already exists and works well; extending it is simpler than adding a second proxy path

### 2. Shared memory / mmap for tool cache

Share the tool list via shared memory so clients don't need IPC for `tools/list`.

**Rejected because:**
- Premature optimization — IPC `tools/list` is already fast (single frame round-trip)
- Shared memory adds complexity (synchronization, format versioning)
- Doesn't help with `tools/call`, which is the actual bottleneck

### 3. LaunchAgent / systemd service

Use OS service management instead of tmux-style auto-start.

**Deferred (not rejected):** This is a good future addition (`plug install-service`) but shouldn't be a prerequisite. The tmux model works immediately with zero setup. LaunchAgent is Workstream 2 material.

## System-Wide Impact

### Interaction Graph

- `plug connect` → `daemon::connect_to_daemon()` → `auto_start_daemon()` → `plug serve --daemon` (fork)
- `plug connect` → IPC `Register` → daemon creates `ClientSession` → returns `Registered`
- MCP client → stdio → `IpcProxyHandler.call_tool()` → IPC `McpRequest` → daemon `dispatch_request` → `ToolRouter.route_tool_call()` → upstream server → response bubbles back
- Client disconnect → `Deregister` → daemon removes `ClientSession` → if last client, start grace timer

### Error Propagation

- Daemon crash → `plug connect` gets socket EOF → should print error and exit (not hang)
- Upstream server failure → daemon's circuit breaker trips → `McpResponse` carries the MCP error → client sees normal MCP error
- IPC frame error → connection drops → auto-deregister
- Auto-start failure → `plug connect` prints "daemon failed to start" and exits with error code

### State Lifecycle Risks

- **Orphaned sessions:** Client crashes without sending `Deregister` → daemon must detect EOF on the IPC connection and auto-deregister. This is handled by the connection handler's cleanup on stream close.
- **Stale daemon:** Daemon crashes but PID file/socket remain → `connect_to_daemon()` already handles this (tries to connect, fails, removes stale socket). `auto_start_daemon` checks PID lock.
- **Race: two clients auto-start simultaneously:** PID file locking prevents this — second `plug serve --daemon` fails with "already running", and the second client retries connecting to the first daemon.

### API Surface Parity

- `plug connect` (stdio) — will use IPC proxy instead of direct Engine
- `plug serve` (HTTP) — unchanged, still runs its own Engine (HTTP clients connect directly)
- `plug tui` — unchanged, still creates its own Engine
- `plug status` — already queries daemon via IPC, will now show real client count
- `plug daemon stop` — unchanged
- `plug tool list` — unchanged (starts temporary Engine)

### Integration Test Scenarios

1. **Multi-client sharing:** Start daemon, connect 3 clients, verify all see the same tools, call a tool from each, verify only 1 upstream connection exists
2. **Auto-start race:** Two `plug connect` processes start simultaneously with no daemon — exactly one daemon starts, both clients connect successfully
3. **Daemon crash recovery:** Kill daemon process, verify connected clients get EOF, verify next `plug connect` auto-starts a new daemon
4. **Grace period:** Connect client, disconnect, verify daemon stays alive for grace period, reconnect before timeout, verify no restart
5. **Client type forwarding:** Connect as "claude-code" and "cursor", verify each gets appropriate tool filtering

## Acceptance Criteria

### Functional Requirements

- [x]`plug connect` auto-starts daemon if not running
- [x]`plug connect` proxies MCP traffic through daemon when daemon is running
- [x]Multiple clients share upstream server connections (N clients = 1 set of upstream connections)
- [x]`plug status` shows actual connected client count
- [x]`plug serve --daemon` still works as explicit start (backward compatible)
- [x]`plug daemon stop` still works
- [x]Client disconnect is detected and session cleaned up
- [x]Daemon grace period prevents unnecessary restart/shutdown cycles
- [x]Client type detection works through proxy (tool filtering per client)
- [x]`RestartServer` IPC command works (not NOT_IMPLEMENTED)

### Non-Functional Requirements

- [x]IPC round-trip for `tools/call` adds < 5ms overhead vs direct
- [x]Auto-start completes within 5 seconds (including upstream server startup)
- [x]No `unsafe` code except the `pre_exec` for `setsid()` (fork detachment)
- [x]All new code has unit tests
- [x]No breaking changes to existing CLI interface

### Quality Gates

- [x]`cargo test` passes
- [x]`cargo clippy -- -D warnings` clean
- [x]`cargo fmt --check` clean
- [x]Manual test: 3+ concurrent `plug connect` instances sharing daemon

## Dependencies & Prerequisites

- **rmcp 1.0.0** — current version supports `ServerHandler` trait needed for `IpcProxyHandler`
- **No new crates required** — all IPC infrastructure (Unix sockets, length-prefixed framing, serde_json) already exists
- **libc crate** — needed for `setsid()` in fork detachment (or use `nix` crate). Check if already in dependency tree via tokio.

## Risk Analysis & Mitigation

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| IPC overhead too high for tool calls | Medium | Low | Benchmark early; IPC is local Unix socket, should be < 1ms |
| rmcp ServerHandler trait doesn't support custom routing | High | Medium | Read rmcp source to verify trait is flexible enough; fallback to raw JSON-RPC handling |
| Fork detachment doesn't work on all platforms | Medium | Low | Only use `setsid()` on Unix; macOS and Linux both support it |
| Auto-start race condition | High | Medium | PID file locking + retry with backoff; test with concurrent starts |
| Daemon memory growth with many sessions | Low | Low | Sessions are lightweight (UUID + metadata); thousands would be fine |

## Future Considerations

- **`plug install-service`** — LaunchAgent/systemd integration for persistent daemon (Workstream 2)
- **Notification forwarding** — MCP notifications (list_changed, progress, cancelled) from upstream servers should be forwarded to all connected clients via IPC push messages
- **HTTP client sharing** — `plug serve` (HTTP mode) could also connect to the daemon instead of running its own Engine
- **Remote daemon** — IPC over TCP socket for connecting to daemon on another machine (not planned, but the protocol would support it)

## Sources & References

### Origin

- **Brainstorm document:** [docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md](docs/brainstorms/2026-03-04-daemon-sharing-and-project-audit-brainstorm.md) — Key decisions: tmux model for auto-start, fix daemon sharing first, fix everything properly with honest documentation

### Internal References

- Daemon infrastructure: `plug/src/daemon.rs` (socket, PID lock, auth, IPC handler)
- IPC protocol: `plug-core/src/ipc.rs` (existing 4 variants, framing, auth)
- Engine API: `plug-core/src/engine.rs` (ServerManager, ToolRouter, events, config)
- ProxyHandler: `plug-core/src/proxy/mod.rs` (ToolRouter.route_tool_call, ServerHandler impl)
- Connect command: `plug/src/main.rs:253` (current cmd_connect — spawns independent Engine)
- Client count bug: `plug/src/daemon.rs:448` (hardcoded `clients: 0`)
- RestartServer bug: `plug/src/daemon.rs:460` (returns NOT_IMPLEMENTED)

### Related Work

- PR #8: Fixed SSRF, timeouts, circuit breaker, HTTPS (merged)
- Issue #7: Original bug report that exposed these architectural gaps
