# Vision, Principles, and Rules

## The Problem

A power user in 2026 uses 5-15 different AI coding and agent clients: Claude Code, Claude Desktop, Cursor, Gemini CLI, Codex, OpenCode, Windsurf, VS Code Copilot, Zed, Factory/Droid, and more. Each client needs MCP servers configured independently. Each runs its own copies of the same servers. They conflict on ports, duplicate processes, scatter configuration across a dozen files in different formats, and cannot share server state.

The result: configuration hell, resource waste, port conflicts, inconsistent tool availability, and hours lost debugging why "it works in Claude but not in Cursor."

## The Solution

**fanout** is a single Rust binary that sits between all your AI clients and all your MCP servers. One install, one config file, every client connected, every server shared — simultaneously, concurrently, without conflicts.

```
$ brew install fanout && fanout
```

That's it. You're done.

---

## Core Identity

fanout is **the lazygit of MCP**. It is:
- A personal tool, not an enterprise platform
- A single binary, not a distributed system
- A multiplexer, not a framework
- Beautiful but not decorative
- Opinionated but not rigid

---

## Design Principles (Non-Negotiable)

These are not aspirations. They are constraints. Every design decision must satisfy all of them. If a decision violates any principle, it is wrong.

### 1. Ruthlessly Minimal

Every feature must justify its existence. Every config option must prove it can't be a sensible default. Every line of code must earn its place.

- One binary. Zero runtime dependencies.
- One config file. TOML. Human-readable.
- The simplest thing that could possibly work, and no simpler.
- If you're adding a feature "just in case" — don't.
- If you're adding an abstraction for one use case — don't.
- Three lines of similar code is better than a premature abstraction.

**Test**: Can you explain this feature to someone in one sentence? If not, simplify it.

### 2. Zero-Friction

The distance from "I heard about fanout" to "all my clients are connected" should be measured in seconds, not minutes.

- `brew install fanout && fanout` must reach a working state.
- Auto-detect existing MCP configs on first run. Import with one keystroke.
- Sensible defaults for everything. Configuration is for customization, never for basic operation.
- No mandatory config file editing. No YAML. No Docker. No databases.
- The first-run experience IS the product.

**Test**: Can a developer who has never heard of fanout go from zero to all-clients-connected in under 60 seconds?

### 3. Dual-Audience UX

Humans and AI agents are equal citizens. Every interface must serve both.

- TUI for humans: beautiful, discoverable, real-time.
- `--output json` for agents: structured, machine-parseable, deterministic.
- Error messages that help humans AND provide machine-readable error codes.
- No interactive prompts that would break agent workflows. Always provide `--yes`, `--non-interactive`.
- The same binary serves both audiences. No separate "agent mode" binary.

**Test**: Can Claude Code configure and manage fanout entirely through `fanout --output json` commands without human intervention?

### 4. Token-Efficient

AI clients have limited context windows. Every token wasted on tool definitions is a token stolen from actual work.

- Default tool serving must minimize token usage (lazy schemas, search mode).
- Client-aware: Cursor gets 40 tools, Windsurf gets 100, Claude Code gets search.
- The multiplexer should be invisible to the context window — near-zero overhead.
- Tool descriptions should be concise and meaningful, not verbose and redundant.

**Test**: Does connecting through fanout use fewer tokens than connecting directly? (It must.)

### 5. Clean Pass-Through

fanout is a multiplexer, not a transformer. The default behavior is faithful proxying.

- Every MCP method passes through unmodified unless the user explicitly opts into transformation.
- Tool annotations, icons, structured output, resource links — all pass through.
- Optional enrichment (name normalization, annotation inference) is opt-in, never default.
- If an upstream server sends it, the client should receive it. Period.

**Test**: Does removing fanout from the chain change the behavior of any tool call? (It must not, unless the user configured it to.)

### 6. Rock-Solid Reliable

This tool sits in the critical path of every AI interaction. It must never be the reason something breaks.

- Graceful degradation: if one server dies, the others keep working.
- Merge-based caching: tools from a timed-out server are preserved from last known state.
- Circuit breakers: stop hammering a dead server.
- Proper shutdown: no orphaned processes, no dangling connections.
- Reconnection with backoff: automatic recovery from transient failures.
- No panics in production paths. Every error is handled.

**Test**: Can you kill a random upstream server and have zero impact on tool calls to other servers?

### 7. Future-Proof

The MCP spec is evolving fast. fanout must be ready for what's coming without over-engineering for what might never arrive.

- Current spec (2025-11-25) fully supported.
- Designed for stateless mode (June 2026 anticipated).
- Server Cards endpoint (`.well-known/mcp.json`) ready.
- Transport-agnostic core: adding a new transport should be a plugin, not a rewrite.
- But: don't implement speculative features. Build hooks, not implementations.

**Test**: When the next MCP spec drops, can we support it by adding a module, not by refactoring the core?

---

## Anti-Principles (Things We Will Never Do)

1. **Never require Docker** — we are a single binary, always.
2. **Never require a database** — config is a TOML file, state is in-memory.
3. **Never require an account or cloud service** — this is local-first, always.
4. **Never add enterprise features at the cost of simplicity** — RBAC, multi-tenancy, OIDC belong in a different product.
5. **Never break the pass-through contract** — unless the user explicitly opted in.
6. **Never add a feature without a clear user story** — "it would be cool" is not a user story.
7. **Never sacrifice startup time** — the binary must be ready in < 1 second.
8. **Never log secrets** — tokens, keys, and credentials must never appear in logs.
9. **Never require sudo/admin** — run on port 3282 (above 1024), no elevated privileges.

---

## Quality Bar

### What "Done" Looks Like

- Every feature has a user story from USERS.md.
- Every error has a clear, actionable message.
- Every CLI command works with `--output json`.
- Every config option has a sensible default.
- The TUI teaches you how to use it while you use it.
- The binary is < 10 MB (release, stripped).
- Startup to ready in < 1 second.
- Tool call overhead < 5ms.
- Zero unsafe code in application logic (only in well-audited dependencies).

### What "Best on the Market" Looks Like

- A developer tries it and says "why didn't this exist before?"
- An AI agent connects and everything just works — no special handling needed.
- The TUI is screenshot-worthy. People share it on Twitter.
- The token efficiency is measurably better than any alternative.
- The reliability is boring — it never comes up because it never fails.
- The codebase is small enough that one person can understand the whole thing.

---

## Project Boundaries

### In Scope

- MCP protocol multiplexing (N clients, M servers)
- stdio, Streamable HTTP, and legacy SSE transports (both directions)
- Tool routing, fan-out, caching, and conflict resolution
- Client-aware tool filtering and token optimization
- Beautiful TUI dashboard + headless daemon mode
- CLI with structured JSON output for AI agents
- Config import/export from all major AI clients
- Health monitoring, circuit breaking, graceful degradation
- `.localhost` subdomain routing (Portless-native)
- Cross-platform: macOS (ARM + Intel), Linux, Windows

### Out of Scope (For Now)

- OAuth proxy for upstream servers (upstream handles its own auth)
- Multi-user / multi-tenant support
- Cloud deployment dashboard
- Plugin/extension system (keep it monolithic until proven otherwise)
- GUI (native app) — the TUI IS the GUI
- Upstream server management (installing/updating MCP servers themselves)

### Maybe Later

- Wasm plugin model for custom transforms
- Remote management API (for headless server deployments)
- Metrics export (Prometheus endpoint)
- Integration test framework for MCP servers
