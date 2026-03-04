# plug

**One binary. Every client. Every server. Zero friction.**

A ruthlessly minimal MCP multiplexer written in Rust. The single point of connection between all your AI coding clients and all your MCP servers — simultaneously, concurrently, without conflicts.

```
Claude Code ──┐                      ┌── github (12 tools)
Claude Code ──┤                      ├── notion (8 tools)
Cursor ───────┤── plug ─────────────┤── filesystem (4 tools)
Gemini CLI ───┤   (single binary)   ├── postgres (6 tools)
Codex ────────┤                      └── brave-search (1 tool)
OpenCode ─────┘
```

## Installation

### Homebrew (macOS and Linux)

```sh
brew install plug-mcp/tap/plug
```

### Shell installer (macOS and Linux)

```sh
curl -fsSL https://get.plug.sh | sh
```

Or install to a specific directory:

```sh
curl -fsSL https://get.plug.sh | sh -s -- --install-dir ~/.local/bin
```

### Cargo

```sh
cargo install plug
```

### Manual

Download the binary for your platform from the [releases page](https://github.com/plug-mcp/plug/releases), verify the SHA-256 checksum, and place it in your PATH.

## Quick Start

**1. Import your existing MCP servers** (auto-discovers from all installed AI clients):

```sh
plug import --all --yes
```

Or create a config file manually at `~/.config/plug/config.toml`:

```toml
[servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }

[servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "~/projects"]
```

**2. Connect your AI client to plug** (instead of to each server individually):

For Claude Code (`.mcp.json` in your project root):

```json
{
  "mcpServers": {
    "plug": {
      "command": "plug",
      "args": ["connect"]
    }
  }
}
```

For Cursor, Windsurf, Gemini CLI, and others — see [docs/CLIENT-COMPAT.md](docs/CLIENT-COMPAT.md).

**3. That's it.** All your servers are available through every client simultaneously.

## Why plug?

You use 10 different AI coding tools. Each one needs its own MCP server configuration. Each one runs its own copies of the same servers. They conflict with each other. Configuration is scattered across a dozen files in different formats.

**plug** fixes this:

- **One config** — define your servers once in `~/.config/plug/config.toml`
- **Every client** — Claude Code, Cursor, Gemini CLI, Codex, Windsurf, VS Code Copilot, OpenCode, Zed
- **Shared connections** — N clients share 1 upstream connection per server (not N connections)
- **Client-aware** — automatically respects per-client tool limits (Windsurf: 100, VS Code: 128)
- **Zero dependencies** — single static binary, no Docker, no database, no account required

## Commands

```sh
plug connect              # Start the MCP proxy (for AI clients to invoke)
plug status               # Show server health and connected clients
plug server list          # List configured servers
plug tool list            # List all available tools across all servers
plug tool list --output json  # Machine-readable output for agent use
plug config validate      # Validate your config file
plug config path          # Show config file location
plug import --all         # Import MCP servers from all installed AI clients
plug import cursor        # Import from a specific client
plug export claude-code   # Generate config snippet for a client
plug doctor               # Diagnose connectivity and configuration issues
plug reload               # Hot-reload config without restarting
plug serve --daemon       # Run as headless daemon with IPC
plug serve                # Run with TUI dashboard
```

## Configuration

Full configuration reference:

```toml
# ~/.config/plug/config.toml

# Global settings
enable_prefix = true       # Prefix tool names with server name (default: true)
prefix_delimiter = "__"    # Delimiter between server name and tool name

[http]
bind_address = "127.0.0.1"
port = 3282

[servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "$GITHUB_TOKEN" }

[servers.notion]
command = "npx"
args = ["-y", "@notionhq/notion-mcp-server"]
env = { NOTION_API_KEY = "$NOTION_API_KEY" }

[servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "~/projects"]

[servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "$DATABASE_URL"]
env = { DATABASE_URL = "$DATABASE_URL" }
max_concurrent = 1         # Limit concurrent requests
enrichment = true          # Infer tool annotations from name patterns
```

Environment variable references (`$VAR_NAME`) in config values are expanded at startup.

## Documentation

| Document | Purpose |
|----------|---------|
| [VISION.md](docs/VISION.md) | Core principles, design philosophy, non-negotiable rules |
| [USERS.md](docs/USERS.md) | Who uses this, user stories, personas, scenarios |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Technical architecture, component design, data flow |
| [MCP-SPEC.md](docs/MCP-SPEC.md) | MCP protocol reference relevant to implementation |
| [CLIENT-COMPAT.md](docs/CLIENT-COMPAT.md) | Every AI client's quirks, limits, and requirements |
| [COMPETITIVE.md](docs/COMPETITIVE.md) | Every competitor analyzed with gap analysis |
| [UX-DESIGN.md](docs/UX-DESIGN.md) | Human TUI + AI agent UX patterns and design |
| [CRATE-STACK.md](docs/CRATE-STACK.md) | Every dependency decision with rationale |
| [PLAN.md](docs/PLAN.md) | Phased implementation plan |
| [RESEARCH-BREADCRUMBS.md](docs/RESEARCH-BREADCRUMBS.md) | Open questions, edge cases, deeper research signals |

## Design Principles

1. **Single binary, zero dependencies** — `brew install plug && plug`
2. **Ruthlessly minimal** — if a feature can't be explained in one sentence, simplify it
3. **Dual-audience UX** — every command works for humans (pretty) AND agents (`--output json`)
4. **Token-efficient** — 5-layer optimization, client-aware tool filtering
5. **Clean pass-through** — faithful proxy by default, optional enrichment
6. **Rock-solid reliable** — circuit breakers, merge cache, graceful degradation
7. **Future-proof** — MCP 2025-11-25, ready for stateless mode (June 2026)

## Tech Stack

- **Language**: Rust (2024 edition)
- **MCP SDK**: rmcp (official Rust SDK)
- **TUI**: Ratatui + Crossterm
- **CLI**: Clap (derive pattern)
- **HTTP**: Axum + Tower + Hyper
- **Async**: Tokio (multi-threaded with work-stealing)
- **Config**: TOML via Figment (layered)

## License

Apache-2.0 — see [LICENSE](LICENSE)
