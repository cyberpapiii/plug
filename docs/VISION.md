# Vision, Principles, and Rules

## The Problem

A power user in 2026 often uses 5-15 AI coding and agent clients: Claude Code, Cursor, Gemini CLI, Codex, OpenCode, Windsurf, VS Code Copilot, Zed, and more. Each client wants MCP configured differently. Each tends to spawn its own copies of the same servers. The result is duplicated config, duplicate processes, port conflicts, inconsistent tool availability, and wasted debugging time.

## The Solution

`plug` is a Rust daemon that sits between AI clients and MCP servers. One install, one config, one place to reason about your MCP setup.

Today, the product surface is:

- Plug.app, the menu bar app that bundles the daemon and owns its lifecycle
- the `plug` CLI for scripting, agents, and advanced use
- a strong core multiplexer behind both

---

## Core Identity

`plug` is:

- a personal tool, not an enterprise platform
- one daemon, not a distributed system
- a multiplexer, not a framework
- operationally boring, not flashy
- opinionated at the UX layer, but simple at the systems layer

---

## Design Principles

### 1. Ruthlessly Minimal

Every feature must justify its existence.

- One binary. Zero runtime dependencies.
- One config file. TOML. Human-readable.
- Prefer sensible defaults over additional flags.
- Prefer straightforward code over speculative abstraction.

**Test**: Can this feature be explained in one sentence?

### 2. Zero-Friction

The distance from “I installed plug” to “my clients are using it” should be short.

- `plug setup` must do useful work immediately.
- The no-args `plug` experience must guide the user toward the next action.
- Configuration is for customization, not basic operation.
- No Docker, no database, no required cloud account.

**Test**: Can a new user get to a working setup quickly without reading architecture docs?

### 3. Dual-Audience UX

Humans and agents are equal citizens.

- Humans get a guided CLI with clear next actions.
- Agents get deterministic `--output json` behavior.
- Interactive flows should have non-interactive equivalents for the main jobs.
- The same binary serves both audiences.

**Test**: Can an agent inspect and manage the core setup flows without depending on prompts?

### 4. Clean Pass-Through

`plug` is a multiplexer, not a product that rewrites everything upstream.

- MCP behavior should pass through faithfully by default.
- Optional enrichment must be explicit or tightly scoped.
- If upstream sends it, downstream should receive it unless `plug` is deliberately adding value.

**Test**: Does removing `plug` from the chain preserve tool behavior in the default case?

### 5. Rock-Solid Reliability

`plug` sits in the critical path of AI workflows. It must not be the fragile part.

- One bad server should not poison the rest.
- Daemon/runtime behavior should recover from transient failures.
- Shutdown should be clean.
- Errors should be actionable.

**Test**: Can a server fail without turning the whole product into a mystery?

### 6. App First, CLI Underneath

Plug.app is the product surface. It installs and runs the daemon, shows what
is working, and fixes what is not.

- Everyday jobs (add a server, sign in, see status, restart) belong in the app.
- The CLI is for scripting, agents (`--output json`), and advanced use. It talks
  to the same daemon and never needs the app window open.
- Neither surface hides state the other can see.

**Test**: Could someone use Plug for a week without opening a terminal?

---

## Anti-Principles

Things `plug` should not do in this phase:

1. Require Docker.
2. Require a database.
3. Require a cloud account.
4. Grow a second UI beside the app.
5. Center transport plumbing in the user-facing story.
6. Add enterprise features at the cost of simplicity.

---

## What “Done” Looks Like

- The command surface maps cleanly to user jobs.
- `plug` with no args is useful.
- `setup`, `link`, `status`, and `doctor` form a coherent human workflow.
- Main admin/setup flows have non-interactive equivalents.
- Docs describe the product as it actually exists.
- The backend remains small enough to understand and reliable enough to trust.

## What “Best On The Market” Looks Like

- A developer uses `plug` and stops thinking about MCP wiring.
- An agent can inspect and manage the runtime without brittle prompt parsing.
- The CLI feels intentional and calm, not like exposed internals.
- Reliability is boring.

---

## Scope

### In Scope

- MCP protocol multiplexing
- Shared runtime and daemon behavior
- Import/export/linking with major AI clients
- Guided CLI for humans
- Structured CLI output for agents
- Health monitoring, recovery, and diagnostics

### Out of Scope For This Phase

- Cloud management features
- Multi-user or enterprise access control
- Turning `plug` into an upstream server installer/manager
