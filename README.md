# fanout

**One binary. Every client. Every server. Zero friction.**

A ruthlessly minimal MCP multiplexer built from scratch in Rust. The single point of connection between all your AI coding clients and all your MCP servers — simultaneously, concurrently, without conflicts.

```
brew install fanout && fanout
```

## What It Does

You use 10 different AI coding tools. Each one needs its own MCP server configuration. Each one runs its own copies of the same servers. They conflict with each other. Configuration is scattered across a dozen files in different formats.

**fanout** fixes this. One install. One config. Every client connected. Every server shared.

```
Claude Code ──┐                      ┌── github (12 tools)
Claude Code ──┤                      ├── notion (8 tools)
Cursor ───────┤── fanout ───────────┤── filesystem (4 tools)
Gemini CLI ───┤   (single binary)   ├── postgres (6 tools)
Codex ────────┤                      └── brave-search (1 tool)
OpenCode ─────┘
```

## Status

**Pre-development.** This repository contains the project specification, research, and implementation plan. Code has not been written yet.

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

## Tech Stack

- **Language**: Rust
- **MCP SDK**: rmcp v0.16.0 (official)
- **TUI**: Ratatui v0.30.0 + Crossterm
- **CLI**: Clap v4.5 (derive)
- **HTTP**: Axum + Tower + Hyper
- **Async**: Tokio (multi-threaded)
- **Config**: TOML via Figment

## License

TBD
