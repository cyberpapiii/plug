# UX Design

fanout serves two audiences equally: humans at a terminal and AI agents calling commands programmatically. Every interface must satisfy both.

---

## Design Philosophy

### Lessons from the Best CLI Tools

We studied lazygit (73K stars), k9s (33K stars), starship (54K stars), zellij (30K stars), atuin (29K stars), gitui (22K stars), and bottom (13K stars). The shared patterns:

1. **Zero-config first run** — Works immediately after install. Configuration is for customization, never for basic operation.
2. **Discoverability over documentation** — The UI teaches you how to use it while you use it. Context-aware keybinding hints. `?` for help.
3. **Speed as a feature** — Terminal users expect instant response. Sub-100ms for any interactive operation.
4. **Progressive disclosure** — Simple on the surface, powerful underneath. Short `--help` by default, `--help` for verbose.
5. **Detect and adapt** — TTY vs pipe detection. Colors auto-disabled when piped. `NO_COLOR` env var respected.
6. **Errors that help** — Don't just say what went wrong. Say what to do about it.

### lazygit's Design Principles (Jesse Duffield)

These directly apply to fanout:
- **Simplicity** — Fewer concepts to learn
- **Consistency** — Same key does the same thing everywhere
- **Discoverability** — Context-aware hints in every panel
- **Sane defaults** — Works without config
- **Shortcuts for common flows** — The 80% case should be one keystroke
- **Two goals that sometimes conflict**: easy for newcomers AND fast for long-term users. When uncertain, add a confirmation.

---

## CLI Design

### Command Structure

```
fanout                              # Start TUI (default action)
fanout --headless                   # Daemon mode (no TUI)
fanout --version                    # Version info

fanout status                       # Server and client health overview
fanout connect                      # stdio bridge (what clients invoke)

fanout server list                  # List all configured servers
fanout server add <name>            # Add a new server
fanout server remove <name>         # Remove a server
fanout server enable <name>         # Enable a disabled server
fanout server disable <name>        # Disable without removing
fanout server restart <name>        # Restart a specific server

fanout tool list                    # List all tools across all servers
fanout tool search <query>          # Search tools by name/description

fanout config validate              # Validate config file
fanout config edit                  # Open config in $EDITOR
fanout config path                  # Print config file path

fanout import <source>              # Import from: claude-desktop, cursor, codex, etc.
fanout import --all                 # Import from all detected sources
fanout export <target>              # Generate config for a client

fanout doctor                       # Diagnose common issues
fanout logs                         # Tail the log file (when not in TUI)
```

### The Noun-Verb Pattern

Commands follow `fanout <noun> <verb>` (like `docker container ls`, `kubectl get pods`). This is exceptionally agent-friendly because exploration is a deterministic tree search:

```
$ fanout --help           → see all nouns (server, tool, config, ...)
$ fanout server --help    → see all verbs (list, add, remove, ...)
$ fanout tool --help      → see all verbs (list, search, ...)
```

**Exception**: Top-level shortcuts for the most common actions:
- `fanout` = `fanout tui` (start the TUI)
- `fanout status` = quick health check without entering TUI
- `fanout connect` = stdio bridge (invoked by clients, not humans)
- `fanout doctor` = diagnostic tool

### Output Modes

Every command supports three output modes:

**Pretty (default for TTY)**:
```
$ fanout status

  Servers (4 connected, 1 failed)
  ────────────────────────────────
  ● github        12 tools   3ms   connected
  ● filesystem     4 tools   1ms   connected
  ● brave-search   1 tool    8ms   connected
  ● postgres       8 tools   2ms   connected
  ○ notion        -- tools   --    failed: connection refused

  Clients (3 connected)
  ────────────────────────────
  ↔ Claude Code #1   25 tools
  ↔ Cursor #2        25/25 tools
  ↔ Gemini CLI #3    25 tools
```

**JSON (--output json)**:
```json
{
  "schema_version": "1.0",
  "command": "status",
  "status": "ok",
  "data": {
    "servers": [
      {"name": "github", "status": "connected", "tools": 12, "latency_ms": 3}
    ],
    "clients": [
      {"id": 1, "type": "claude-code", "tools_served": 25, "tools_total": 25}
    ]
  },
  "errors": [
    {"server": "notion", "code": "CONNECTION_REFUSED", "retryable": true}
  ],
  "warnings": []
}
```

**JSONL (--output jsonl)** — for streaming commands like `fanout logs`:
```jsonl
{"timestamp":"2026-03-03T14:32:01Z","level":"info","event":"tool_call","server":"github","tool":"create_issue","duration_ms":142}
{"timestamp":"2026-03-03T14:32:02Z","level":"warn","event":"timeout","server":"notion","duration_ms":25000}
```

### Error Messages

**For humans** (default):
```
Error: Server "notion" failed to start

  The command `npx @modelcontextprotocol/server-notion` exited with code 1.

  Likely cause: The NOTION_TOKEN environment variable is not set.

  To fix:
    export NOTION_TOKEN="your-token-here"
    fanout server restart notion
```

**For agents** (--output json):
```json
{
  "status": "error",
  "errors": [{
    "code": "SERVER_START_FAILED",
    "server": "notion",
    "message": "Command exited with code 1",
    "cause": "Missing environment variable: NOTION_TOKEN",
    "retryable": false,
    "hint": "Set NOTION_TOKEN environment variable"
  }]
}
```

### Non-Interactive Mode

For agent and scripted use, every interactive prompt has a non-interactive equivalent:

| Interactive | Non-Interactive |
|-------------|-----------------|
| "Import configs? [Y/n]" | `fanout import --all --yes` |
| "Remove server? [y/N]" | `fanout server remove notion --yes` |
| "Select servers to import: [1,2,3]" | `fanout import claude-desktop cursor` |

`FANOUT_NON_INTERACTIVE=1` environment variable forces non-interactive globally.

---

## TUI Design

### Layout

```
┌─ fanout v0.1.0 ──────────────────────────────────────────────────┐
│                                                                   │
│  ┌─ Servers ─────────────────────┐  ┌─ Clients ────────────────┐ │
│  │ ● github       12 tools  3ms │  │ ↔ Claude Code #1    25t  │ │
│  │ ● filesystem    4 tools  1ms │  │ ↔ Claude Code #2    25t  │ │
│  │ ● brave-search  1 tool   8ms │  │ ↔ Cursor #3     25/40t  │ │
│  │ ● postgres      8 tools  2ms │  │ ↔ Gemini CLI #4     25t  │ │
│  │ ○ notion       -- tools  err │  │                           │ │
│  └───────────────────────────────┘  └───────────────────────────┘ │
│                                                                   │
│  ┌─ Activity ────────────────────────────────────────────────────┐│
│  │ 14:32:01 [Claude Code #1]  tools/call github__create_issue   ││
│  │ 14:31:58 [Cursor #3]       tools/list (25/25 served)         ││
│  │ 14:31:55 [Gemini CLI #4]   prompts/list (0 prompts)          ││
│  │ 14:31:52 [Claude Code #2]  tools/call postgres__query  143ms ││
│  │ 14:31:50 [System]          notion: circuit breaker opened     ││
│  └───────────────────────────────────────────────────────────────┘│
│                                                                   │
│  [s]ervers  [c]lients  [t]ools  [l]ogs  [d]octor  [?]help  [q]  │
└───────────────────────────────────────────────────────────────────┘
```

### Navigation

**Global keys** (always work):
- `s` — Focus servers panel
- `c` — Focus clients panel
- `t` — Tools view (all tools, searchable)
- `l` — Full log viewer
- `d` — Doctor diagnostics
- `?` — Help overlay
- `q` — Quit
- `/` — Search within current view

**Panel-specific keys** (shown in bottom bar when panel focused):
- Servers: `Enter` = details, `r` = restart, `d` = disable/enable, `a` = add
- Clients: `Enter` = details (tools served, session info)
- Tools: `Enter` = full schema, `/` = search

### Visual Language

**Status indicators**:
- `●` (solid circle, green) — Connected and healthy
- `◐` (half circle, yellow) — Degraded (circuit breaker half-open)
- `○` (empty circle, red) — Failed or disconnected
- `↔` (double arrow) — Active client connection
- `…` (ellipsis) — Connecting/starting

**Colors** (respects NO_COLOR, auto-disabled when piped):
- Green: healthy, success
- Yellow: warning, degraded
- Red: error, failed
- Cyan: informational, headers
- Dim: secondary information

### Panels

**Servers Panel**: Real-time list of upstream MCP servers. Shows name, tool count, latency (rolling P50), and health status. Highlighted row shows server details.

**Clients Panel**: Real-time list of connected downstream clients. Shows client type, session ID, tools served vs total, and last activity timestamp.

**Activity Panel**: Rolling log of MCP requests flowing through fanout. Each entry shows timestamp, client, method, target, and latency.

**Tools View** (full screen when activated): Searchable list of all tools across all servers. Shows tool name, server origin, description preview, and annotations. `Enter` shows full schema.

**Log View** (full screen): Full structured log output from tracing. Filterable by level, server, client.

**Doctor View** (full screen): Diagnostic results. Checks for port conflicts, missing env vars, expired tokens, unreachable servers, config errors.

### Responsive Layout

The TUI adapts to terminal size:
- **Wide** (> 120 cols): Side-by-side servers + clients, activity below
- **Medium** (80-120 cols): Stacked servers/clients, activity below
- **Narrow** (< 80 cols): Single panel with tabs

### Keyboard Shortcut Display

Context-aware keybinding bar at the bottom of every view. Changes based on what's focused:

```
# When servers panel focused:
[enter] details  [r]estart  [d]isable  [a]dd  [/]search  [?]help

# When tools view active:
[enter] schema  [/]search  [esc] back  [?]help

# When log view active:
[f]ilter  [/]search  [esc] back  [?]help
```

---

## First-Run Experience

The first-run experience IS the product. It must be perfect.

### Flow

```
$ fanout

┌─ fanout ─────────────────────────────────────────────────────────┐
│                                                                   │
│  Welcome to fanout — MCP multiplexer                             │
│                                                                   │
│  Scanning for existing MCP configurations...                     │
│                                                                   │
│  Found:                                                          │
│    [✓] Claude Desktop  — 3 servers: github, filesystem, brave    │
│    [✓] Cursor          — 1 server: github                        │
│    [✓] Claude Code     — 2 servers: github, postgres             │
│    [ ] Codex           — not found                               │
│    [ ] Gemini CLI      — not found                               │
│                                                                   │
│  4 unique servers detected across 3 clients.                     │
│                                                                   │
│  Import all? [Y/n/select]                                        │
│                                                                   │
└──────────────────────────────────────────────────────────────────┘
```

After import:
```
│  Config written to ~/.config/fanout/config.toml                  │
│                                                                   │
│  Starting servers...                                             │
│    ● github        connected  (12 tools, 15ms)                   │
│    ● filesystem    connected  (4 tools, 3ms)                     │
│    ● brave-search  connected  (1 tool, 8ms)                      │
│    ● postgres      connected  (8 tools, 5ms)                     │
│                                                                   │
│  Ready. 25 tools available across 4 servers.                     │
│                                                                   │
│  Connect your AI clients:                                        │
│                                                                   │
│  Claude Code / Cursor / Zed (stdio):                             │
│    Add to .mcp.json:                                             │
│    {"mcpServers":{"fanout":{"command":"fanout","args":["connect"]}}}│
│                                                                   │
│  Gemini CLI (HTTP):                                              │
│    Add to ~/.gemini/settings.json:                               │
│    {"mcpServers":{"fanout":{"httpUrl":"http://localhost:3282/mcp"}}}│
│                                                                   │
│  Or auto-configure:                                              │
│    fanout export claude-desktop                                  │
│    fanout export cursor                                          │
│    fanout export gemini-cli                                      │
│                                                                   │
│  Press any key to enter the dashboard...                         │
```

### Non-Interactive First Run (for agents/scripts)

```bash
fanout --import-all --yes --headless
# Imports all detected configs, starts in daemon mode, exits to dashboard
# Output is structured JSON when --output json is added
```

---

## Doctor Command

`fanout doctor` runs a series of diagnostic checks:

```
$ fanout doctor

  fanout doctor — diagnosing your setup

  ✓ Config file valid (~/.config/fanout/config.toml)
  ✓ Port 3282 available
  ✓ All environment variables set
  ✗ Server "notion": NOTION_TOKEN is empty
  ✓ Server "github": connected, 12 tools
  ✓ Server "filesystem": connected, 4 tools
  ✗ Server "notion": connection refused (is the server installed?)
  ✓ Server "postgres": connected, 8 tools
  ✓ No tool name collisions detected
  ⚠ Cursor detected: 25 tools served of 25 (below 40 limit)

  2 issues found:
    1. Set NOTION_TOKEN: export NOTION_TOKEN="your-token"
    2. Install Notion MCP server: npx @modelcontextprotocol/server-notion

  Run `fanout doctor --output json` for machine-readable output.
```
