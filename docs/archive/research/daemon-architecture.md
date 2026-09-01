# Daemon Architecture Research: Shared Upstream Sessions for plug

**Research for**: Q4 (Multiple Concurrent `plug connect` Instances) and E16 (Daemon vs Embedded Architecture)
**Date**: 2026-03-03
**Status**: Design recommendation ready for implementation

---

## Table of Contents

1. [Problem Statement](#problem-statement)
2. [Prior Art: tmux](#prior-art-tmux)
3. [Prior Art: Docker](#prior-art-docker)
4. [Prior Art: zellij](#prior-art-zellij)
5. [Comparative Analysis](#comparative-analysis)
6. [Design: Daemon Lifecycle](#design-daemon-lifecycle)
7. [Design: Unix Socket IPC Protocol](#design-unix-socket-ipc-protocol)
8. [Design: Failure Modes & Recovery](#design-failure-modes--recovery)
9. [Embedded vs Daemon Tradeoff](#embedded-vs-daemon-tradeoff)
10. [Recommended Architecture](#recommended-architecture)
11. [Migration Path: Phase 1 to Phase 4](#migration-path-phase-1-to-phase-4)
12. [Open Questions](#open-questions)

---

## Problem Statement

plug is an MCP multiplexer. Multiple AI clients (Claude Code, Cursor, Gemini CLI, etc.) each invoke `plug connect` as a stdio bridge. Each invocation is a separate OS process with its own stdin/stdout. The core question:

**How do N independent `plug connect` processes share a single set of M upstream MCP servers?**

Without sharing, each `plug connect` spawns its own copy of every upstream server. With 5 clients and 8 servers, that is 40 child processes instead of 8. This wastes memory, causes port conflicts (for HTTP-based servers), and creates session isolation issues (subscriptions, state).

```
WITHOUT SHARING (N x M):                WITH SHARING (N + M):

Claude Code 1 ─┬─ github                Claude Code 1 ─┐
               ├─ filesystem                            │
               └─ postgres              Claude Code 2 ─┤
                                                        │        ┌─ github
Claude Code 2 ─┬─ github               Cursor ─────────┼─ DAEMON ┼─ filesystem
               ├─ filesystem                            │        └─ postgres
               └─ postgres              Gemini CLI ─────┘

Cursor ────────┬─ github                (4 + 3 = 7 processes)
               ├─ filesystem
               └─ postgres

(3 x 3 = 9 server instances, 12 total processes)
```

---

## Prior Art: tmux

### Architecture Overview

tmux uses a strict client-server architecture where a single server process manages all state (sessions, windows, panes, running programs), and thin client processes connect to the server to render and interact.

```
                                    ┌──────────────────────────────┐
                                    │        tmux server           │
 Terminal 1 ─── tmux client 1 ──┐   │                              │
                                ├──►│  Session 1: [Window 1] [W 2] │
 Terminal 2 ─── tmux client 2 ──┤   │  Session 2: [Window 1]       │
                                │   │                              │
 Terminal 3 ─── tmux client 3 ──┘   │  Event loop (libevent)       │
                                    │  PTY management              │
                     Unix socket    │  Buffer management           │
                /tmp/tmux-$UID/     └──────────────────────────────┘
                    default
```

### Server Auto-Start

tmux starts the server transparently on first use. When you run `tmux new-session`, the client attempts to connect to the socket. If the socket does not exist, the client forks a server process, which creates the socket and enters its event loop. The client then connects. This is all invisible to the user -- there is no separate "start the server" step.

Key implementation detail: the server is started from `server_start()`, which is called only when an attempt to connect to an existing server fails. The `-N` flag can prevent server auto-start for commands that would normally trigger it.

### Socket Location & Naming

- Default path: `/tmp/tmux-$UID/default`
- Overridable via `TMUX_TMPDIR` environment variable
- The `-L` flag allows named sockets for multiple independent servers (e.g., `tmux -L moo` creates `/tmp/tmux-$UID/moo`)
- User-specific directory (`tmux-$UID`) provides per-user isolation with restrictive permissions

### IPC Protocol

tmux uses the `imsg` protocol (inter-process messaging from OpenBSD) over Unix domain sockets. This is a binary message-passing protocol with typed message enums for identification, commands, and I/O operations. The protocol is custom and tightly coupled to tmux's internal data structures.

### Stale Session Detection

tmux does NOT use PID files. It relies entirely on the Unix socket:

- If the socket file exists and accepts connections, the server is alive.
- If the socket file exists but `connect()` returns `ECONNREFUSED`, the socket is stale (server crashed).
- Stale sockets can be cleaned up by removing the socket file, or the server can recreate its socket on `SIGUSR1`.

### Server Lifecycle

- **Start**: Implicit on first client connection attempt
- **Stay alive**: Server runs as long as any session exists (not any client -- sessions can persist without attached clients)
- **Shutdown**: When the last session is destroyed (`kill-session` or last shell exits)
- **Crash recovery**: Socket becomes stale. Next `tmux` invocation detects `ECONNREFUSED`, removes stale socket, starts fresh server. All previous sessions are lost.

### Relevance to plug

tmux's auto-start-on-first-connect model is exactly what plug needs. The socket-based liveness check (no PID file) is elegant and race-condition-free. However, tmux's "sessions persist without clients" model differs from plug -- plug's daemon should stay alive while clients are connected, not based on internal sessions.

**Sources**:
- [tmux Getting Started Wiki](https://github.com/tmux/tmux/wiki/Getting-Started)
- [Tao of tmux: Server chapter](https://tao-of-tmux.readthedocs.io/en/latest/manuscript/04-server.html)
- [tmux(1) man page](https://man7.org/linux/man-pages/man1/tmux.1.html)
- [tmux PID file discussion (Issue #1305)](https://github.com/tmux/tmux/issues/1305)
- [Reconnect to broken tmux session](https://timvisee.com/blog/reconnect-to-broken-tmux-session/)

---

## Prior Art: Docker

### Architecture Overview

Docker uses a fully separated client-daemon architecture where the CLI (`docker`) is a thin client that talks to a long-running daemon (`dockerd`) over a well-defined REST API.

```
                                         ┌────────────────────────────────┐
                                         │           dockerd              │
  docker build ────┐                     │                                │
                   │   REST API over     │  Container runtime (containerd)│
  docker run ──────┼──────────────────►  │  Image management              │
                   │   Unix socket       │  Network management            │
  docker ps ───────┘   /var/run/         │  Volume management             │
                       docker.sock       │                                │
                                         └────────────────────────────────┘
```

### Communication

- Default: Unix domain socket at `/var/run/docker.sock`
- Protocol: HTTP REST API (not raw socket protocol)
- Optional: TCP socket (port 2375 unencrypted, 2376 TLS) for remote access
- Optional: SSH tunneling
- Multiple sockets simultaneously via multiple `-H` flags

The Unix socket approach means the Docker CLI is stateless -- every command opens a new HTTP connection to the daemon, performs its operation, and exits. There is no persistent connection.

### Daemon Management

Docker daemon is typically managed by systemd (`systemctl start docker`). It is NOT auto-started by the CLI. If `dockerd` is not running, `docker` commands fail with:

```
Cannot connect to the Docker daemon at unix:///var/run/docker.sock.
Is the docker daemon running?
```

This is a conscious design choice: Docker is infrastructure, expected to be always-on, managed by the init system.

### PID File & Crash Recovery

- PID file at `/var/run/docker.pid`
- On crash, the PID file may be left behind (stale)
- On restart, `dockerd` checks if the PID in the file corresponds to a running process
- If the PID is stale (process dead), it overwrites the PID file and starts
- If the PID is alive, it refuses to start (prevents dual-daemon)
- Known issue: on Windows, leftover PID files after host crashes can prevent restart

### Relevance to plug

Docker's model is too heavy for plug. Docker expects system-level daemon management (systemd), which conflicts with plug's "single binary, zero dependencies" principle. However, two Docker patterns are valuable:

1. **HTTP-over-Unix-socket**: A structured protocol over Unix sockets (rather than raw bytes) is cleaner for debugging and extensibility.
2. **PID file + process liveness check**: The pattern of reading the PID file, checking if the process is alive (`kill -0 PID`), and detecting stale PIDs is robust.

**Sources**:
- [dockerd reference](https://docs.docker.com/reference/cli/dockerd/)
- [Understanding docker.sock](https://dev.to/piyushbagani15/understanding-varrundockersock-the-key-to-dockers-inner-workings-nm7)
- [Docker client-server architecture](https://oneuptime.com/blog/post/2026-02-08-how-to-understand-the-docker-client-server-architecture/view)
- [Leftover PID file issue (moby #26729)](https://github.com/moby/moby/issues/26729)
- [PID file recovery (moby #46988)](https://github.com/moby/moby/issues/46988)

---

## Prior Art: zellij

### Architecture Overview

zellij uses a client-server architecture very similar to tmux but implemented in Rust with a thread-per-subsystem model and Protocol Buffers for IPC serialization.

```
                                       ┌──────────────────────────────────┐
                                       │         zellij server            │
                                       │                                  │
 Terminal 1 ─── zellij client 1 ──┐    │  ┌─────────┐  ┌───────────┐     │
                                  ├───►│  │  Route   │  │  Screen   │     │
 Terminal 2 ─── zellij client 2 ──┤    │  │  Thread  │  │  Thread   │     │
                                  │    │  └────┬─────┘  └─────┬─────┘     │
 Web browser ── web client ───────┘    │       │ MPSC         │ MPSC      │
                                       │  ┌────▼─────┐  ┌─────▼─────┐    │
            Unix socket                │  │   PTY    │  │  Plugin   │    │
  /tmp/zellij-$UID/$SESSION            │  │  Thread  │  │  Thread   │    │
                                       │  └──────────┘  └───────────┘    │
                                       └──────────────────────────────────┘
```

### Server Auto-Start

Like tmux, the zellij server can be started automatically when a client attempts to connect to a non-existent session. It can also be started explicitly via `zellij --server <socket_path>`. The server startup is transparent to the user.

### Socket Location

- Path: `/tmp/zellij-{uid}/{session-name}`
- Configurable via `ZELLIJ_SOCK_DIR`
- Session name must fit within Unix socket path limit of 108 bytes total
- Maximum session name length: `108 - len(ZELLIJ_SOCK_DIR) - 1` bytes

### IPC Protocol

zellij uses Protocol Buffers (protobuf) for binary serialization of IPC messages. Two message types:

- **ClientToServerMsg**: User actions, CLI commands, input events
- **ServerToClientMsg**: Render output, status updates, exit signals

Messages are wrapped in `IpcSenderWithContext<T>` and `IpcReceiverWithContext<T>`, which add error context for debugging. The implementation lives in `zellij-utils/src/client_server_contract/`.

### Thread Architecture

The server uses six specialized threads communicating via typed MPSC channels:

| Thread | Responsibility |
|--------|---------------|
| Route | Primary dispatcher -- converts client actions to subsystem instructions |
| Screen | UI state management, rendering, pane/tab state |
| PTY | Terminal process spawn/kill, I/O routing |
| Plugin | WASM plugin execution (wasmi runtime) |
| PTY Writer | Async writes to PTY file descriptors |
| Background Jobs | HTTP downloads, async operations |

A central `Bus<T>` abstraction provides each thread with a typed receiver, while `ThreadSenders` provides typed senders to all threads.

### Session Resurrection (Crash Recovery)

zellij has built-in session resurrection:

- The session layout is serialized every 1 second to the user's cache folder
- On crash or quit, the serialized layout is preserved
- Users can resurrect exited sessions by reattaching (sessions appear in an "EXITED" section)
- This gives zellij a significant crash-recovery advantage over tmux

### Multiple Clients per Session

Multiple clients can attach to a single server session simultaneously. Each client may have a different terminal size and capabilities. The server sends customized render output per client based on their terminal dimensions.

### Relevance to plug

zellij's Rust implementation makes it the closest architectural reference for plug. Key takeaways:

1. **Protobuf over Unix socket** is a proven pattern in Rust for high-performance IPC. However, for plug's needs (forwarding JSON-RPC messages), protobuf adds unnecessary serialization overhead -- the messages are already JSON.
2. **Thread-per-subsystem with MPSC channels** is a clean model for plug's daemon (one thread for upstream management, one for client connections, one for routing).
3. **Session resurrection** is interesting but not directly applicable -- plug's state is simpler (it does not manage terminal buffers, just connections).
4. **Auto-start on connect** confirms this pattern works well in Rust.

**Sources**:
- [Zellij Client-Server Model (DeepWiki)](https://deepwiki.com/zellij-org/zellij/2.1-client-server-model)
- [Zellij Session Management (DeepWiki)](https://deepwiki.com/zellij-org/zellij/5.2-session-management)
- [Zellij Session Resurrection Docs](https://zellij.dev/documentation/session-resurrection.html)
- [Zellij GitHub Repository](https://github.com/zellij-org/zellij)
- [Building Zellij's web client](https://poor.dev/blog/building-zellij-web-terminal/)

---

## Comparative Analysis

| Aspect | tmux | Docker | zellij | **plug (proposed)** |
|--------|------|--------|--------|-------------------|
| **Language** | C | Go | Rust | Rust |
| **IPC transport** | Unix socket | Unix socket (+ TCP) | Unix socket | Unix socket |
| **IPC protocol** | imsg (binary) | HTTP REST | Protobuf | Length-prefixed JSON-RPC |
| **Server start** | Auto on first client | Manual (systemd) | Auto on first client | Auto on first `connect` |
| **Server stop** | Last session destroyed | Manual | Last session destroyed | Configurable (see below) |
| **Liveness check** | Socket connect test | PID file + process check | Socket connect test | Socket connect + PID file |
| **PID file** | No | Yes (`/var/run/docker.pid`) | No | Yes (`~/.local/state/plug/plug.pid`) |
| **Crash recovery** | Sessions lost | Containers survive (restart policy) | Session resurrection (serialized layout) | Clients reconnect, upstream re-initialized |
| **Multi-user** | Per-UID socket dir | Socket permissions / group | Per-UID socket dir | Per-UID socket dir |

---

## Design: Daemon Lifecycle

### State Machine

```
                    ┌─────────┐
                    │  ABSENT  │ ←── No daemon process, no socket file
                    └────┬─────┘
                         │
                    First `plug connect` or `plug start`
                         │
                         ▼
                    ┌──────────┐
              ┌────►│ STARTING │ ←── Daemon spawned, initializing upstreams
              │     └────┬─────┘
              │          │
              │     Socket created, upstreams ready
              │          │
              │          ▼
              │     ┌─────────┐
              │     │ RUNNING  │ ←── Accepting client connections
              │     └────┬─────┘
              │          │
              │     Last client disconnects
              │          │
              │          ▼
              │     ┌──────────┐
              │     │ DRAINING │ ←── Grace period (configurable, default 30s)
              │     └────┬─────┘
              │          │
              │     ┌────┴────┐
              │     │         │
              │  New client   Timer expires / `plug stop`
              │  connects     │
              │     │         ▼
              │     │    ┌────────────┐
              └─────┘    │ SHUTTING   │ ←── Gracefully closing upstreams
                         │ DOWN       │
                         └─────┬──────┘
                               │
                          Cleanup complete
                               │
                               ▼
                         ┌──────────┐
                         │  ABSENT   │ ←── PID file removed, socket removed
                         └──────────┘
```

### Auto-Start on First Connect

When `plug connect` is invoked:

```
1. Check for existing daemon:
   a. Does socket file exist at ~/.local/state/plug/plug.sock?
      - No  → Go to step 2 (start daemon)
      - Yes → Try to connect
        - Connection succeeds → Daemon is alive. Register as client. Done.
        - ECONNREFUSED       → Stale socket. Remove it. Go to step 2.

2. Start the daemon:
   a. Read PID file at ~/.local/state/plug/plug.pid (if exists)
      - PID file exists and process is alive → Unexpected state.
        Wait briefly, retry socket connect (race condition: daemon
        may be mid-startup). If still fails, warn and exit.
      - PID file exists and process is dead → Stale PID file. Remove it.
      - PID file does not exist → Clean state.
   b. Fork/spawn daemon process:
      - Double-fork (Unix) to fully detach from terminal
      - Redirect stdout/stderr to log file
      - Write PID file
      - Create Unix socket and begin listening
      - Load config, start upstream MCP servers
      - Signal readiness (write a byte to a notification pipe, or create a readiness file)
   c. Wait for daemon readiness (up to 10s timeout)
   d. Connect to daemon via socket
   e. Register as client

3. Begin stdio bridge:
   - Read MCP JSON-RPC from stdin, forward to daemon over socket
   - Read responses from daemon over socket, write to stdout
```

### Shutdown Options (Configurable)

```toml
# ~/.config/plug/config.toml

[daemon]
# When to shut down the daemon after the last client disconnects.
# Options:
#   "immediate"     - Shut down as soon as last client disconnects
#   "never"         - Stay alive until explicit `plug stop`
#   "30s"           - Wait 30 seconds, shut down if no new clients connect
#   "5m"            - Wait 5 minutes
shutdown_policy = "30s"
```

The 30-second default balances resource efficiency with avoiding repeated cold starts when switching between editors.

### PID File + Socket File (Belt and Suspenders)

The PID file and socket file serve complementary purposes:

| Check | What it detects | Limitation |
|-------|----------------|------------|
| Socket file exists | Daemon was started at some point | Does not prove daemon is alive |
| Socket `connect()` succeeds | Daemon is alive and accepting connections | Definitive liveness proof |
| Socket `connect()` returns `ECONNREFUSED` | Socket is stale (daemon crashed) | Requires cleanup |
| PID file exists + `kill(pid, 0)` succeeds | A process with that PID exists | PID could be recycled (different process) |
| PID file exists + `/proc/$PID/cmdline` contains "plug" | The plug daemon process exists | Linux-specific, not portable to macOS |
| PID file exists + process dead | Daemon crashed, stale PID file | Requires cleanup |

**Recommended liveness check order**:

```
1. Try socket connect (definitive)
2. If socket missing/stale, check PID file (informational)
3. If PID file stale, clean up both files
4. If PID file alive but socket missing, daemon is mid-startup or corrupt
```

Why both? The PID file enables `plug stop` to send SIGTERM to the daemon even if the socket is wedged. The socket file is the primary liveness check for `plug connect`.

### File Locations

```
~/.local/state/plug/
    plug.pid             # Daemon PID (created by daemon, removed on clean shutdown)
    plug.sock            # Unix domain socket (created by daemon, removed on clean shutdown)

~/.local/share/plug/
    logs/
        plug-daemon.log  # Daemon log output (rolling, daily rotation)

~/.config/plug/
    config.toml          # User configuration (includes [daemon] section)
```

These follow the [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/latest/):
- `~/.local/state/` for runtime state files that should persist across reboots but are not config
- `~/.local/share/` for application data
- `~/.config/` for configuration

Note: On macOS, the `daemonize` crate or manual double-fork handles detaching from the terminal. On Linux, systemd user services are an optional alternative but not required.

---

## Design: Unix Socket IPC Protocol

### Socket Location

```
~/.local/state/plug/plug.sock
```

Permissions: `0600` (owner read/write only). The directory should be `0700`.

### Protocol Choice: Length-Prefixed JSON

After evaluating the options:

| Option | Pros | Cons |
|--------|------|------|
| Raw MCP forwarding (no framing) | Zero overhead | Need streaming JSON parser, message boundary issues |
| Newline-delimited JSON (NDJSON) | Simple, debuggable | Breaks if JSON contains literal newlines (it can) |
| Length-prefixed JSON | Simple framing, handles any JSON | 4-byte overhead per message |
| Protobuf | Efficient, typed | Unnecessary -- messages are already JSON-RPC |
| HTTP over Unix socket | Rich semantics (like Docker) | Overkill for bidirectional streaming |
| JSON-RPC with envelope | Structured, extensible | Adds a wrapper layer on top of already-JSON-RPC messages |

**Recommendation: Length-prefixed JSON** with a thin envelope for control messages.

### Wire Format

```
┌──────────────────────────────────────────────────────────────┐
│  4 bytes (u32 big-endian)  │  N bytes (UTF-8 JSON payload)  │
│  payload length = N        │  { ... }                        │
└──────────────────────────────────────────────────────────────┘
```

Each message on the socket is:
1. A 4-byte unsigned 32-bit integer in big-endian (network byte order) representing the length of the JSON payload
2. The JSON payload (exactly that many bytes)

This is the same framing used by many IPC systems (Ethereum's go-ethereum uses a similar approach for JSON-RPC over IPC).

### Message Types

The IPC protocol uses a thin envelope around MCP messages:

```json
// ── Client → Daemon ──

// Register a new client session (sent immediately after connect)
{
  "type": "register",
  "client_id": "uuid-v4",
  "client_info": {
    "name": "Claude Code",
    "version": "1.2.3"
  }
}

// Forward an MCP message from the AI client (stdin) to the daemon
{
  "type": "mcp",
  "payload": {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/list"
  }
}

// Client is disconnecting gracefully
{
  "type": "disconnect",
  "client_id": "uuid-v4"
}

// ── Daemon → Client ──

// Registration acknowledged
{
  "type": "registered",
  "client_id": "uuid-v4",
  "session_id": "internal-session-id"
}

// Forward an MCP response/notification back to the AI client (stdout)
{
  "type": "mcp",
  "payload": {
    "jsonrpc": "2.0",
    "id": 1,
    "result": { "tools": [...] }
  }
}

// Daemon is shutting down
{
  "type": "shutdown",
  "reason": "explicit stop"
}

// ── Control messages (bidirectional) ──

// Heartbeat (keepalive)
{
  "type": "ping"
}

{
  "type": "pong"
}

// Status query (from CLI commands like `plug status`)
{
  "type": "status_request"
}

{
  "type": "status_response",
  "servers": [...],
  "clients": [...],
  "uptime": 3600
}
```

### Data Flow: `plug connect` as stdio Bridge

```
                          Unix socket
  AI Client             plug connect              plug daemon
  (e.g. Claude)         (stdio bridge)            (background)
      │                      │                         │
      │  MCP JSON-RPC        │                         │
      │  on stdin             │                         │
      ├─────────────────────►│                         │
      │                      │  Wrap in envelope       │
      │                      │  {type:"mcp",           │
      │                      │   payload: {...}}       │
      │                      ├────────────────────────►│
      │                      │                         │
      │                      │                         │  Route to upstream
      │                      │                         │  server, get response
      │                      │                         │
      │                      │  {type:"mcp",           │
      │                      │   payload: {response}}  │
      │                      │◄────────────────────────┤
      │                      │                         │
      │  MCP JSON-RPC        │  Unwrap envelope,       │
      │  on stdout            │  write payload to stdout│
      │◄─────────────────────┤                         │
      │                      │                         │
```

### `plug connect` Implementation (Pseudocode)

```rust
async fn connect() -> Result<()> {
    // 1. Find or start daemon
    let socket_path = state_dir().join("plug.sock");

    if !try_connect(&socket_path).await? {
        start_daemon().await?;
        wait_for_readiness(&socket_path, Duration::from_secs(10)).await?;
    }

    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, writer) = stream.into_split();

    // 2. Register with daemon
    send_message(&writer, RegisterMsg {
        client_id: Uuid::new_v4(),
        client_info: detect_parent_client(),
    }).await?;

    // 3. Bridge stdin ↔ socket, socket ↔ stdout
    let stdin_to_daemon = async {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        loop {
            let mcp_msg = read_jsonrpc_message(&mut reader).await?;
            send_message(&writer, Envelope::Mcp(mcp_msg)).await?;
        }
    };

    let daemon_to_stdout = async {
        let stdout = tokio::io::stdout();
        let mut writer = BufWriter::new(stdout);
        loop {
            let envelope = recv_message(&reader).await?;
            match envelope {
                Envelope::Mcp(payload) => {
                    write_jsonrpc_message(&mut writer, payload).await?;
                }
                Envelope::Shutdown { reason } => {
                    eprintln!("plug daemon shutting down: {reason}");
                    break;
                }
                Envelope::Ping => {
                    send_message(&writer, Envelope::Pong).await?;
                }
                _ => {}
            }
        }
    };

    tokio::select! {
        r = stdin_to_daemon => r,
        r = daemon_to_stdout => r,
    }
}
```

### CLI Commands via Same Socket

`plug status`, `plug server list`, `plug tool list`, and `plug stop` all use the same Unix socket to communicate with the daemon:

```
plug status ──connect──► plug.sock ──► daemon
                                       │
             {type:"status_request"}   │
             ──────────────────────►   │
                                       │
             {type:"status_response",  │
              servers: [...],          │
              clients: [...]}          │
             ◄──────────────────────   │
                                       │
             disconnect                │
```

This means `plug status` works the same whether the daemon was started by `plug connect` or by `plug start` directly.

---

## Design: Failure Modes & Recovery

### Failure Mode 1: Daemon Crashes While Clients Are Connected

**What happens**:
- The daemon process terminates unexpectedly
- The socket file remains on disk (stale)
- The PID file remains on disk (stale)
- All connected `plug connect` processes lose their socket connection
- All upstream MCP server child processes become orphans (their parent died)

**Detection by clients**:
- The `plug connect` process will receive `EOF` or `ECONNRESET` on the Unix socket
- This triggers the client-side error path

**Recovery**:

```
plug connect detects daemon crash:
    │
    ├── Write error message to stderr (visible to AI client as diagnostic)
    │
    ├── Attempt to restart daemon:
    │   ├── Remove stale socket file
    │   ├── Remove stale PID file
    │   ├── Kill orphaned upstream servers (read PIDs from state file if available)
    │   ├── Start new daemon process
    │   ├── Wait for readiness
    │   └── Reconnect
    │
    └── If restart fails:
        ├── Fall back to embedded mode (run upstreams in-process)
        └── OR exit with clear error message
```

**Design decision**: Whether `plug connect` should auto-restart the daemon or exit with an error is configurable:

```toml
[daemon]
# What to do when the daemon crashes while this client is connected.
# Options:
#   "restart"   - Attempt to restart the daemon and reconnect
#   "exit"      - Exit with error (let the AI client handle retry)
#   "embedded"  - Fall back to running upstreams in-process (no sharing)
on_daemon_crash = "restart"
```

### Failure Mode 2: Client Crashes (Ungraceful Disconnect)

**What happens**:
- A `plug connect` process is killed (SIGKILL, crash, parent AI client killed)
- The Unix socket connection is terminated by the OS (FIN/RST)
- No `disconnect` message is sent

**Detection by daemon**:
- The daemon's read loop on the client's socket returns EOF or error
- Alternatively, periodic heartbeat (ping/pong) detects dead clients

**Recovery**:
- Daemon removes the client session from its registry
- If this was the last client and `shutdown_policy` is timer-based, start the drain timer
- No cleanup of upstream servers needed (they are shared and may serve other clients)
- Log the disconnection event

**Implementation**:

```rust
// In the daemon's per-client handler
async fn handle_client(stream: UnixStream, engine: Arc<Engine>) {
    let client_id = register_client(&stream, &engine).await;

    let result = client_message_loop(&stream, &engine, &client_id).await;

    // Regardless of how the loop ended (graceful or crash),
    // clean up the client session
    engine.remove_client(&client_id);

    if engine.client_count() == 0 {
        engine.start_drain_timer();
    }

    match result {
        Ok(()) => info!("Client {client_id} disconnected gracefully"),
        Err(e) => warn!("Client {client_id} disconnected: {e}"),
    }
}
```

### Failure Mode 3: Stale PID File / Stale Socket File

**Scenarios that create stale files**:
- Daemon killed with SIGKILL (no cleanup handler runs)
- Machine crash/reboot (files persist if not in tmpdir)
- Bug in cleanup logic

**Detection**:

```
Check socket:
    exists? ─── connect() succeeds? ──► DAEMON ALIVE
         │              │
         │         ECONNREFUSED ──► STALE SOCKET
         │                          Remove file, proceed
         │
    does not exist ──► check PID file
                            │
                       exists? ─── kill(pid, 0) succeeds? ──► DAEMON STARTING
                            │              │                   (socket not yet created)
                            │         ESRCH (no such process) ──► STALE PID FILE
                            │                                      Remove file, proceed
                            │
                       does not exist ──► DAEMON ABSENT
                                          Start new daemon
```

**Additional safety**: On startup, the daemon acquires an exclusive advisory lock (`flock`) on the PID file. This prevents two daemons from starting simultaneously (race condition between socket check and daemon start). If the lock cannot be acquired, another daemon is starting up concurrently -- wait and retry.

### Failure Mode 4: Permission Issues on Socket File

**Scenario**: Socket file was created by a different user, or directory permissions changed.

**Prevention**:
- Create `~/.local/state/plug/` with mode `0700` (drwx------)
- Create socket with mode `0600` (srw-------)
- Verify ownership on connect: if the socket file is not owned by the current user, refuse to connect and log a clear error

**Error message**:
```
Error: Socket file ~/.local/state/plug/plug.sock is owned by uid 1001,
but you are uid 1000. This could indicate a security issue.
Remove the socket file and try again, or check directory permissions.
```

### Failure Mode 5: Multiple Users on Same Machine

**Solution**: Per-user state directory. The socket path includes no user-specific component by default, but `~/.local/state/` is inherently per-user (it is under the user's home directory). No conflict between users.

If `XDG_STATE_HOME` is set to a shared location (unusual), the socket filename could include the UID: `plug-$UID.sock`. However, this is an edge case not worth optimizing for in Phase 1.

### Failure Mode 6: Orphaned Upstream Server Processes

**Scenario**: Daemon crashes. Its child processes (upstream MCP servers) become orphans (reparented to init/PID 1).

**Prevention**:
- Use process groups (`setsid` / `setpgid`) so all upstream servers share a process group with the daemon
- On daemon start, record child PIDs in a state file (`~/.local/state/plug/children.json`)
- On next daemon start, check for orphaned processes from the previous state file and kill them
- Use `tokio::process::Command`'s `kill_on_drop` behavior as a first line of defense (but this does not help with SIGKILL)

**Supplementary strategy**: On macOS, use `prctl(PR_SET_PDEATHSIG, SIGTERM)` equivalent (not directly available; use `kqueue` with `EVFILT_PROC` and `NOTE_EXIT` on the parent PID, or accept the orphan risk and rely on the state file approach).

---

## Embedded vs Daemon Tradeoff

### Option A: Embedded (Each `plug connect` is Independent)

```
Claude Code ─── plug connect ─┬─ github-server
                               ├─ filesystem-server
                               └─ postgres-server

Cursor ──────── plug connect ─┬─ github-server      ← DUPLICATE
                               ├─ filesystem-server  ← DUPLICATE
                               └─ postgres-server    ← DUPLICATE
```

**Pros**:
- Dead simple -- no IPC, no daemon management, no socket protocol
- Complete failure isolation -- one client crashing cannot affect others
- Easier to debug -- each instance is self-contained
- Works on all platforms identically (no Unix socket platform differences)
- No startup ordering issues

**Cons**:
- N copies of every upstream server (N clients x M servers = N*M processes)
- Port conflicts for HTTP-based upstream servers (two instances trying to bind same port)
- No shared state (each instance discovers tools independently)
- Cannot have a shared TUI/dashboard (no central state to display)
- Higher memory usage and CPU usage
- Upstream servers that maintain state (subscriptions, etc.) have isolated state per client

### Option B: Shared Daemon

```
Claude Code ─── plug connect ─┐
                                ├──► plug daemon ─┬─ github-server
Cursor ──────── plug connect ─┤                  ├─ filesystem-server
                                │                  └─ postgres-server
Gemini CLI ──── plug connect ─┘
```

**Pros**:
- One copy of each upstream server (M processes total, regardless of N clients)
- No port conflicts
- Shared tool cache (warm cache after first client connects)
- Central state for TUI/dashboard
- Lower resource usage
- Natural fit for headless/daemon deployment

**Cons**:
- IPC complexity (socket protocol, message framing, envelope format)
- Daemon lifecycle management (auto-start, shutdown, crash recovery)
- Single point of failure (daemon crash kills all clients' upstream connections)
- Platform differences (Unix socket vs Windows named pipes)
- Harder to debug (messages passing through an additional hop)
- Startup ordering (daemon must be ready before clients can connect)

### Option C: First-Connect-Is-Leader

```
Claude Code ─── plug connect ─┬─ github-server       (this instance IS the leader)
                                │                      (it runs upstreams in-process)
Cursor ──────── plug connect ──┘  (connects to leader via IPC)
```

**Pros**:
- No separate daemon process
- Leader starts instantly (no fork/spawn overhead)
- Simpler than full daemon (leader is just a regular `plug connect` with extra responsibilities)

**Cons**:
- Leader crash kills ALL clients' upstream connections (same as daemon, but worse because the leader is also a client -- its client crashing kills the server for everyone)
- Leader cannot be detached from its terminal (it is a stdio process)
- Complex handoff if leader disconnects (who becomes the new leader?)
- Leader election races when two clients connect simultaneously
- The leader's stdout is consumed by its AI client -- it cannot log to stdout

### Recommendation

**Phase 1: Start with Option A (Embedded)**

Rationale:
- Gets a working product shipped fastest
- No IPC protocol to design, implement, test, debug
- Validates the core multiplexing logic (tool routing, fan-out, merge) independently of daemon concerns
- Most users during early adoption will have 1-2 clients, where the waste is minimal
- Failure isolation is valuable during early development (bugs in one session do not cascade)

**Phase 4: Migrate to Option B (Shared Daemon)**

Rationale:
- By Phase 4, the core engine is battle-tested
- The TUI requires a central daemon anyway (it needs to observe all clients and servers)
- Users with 3+ clients will feel the pain of duplicate upstream servers
- The IPC protocol can be designed based on real-world usage patterns from Phase 1-3

**The migration is low-risk** because the core `Engine` struct is already designed to be UI-agnostic (it does not know whether it is running embedded or in a daemon). The daemon is just a new host for the same Engine, with a Unix socket transport added for client communication.

---

## Recommended Architecture

### Phase 4 Daemon Architecture (Target Design)

```
┌──────────────────────────────────────────────────────────────────────┐
│                         plug daemon process                         │
│                                                                      │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │                        Core Engine                            │    │
│  │                                                              │    │
│  │  SessionMgr ◄─► ToolRouter ◄─► ToolCache                    │    │
│  │       │               │              │                        │    │
│  │       ▼               ▼              ▼                        │    │
│  │  ClientRegistry   ServerRegistry  MergeEngine                 │    │
│  │                                                              │    │
│  │  EventBus (broadcast::Sender<Event>)                         │    │
│  └──────────┬───────────────────────────┬────────────────────────┘    │
│             │                           │                            │
│  ┌──────────▼──────────┐    ┌───────────▼───────────────┐            │
│  │  Client Listener    │    │  Upstream Manager          │            │
│  │                     │    │                            │            │
│  │  Unix socket server │    │  Spawn & manage child      │            │
│  │  Accept connections │    │  processes (stdio servers)  │            │
│  │  Per-client task    │    │  HTTP clients (remote)      │            │
│  │  Length-prefixed    │    │  Health checks              │            │
│  │  JSON envelope      │    │  Circuit breakers           │            │
│  └─────────────────────┘    └────────────────────────────┘            │
│                                                                      │
│  ┌─────────────────────┐    ┌────────────────────────────┐            │
│  │  HTTP Server        │    │  Lifecycle Manager          │            │
│  │  (Axum on :3282)    │    │                            │            │
│  │  Streamable HTTP    │    │  PID file management        │            │
│  │  Legacy SSE         │    │  Signal handlers            │            │
│  │  .localhost routing │    │  Drain timer                │            │
│  └─────────────────────┘    │  Graceful shutdown          │            │
│                              └────────────────────────────┘            │
│                                                                      │
│  ┌─────────────────────────────────────────────────────────────┐     │
│  │  Optional: TUI (Ratatui)                                    │     │
│  │  Subscribes to EventBus, renders to daemon's own terminal   │     │
│  │  (only when daemon is started with `plug` or `plug tui`)    │     │
│  └─────────────────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────────────────┘

          │                                          │
    Unix socket                                 TCP :3282
  ~/.local/state/                          (Streamable HTTP)
    plug/plug.sock
          │                                          │
    ┌─────┴──────┐                            ┌──────┴───────┐
    │            │                            │              │
    ▼            ▼                            ▼              ▼
plug connect  plug connect              Gemini CLI     Remote client
(Claude Code) (Cursor)                  (HTTP POST)    (HTTP POST)
```

### Daemon Startup Sequence

```
1.  Parse config (~/.config/plug/config.toml)
2.  Validate config (or exit with error)
3.  Check for existing daemon (socket + PID file check)
4.  If existing daemon found → error "already running" (or connect to it)
5.  Acquire exclusive flock on PID file
6.  Write PID file
7.  Create Unix socket, bind and listen
8.  Start upstream MCP servers (batched, concurrent)
9.  Wait for upstream initialization (with timeouts)
10. Build tool cache (fan-out tools/list to all upstreams)
11. Start HTTP server on :3282 (if configured)
12. Signal readiness (to any waiting `plug connect` process)
13. Enter main event loop:
    - Accept client connections on Unix socket
    - Accept HTTP connections on :3282
    - Process MCP messages (route, fan-out, merge)
    - Health checks on upstreams (periodic)
    - Heartbeat to connected clients (periodic)
    - Config file change detection (notify watcher)
    - Signal handling (SIGTERM → graceful shutdown, SIGHUP → reload config)
```

### Daemon Shutdown Sequence

```
1.  Receive shutdown signal (SIGTERM, drain timer, or `plug stop`)
2.  Stop accepting new client connections
3.  Send {type: "shutdown"} to all connected clients
4.  Wait up to 5s for in-flight MCP requests to complete
5.  Close all client socket connections
6.  Graceful upstream shutdown:
    a. Close stdin on each child process (signals EOF to upstream)
    b. Wait up to 5s for process exit
    c. Send SIGTERM to remaining processes
    d. Wait up to 5s
    e. Send SIGKILL to remaining processes
7.  Close HTTP server (drain connections)
8.  Remove Unix socket file
9.  Remove PID file
10. Release flock
11. Exit with code 0
```

---

## Migration Path: Phase 1 to Phase 4

The migration from embedded mode to daemon mode is designed to be incremental and non-breaking.

### Phase 1 (Embedded): What We Build

```rust
// main.rs -- plug connect runs the engine in-process

async fn cmd_connect(config: Config) -> Result<()> {
    let engine = Engine::new(config).await?;
    engine.start_upstreams().await?;

    let stdio = StdioTransport::new();
    engine.handle_client(stdio).await?;

    engine.shutdown().await?;
    Ok(())
}
```

The Engine is a standalone struct that manages upstreams and routes messages. It has no knowledge of daemon mode.

### Phase 4 (Daemon): What We Add

```rust
// daemon.rs -- the daemon hosts the engine and accepts socket connections

async fn run_daemon(config: Config) -> Result<()> {
    let engine = Arc::new(Engine::new(config).await?);
    engine.start_upstreams().await?;

    let listener = UnixListener::bind(socket_path())?;

    loop {
        let (stream, _) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            let transport = SocketTransport::new(stream);
            engine.handle_client(transport).await;
            // Client disconnected; engine.handle_client does cleanup
        });
    }
}

// connect.rs -- plug connect becomes a thin stdio↔socket bridge

async fn cmd_connect() -> Result<()> {
    let socket = ensure_daemon_running().await?;
    bridge_stdio_to_socket(socket).await
}
```

### What Changes Between Phases

| Component | Phase 1 | Phase 4 | Migration Effort |
|-----------|---------|---------|-----------------|
| `Engine` | Runs in `plug connect` process | Runs in daemon process | **None** (same struct) |
| `plug connect` | Full engine in-process | Thin stdio↔socket bridge | **Rewrite** (simpler code) |
| Client transport | `StdioTransport` | `SocketTransport` wrapping socket | **New trait impl** |
| Upstream management | Per-process | Shared in daemon | **None** (Engine handles it) |
| `plug status` | Must start engine to query | Queries daemon via socket | **Add socket client** |
| PID file / socket | Not needed | Required | **New code** |
| TUI | N/A | Subscribes to daemon's EventBus | **New code** |

The key insight: **the Engine does not change**. It accepts a `Transport` (trait object) for each client. In Phase 1, that transport is `StdioTransport`. In Phase 4, it is `SocketTransport`. The Engine does not know or care about the difference.

### Backward Compatibility

In Phase 4, `plug connect` should still support embedded mode as a fallback:

```
plug connect              → Connect to daemon (default in Phase 4)
plug connect --embedded   → Run engine in-process (Phase 1 behavior)
```

This ensures users can always fall back to the simpler mode if the daemon has issues, and provides a migration path where users can opt-in to daemon mode before it becomes the default.

---

## Open Questions

### OQ1: Should the daemon support multiple configs?

If two users (or two separate projects with different `config.toml` files) want to run separate daemon instances, should there be support for named daemon instances (like tmux's `-L` flag)?

Proposed answer: Yes, via `--name` flag:
```
plug start --name work     → ~/.local/state/plug/work.sock
plug start --name personal → ~/.local/state/plug/personal.sock
plug connect --name work   → connects to the "work" daemon
```

Default name is `default`, matching tmux's convention.

### OQ2: Should the daemon be a child process of `plug connect` or a fully detached daemon?

- **Child process**: Easier to implement, but dies when the first `plug connect` process exits (unless double-forked).
- **Fully detached (double-fork)**: Survives the parent process, but requires proper daemonization (redirect stdio, setsid, etc.).

Proposed answer: Fully detached via double-fork. Use the `daemonize` crate for Rust, which handles the fork-setsid-fork-redirect pattern. This matches tmux and zellij's approach.

### OQ3: How should `plug connect` detect that it is being invoked by an AI client?

When `plug connect` is invoked, it needs to know whether:
1. It is the stdio bridge for an AI client (should connect to daemon, bridge stdio)
2. It is a human typing `plug connect` in a terminal (should show status info)

Proposed answer: `plug connect` always behaves as a stdio bridge. Humans use `plug status` for information. If stdin is a TTY (not piped), `plug connect` could print a warning: "plug connect is designed to be invoked by AI clients. Use `plug status` to check daemon status."

### OQ4: What is the heartbeat interval?

Proposed: 30-second ping/pong between daemon and each connected client. If 3 consecutive pongs are missed (90 seconds), the client is considered dead and its session is cleaned up.

### OQ5: Should the IPC protocol version be negotiated?

Yes. The `register` message should include a protocol version field. This allows future changes to the envelope format without breaking older `plug connect` binaries talking to a newer daemon (or vice versa).

```json
{
  "type": "register",
  "protocol_version": 1,
  "client_id": "uuid-v4",
  "client_info": { ... }
}
```

If the daemon does not support the requested protocol version, it responds with an error and the minimum/maximum supported versions, allowing the client to adapt or fail with a clear message ("Please update plug: your client speaks protocol v1 but the daemon requires v2+").

---

## Summary of Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Phase 1 architecture | Embedded (Option A) | Ship faster, validate core logic first |
| Phase 4 architecture | Shared daemon (Option B) | Efficient, enables TUI, proven by tmux/zellij |
| Server auto-start | Yes, on first `plug connect` | Matches tmux/zellij UX, zero-friction |
| Liveness detection | Socket connect test + PID file | Socket is definitive; PID file enables `plug stop` |
| IPC framing | 4-byte length-prefixed JSON | Simple, debuggable, handles arbitrary JSON |
| IPC messages | Thin envelope around MCP JSON-RPC | Minimal overhead, MCP messages pass through unchanged |
| Socket location | `~/.local/state/plug/plug.sock` | XDG-compliant, per-user isolation |
| Shutdown policy | Configurable timer (default 30s) | Balances resource usage vs restart overhead |
| Crash recovery | Auto-restart daemon, clean stale files | Matches user expectation of "just works" |
| Daemon detachment | Double-fork (fully detached) | Survives parent process exit, matches tmux |
| Named instances | Support via `--name` flag | Enables multi-project workflows |
| Protocol versioning | Version field in register message | Forward compatibility |
