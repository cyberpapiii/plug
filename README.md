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

### Local development reinstall

When working on `plug` locally, use the repo script instead of manually copying binaries:

```sh
./scripts/dev-reinstall.sh
```

This rebuilds the workspace, reinstalls `plug`, and normalizes `~/.local/bin/plug`
to a symlink pointing at `~/.cargo/bin/plug` so the PATH binary stays in sync.

### Manual

Download the binary for your platform from the [releases page](https://github.com/plug-mcp/plug/releases), verify the SHA-256 checksum, and place it in your PATH.

## Quick Start

**1. Run the guided setup flow**:

```sh
plug setup
```

This discovers existing MCP servers, imports them into `plug`, and walks you through linking your AI clients.

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

**2. Link an AI client to plug** (instead of to each server individually):

Interactive:

```sh
plug link
```

Non-interactive:

```sh
plug link claude-code cursor
```

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
- **Lazy tool discovery** — clients like OpenCode can start with a tiny search/load bridge instead of seeing hundreds of tool schemas up front
- **Zero dependencies** — single static binary, no Docker, no database, no account required
- **OAuth built in** — authenticate to remote MCP servers with `plug auth login`, background token refresh handles the rest
- **Every transport** — upstream stdio, HTTP, and legacy SSE; downstream stdio and Streamable HTTP/HTTPS

## Commands

```sh
plug                         # Show a compact overview and next actions
plug start                   # Start the shared background service (IPC + HTTP)
plug setup                   # Discover servers and link clients
plug clients                 # View and manage linked, detected, and live clients
plug servers                 # View and manage configured servers
plug tools                   # View and manage the effective tool surface
plug status                  # Show runtime health and next useful action
plug doctor                  # Diagnose connectivity and configuration issues
plug repair                  # Refresh linked client configuration files
plug config check            # Validate config syntax and core rules
plug tools disable --server slack
plug tools enable --server slack
plug tools --output json     # Machine-readable output for agent use
plug auth login --server name  # OAuth login for remote MCP servers
plug auth status               # Show per-server auth status
plug connect                   # Internal stdio adapter AI clients invoke
plug serve                     # Run standalone HTTP/HTTPS in the foreground
plug serve --daemon            # Run the shared background service (IPC + HTTP)
```

## Configuration

Full configuration reference:

```toml
# ~/.config/plug/config.toml

# Global settings
enable_prefix = true       # Legacy compatibility field; tool names are always prefixed in v0.1
prefix_delimiter = "__"    # Delimiter between server name and tool name
daemon_grace_period_secs = 0  # Default: keep the shared daemon alive until explicit shutdown

[lazy_tools]
mode = "auto"              # auto, standard, native, bridge

[lazy_tools.clients]
opencode = "bridge"        # search/load/evict bridge, then direct-call loaded routed tools
"claude-code" = "native"   # let native client-side lazy discovery handle large catalogs
"codex-cli" = "native"

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

# Optional naming controls per server
[servers.workspace]
transport = "http"
url = "http://localhost:8000/mcp"

  [servers.workspace.tool_renames]
  search_docs = "get_doc_search_results"

  [[servers.workspace.tool_groups]]
  prefix = "Gmail"
  contains = ["gmail"]
  strip = ["gmail"]

# Remote HTTP server with OAuth authentication
[servers.remote-notion]
transport = "http"
url = "https://mcp.notion.so/mcp"
auth = "oauth"
oauth_scopes = ["mcp:read", "mcp:write"]
```

Environment variable references (`$VAR_NAME`) in config values are expanded at startup.

### Lazy Tool Discovery

`plug` can choose a lazy tool mode per downstream client:

- `standard`: expose the normal routed tool catalog.
- `native`: expose the normal routed catalog and let clients like Claude Code, Cursor, or Codex apply their own deferred tool loading.
- `bridge`: expose a compact discovery surface first, then let the session load real routed tools by name.

OpenCode defaults to `bridge`, so it initially sees only compact `plug__*` discovery tools such as `plug__search_tools`, `plug__load_tool`, `plug__evict_tool`, and `plug__list_loaded_tools`. After `plug__load_tool`, the loaded tool appears with its normal routed name, for example `Slack__search_messages`, and is called directly rather than through a permanent wrapper.

Use `plug clients` to inspect the resolved mode and whether it came from an automatic default, global override, or per-client override.

### Tool Naming And Display

`plug` exposes MCP tools with a stable prefixed wire `name` and separate human-facing display metadata:

- `name`: stable machine identifier used for routing and tool calls, e.g. `Slack__channels_list`
- `title`: canonical display label generated by `plug`, e.g. `Slack: Channels List`
- `annotations.title`: compatibility display label; `plug` normalizes this to match the canonical top-level `title` for merged tools

Notes:

- Wire names are always prefixed in v0.1, regardless of `enable_prefix`
- Some servers can be split into sub-service prefixes via `tool_groups`
- Some clients still render raw `name` or synthesize their own labels, so perfect cross-client visual consistency is not always possible

## Documentation

| Document | Purpose |
|----------|---------|
| [PROJECT-STATE-SNAPSHOT.md](docs/PROJECT-STATE-SNAPSHOT.md) | Current state of what is implemented on `main` |
| [PLAN.md](docs/PLAN.md) | Current state and remaining work |
| [VISION.md](docs/VISION.md) | Core principles, design philosophy, non-negotiable rules |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | Technical architecture, component design, data flow |
| [CLIENT-COMPAT.md](docs/CLIENT-COMPAT.md) | AI client quirks, limits, and configuration |
| [UX-DESIGN.md](docs/UX-DESIGN.md) | Guided CLI + agent UX patterns |
| [CRATE-STACK.md](docs/CRATE-STACK.md) | Dependency decisions with rationale |
| [MCP-SPEC.md](docs/MCP-SPEC.md) | MCP protocol reference relevant to implementation |
| [USERS.md](docs/USERS.md) | User stories, personas, scenarios |
| [COMPETITIVE.md](docs/COMPETITIVE.md) | Competitor analysis |
| [RESEARCH-BREADCRUMBS.md](docs/RESEARCH-BREADCRUMBS.md) | Open questions and deeper research signals |

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
- **CLI**: Clap (derive pattern)
- **HTTP**: Axum + Tower + Hyper
- **Async**: Tokio (multi-threaded with work-stealing)
- **Config**: TOML via Figment (layered)

## License

Apache-2.0 — see [LICENSE](LICENSE)
