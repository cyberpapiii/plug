# Users, Personas, and User Stories

## Who Uses This

fanout is built for **individual power users** — developers and AI practitioners who use multiple AI coding and agent clients daily and are frustrated by the fragmentation of MCP server management.

fanout is NOT built for:
- Enterprise IT teams managing MCP servers for hundreds of developers (use AgentGateway or IBM ContextForge)
- Non-technical users who don't know what MCP is
- People who use only one AI client (they don't need a multiplexer)

---

## Primary Personas

### Persona 1: "The Polyglot" — Multi-Client Power User

**Who**: A senior developer or tech lead who uses 3-8 AI clients simultaneously throughout their workday. They might have Claude Code open for coding, Cursor for a different project, Gemini CLI for quick questions, and Claude Desktop for longer conversations.

**Pain today**:
- Has MCP server configs duplicated across 5+ config files in different formats (JSON for Claude Desktop, TOML for Codex, JSON for Cursor, etc.)
- When they add a new MCP server, they have to update every client's config
- Port conflicts: two clients trying to run the same MCP server simultaneously
- Different clients expose different subsets of tools — inconsistent experience
- Cursor silently drops tools beyond its 40-tool limit — they don't know which tools are missing
- Has wasted hours debugging "why does this tool work in Claude but not in Cursor?"

**What they want**:
- ONE place to manage all MCP servers
- ALL clients connected through that one place
- Tools available consistently across all clients
- No more port conflicts or duplicate processes
- Quick visual status of what's connected and healthy

**Success metric**: They should never think about MCP server configuration again after initial setup.

### Persona 2: "The Automator" — AI Agent Operator

**Who**: A developer who runs autonomous AI agents (Claude Code in autonomous mode, Codex, custom agents) that need reliable, programmatic access to MCP tools. They may run multiple agent instances concurrently.

**Pain today**:
- Agents can't share MCP servers — each instance spawns its own
- No structured error output — agents get cryptic error messages they can't parse
- Interactive prompts (OAuth redirects, confirmation dialogs) break agent workflows
- No way to programmatically check server health or tool availability
- Tool discovery is slow — full fan-out on every connection

**What they want**:
- Concurrent agent instances sharing the same MCP servers without conflicts
- Structured JSON output for every operation (`--output json`)
- Non-interactive everything (`--yes`, `--non-interactive`, token-based auth)
- Fast tool discovery (cached, not fan-out on every connect)
- Reliable error reporting with error codes, retryable flags, and recovery hints

**Success metric**: An agent should be able to discover, configure, and use MCP servers without any human intervention.

### Persona 3: "The Tinkerer" — MCP Server Developer

**Who**: A developer who builds and tests MCP servers. They need a way to quickly test their server against multiple clients, inspect tool definitions, and debug protocol-level issues.

**Pain today**:
- Have to configure each client separately to test their server
- No easy way to see what the multiplexer is doing to their tool definitions
- Protocol-level debugging requires custom tooling
- No centralized log of all MCP traffic

**What they want**:
- Connect their server once, test it from multiple clients
- See exactly what tool definitions clients receive (including after any transforms)
- Activity log showing every MCP request/response
- Quick health check for their server

**Success metric**: They should be able to develop and test an MCP server with fanout faster than connecting clients directly.

### Persona 4: "The Server Admin" — Always-On Deployment

**Who**: A developer or team that runs an always-on server (home lab, cloud VM, dedicated machine) where AI agents operate autonomously 24/7. The machine may not have a display.

**Pain today**:
- No good headless MCP multiplexer option — most require GUI or Docker
- No monitoring of server health or tool availability
- No automatic recovery from server failures
- No way to manage the multiplexer remotely

**What they want**:
- Headless/daemon mode (`fanout --headless`)
- Automatic recovery from failures (circuit breakers, reconnection)
- Structured logging to files
- CLI commands for remote management (over SSH)
- Minimal resource footprint (RAM, CPU)

**Success metric**: fanout should run for weeks without intervention, automatically recovering from transient failures.

---

## User Stories

### Setup & Onboarding

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| S1 | As a polyglot, I want to install fanout with one command (`brew install fanout`) so that I can get started immediately | 1 | P0 |
| S2 | As a polyglot, I want fanout to auto-detect my existing MCP configs (Claude Desktop, Cursor, Codex, Claude Code) on first run so that I don't have to re-enter everything | 1 | P0 |
| S3 | As a polyglot, I want to import configs with one keystroke so that migration is effortless | 1 | P0 |
| S4 | As an automator, I want to run `fanout --import-all --yes` non-interactively so that I can script the setup | 2 | P0 |
| S5 | As a polyglot, I want fanout to generate the correct config snippet for each client (Claude Code .mcp.json, Cursor config, etc.) so that I don't have to figure out the format | 1 | P0 |
| S6 | As a tinkerer, I want to add a new MCP server with `fanout server add` and have it immediately available to all clients | 3 | P1 |
| S7 | As a server admin, I want to start fanout as a daemon with `fanout --headless` so that it runs without a terminal | 4 | P1 |

### Core Multiplexing

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| M1 | As a polyglot, I want all my AI clients to share the same MCP servers so that tools are consistent everywhere | 1 | P0 |
| M2 | As a polyglot, I want to have multiple Claude Code instances open simultaneously, all using the same MCP servers without conflicts | 1 | P0 |
| M3 | As an automator, I want 10+ concurrent agent instances sharing servers so that I can run agents in parallel | 2 | P0 |
| M4 | As a polyglot, I want tool calls to route to the correct server automatically so that I never think about routing | 1 | P0 |
| M5 | As a polyglot, I want `tools/list` results to be merged from all servers and returned as one unified list | 1 | P0 |
| M6 | As a polyglot, I want resources, prompts, and completions to pass through cleanly to the correct server | 1 | P1 |
| M7 | As a polyglot, I want `list_changed` notifications from any server to propagate to all connected clients | 1 | P1 |

### Client Compatibility

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| C1 | As a Cursor user, I want fanout to automatically limit tools to 40 (Cursor's hard cap) and prioritize the most-used tools so that I don't lose tools silently | 1 | P0 |
| C2 | As a Windsurf user, I want fanout to limit tools to 100 | 1 | P0 |
| C3 | As a VS Code user, I want fanout to limit tools to 128 | 1 | P0 |
| C4 | As a Gemini CLI user, I want `prompts/list` to respond instantly so that tool discovery isn't blocked by Gemini's sequential flow | 1 | P0 |
| C5 | As a Codex user, I want `resources/list` to always return `{resources: []}` (not an error) so that Codex doesn't mark fanout as unavailable | 1 | P0 |
| C6 | As an OpenCode user, I want fanout to serve SSE so that OpenCode can connect (it doesn't support Streamable HTTP yet) | 1 | P1 |
| C7 | As a Zed user, I want to connect via stdio (`fanout connect`) since Zed only supports stdio | 1 | P1 |
| C8 | As a Claude Code user, I want fanout to support tool search / catalog mode so that large tool sets don't bloat my context window | 1 | P1 |

### Token Efficiency

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| T1 | As a polyglot, I want tool definitions sent to my clients to use minimal tokens so that my context window isn't wasted | 1 | P0 |
| T2 | As a polyglot, I want full tool schemas loaded only when a tool is actually used, not on initial list | 1 | P1 |
| T3 | As a Cursor user with 40 tools, I want the 40 most relevant tools selected (not arbitrary first 40) | 1 | P1 |
| T4 | As an automator, I want to configure which tools are prioritized per-client so that agents get the tools they need | 2 | P2 |

### Reliability

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| R1 | As a polyglot, I want fanout to keep working when one MCP server crashes — only that server's tools should be affected | 1 | P0 |
| R2 | As a server admin, I want fanout to automatically reconnect to servers that come back online | 4 | P0 |
| R3 | As a polyglot, I want tools from a timed-out server to be served from cache (last known state) so that temporary blips don't lose tools | 1 | P1 |
| R4 | As a server admin, I want circuit breakers so that a consistently failing server doesn't slow down the entire system | 4 | P1 |
| R5 | As a server admin, I want fanout to run for weeks without intervention | 4 | P1 |
| R6 | As a polyglot, I want no orphaned MCP server processes when fanout shuts down | 1 | P0 |

### Monitoring & Debugging

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| D1 | As a polyglot, I want a TUI dashboard showing server health, connected clients, and tool counts at a glance | 1 | P1 |
| D2 | As a tinkerer, I want to see a real-time activity log of all MCP requests flowing through fanout | 3 | P1 |
| D3 | As an automator, I want `fanout status --output json` to return structured server health data | 2 | P0 |
| D4 | As a polyglot, I want `fanout doctor` to diagnose common issues (port conflicts, missing env vars, expired tokens) | 1 | P1 |
| D5 | As a tinkerer, I want to see exactly which tools each client received (after filtering/limiting) | 3 | P2 |

### Configuration

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| F1 | As a polyglot, I want one TOML config file for all my servers | 1 | P0 |
| F2 | As a polyglot, I want to reference env vars in config (`$GITHUB_TOKEN`) so secrets aren't in the file | 1 | P0 |
| F3 | As a polyglot, I want config changes to apply without restarting fanout (hot reload) | 1 | P1 |
| F4 | As a polyglot, I want per-client overrides (tool limits, priority tools) in the same config file | 1 | P2 |
| F5 | As a polyglot, I want config validation with clear error messages (`fanout config validate`) | 1 | P1 |

### Portless (.localhost Routing)

| ID | Story | Persona | Priority |
|----|-------|---------|----------|
| P1 | As a tinkerer, I want each server reachable at `servername.localhost:3282` so that I can test individual servers easily | 3 | P2 |
| P2 | As an automator, I want stable, named URLs for servers instead of port numbers so that agent configs don't break when ports change | 2 | P2 |

---

## Usage Scenarios (Detailed)

### Scenario 1: "Monday Morning Setup"

Rob just reformatted his MacBook. He has Claude Code, Cursor, Gemini CLI, and Codex installed. He needs GitHub, Notion, Filesystem, and Postgres MCP servers available in all of them.

**Without fanout**: Configure each server in each client. 4 servers x 4 clients = 16 configurations across 4 different config file formats. Takes 30-60 minutes. Debugging port conflicts takes another 30 minutes.

**With fanout**:
```bash
brew install fanout
fanout
# Auto-detects existing configs, imports with Y
# Outputs connection snippets for each client
# Copy-paste 4 snippets. Done. 5 minutes.
```

### Scenario 2: "The 3 AM Agent Run"

Rob kicks off 5 autonomous Claude Code agents to work on different parts of a codebase overnight. Each needs access to GitHub, Postgres, and Filesystem MCP servers.

**Without fanout**: Each agent spawns its own MCP server instances. 5 x 3 = 15 processes. Port conflicts. Memory bloat. Some agents fail silently because servers are already bound.

**With fanout**: All 5 agents connect to one fanout instance. 3 server processes total. Zero conflicts. If Postgres server crashes at 4 AM, fanout's circuit breaker kicks in, other tools keep working, Postgres auto-reconnects when it comes back.

### Scenario 3: "New Tool, Every Client"

Rob discovers a new MCP server for Jira. He wants it available in all 6 of his AI clients immediately.

**Without fanout**: Edit 6 config files. Restart 6 clients. Cross fingers that the tool name doesn't collide with existing tools.

**With fanout**:
```bash
fanout server add jira --command "npx -y @modelcontextprotocol/server-jira" --env JIRA_TOKEN=$JIRA_TOKEN
# Done. All 6 clients see the new tools on the next tools/list call.
```

### Scenario 4: "Why Is Cursor Missing Tools?"

Rob has 60 tools across all his MCP servers. He notices Cursor only shows 40. He doesn't know why.

**Without fanout**: No visibility. Cursor silently drops tools beyond 40. No error message. No way to know which tools are missing.

**With fanout**: The TUI shows `Cursor #3: 40/60 tools served`. The doctor command explains the limit. The config allows setting `priority_tools` to control which 40 Cursor gets.

### Scenario 5: "The Server Admin"

Rob runs a headless Linux server where AI agents operate 24/7. He needs MCP servers always available.

**Without fanout**: Write a custom systemd service. No monitoring. No auto-recovery. SSH in to check if things are working.

**With fanout**:
```bash
fanout --headless  # or run as a systemd service
fanout status --output json  # check health over SSH
# Circuit breakers and auto-reconnection handle failures
# Logs rotate to ~/.local/share/fanout/logs/
```

---

## Edge Cases to Design For

These are the sharp edges that separate "good enough" to "undeniably perfect":

1. **Client connects before all servers are ready** — What tools does it see? (Answer: whatever's ready so far, updated via list_changed as more come online)
2. **Server crashes mid-tool-call** — What error does the client get? (Answer: clear JSON-RPC error with error code, not a hang or timeout)
3. **Two servers define a tool with the same name** — How is the collision resolved? (Answer: prefix with server name, warn in TUI)
4. **Client disconnects and reconnects** — Does it get a stale tool list? (Answer: fresh list_changed on reconnect)
5. **Config file is edited while fanout is running** — What happens? (Answer: hot-reload, add new servers, remove deleted ones)
6. **GITHUB_TOKEN env var is not set** — What happens on server start? (Answer: clear error message naming the missing var, server marked as failed, other servers unaffected)
7. **Upstream server sends a notification while no client SSE stream is open** — Is it lost? (Answer: queued and delivered when stream reconnects, with resumability via Last-Event-ID)
8. **User runs `fanout connect` from two different terminals for the same client type** — Are they isolated? (Answer: yes, each gets its own session)
9. **Server returns 10,000 tools** — What happens? (Answer: catalog mode, search meta-tool, lazy schemas)
10. **User wants to temporarily disable a server without removing it** — How? (Answer: `enabled = false` in config, or `fanout server disable <name>`)
