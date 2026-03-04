---
title: "feat: Phase 5 — Polish + Distribution"
type: feat
status: completed
date: 2026-03-04
---

# Phase 5: Polish + Distribution

## Overview

Final phase of plug MCP multiplexer. Adds config auto-import/export for 12+ AI clients, config hot-reload, doctor diagnostics, tool enrichment, release pipeline (cargo-dist + Homebrew), and server cards. Carries forward deferred TUI views (Log view, Doctor view) from Phase 4.

**Builds on**: Phase 1 (stdio proxy, PR #1), Phase 2 (HTTP transport, PR #2), Phase 3 (resilience + token efficiency, PR #3), Phase 4 (TUI dashboard + daemon, PR #4).

## Problem Statement

Users currently must manually write plug's `config.toml` to define their MCP servers. Most users already have servers configured in their AI clients (Claude Code, Cursor, VS Code, etc.) — importing these automatically eliminates the biggest friction point. Similarly, users connecting new clients need hand-crafted config snippets. And there's no way to install plug without building from source.

## Proposed Solution

Eight sub-features organized into 4 implementation sub-phases:

1. **Sub-phase A**: Config import/export + doctor (core UX)
2. **Sub-phase B**: Config hot-reload + tool enrichment (runtime features)
3. **Sub-phase C**: TUI log view + doctor view (deferred Phase 4 items)
4. **Sub-phase D**: Release pipeline + server cards (distribution)

## Technical Approach

### Architecture

All new business logic goes in `plug-core/`. CLI commands go in `plug/src/main.rs`. New modules:

| Module | Crate | Purpose |
|--------|-------|---------|
| `plug-core/src/import.rs` | plug-core | Client config scanners + dedup logic |
| `plug-core/src/export.rs` | plug-core | Client config generators |
| `plug-core/src/doctor.rs` | plug-core | Diagnostic checks |
| `plug-core/src/reload.rs` | plug-core | Config hot-reload (file watcher + diff) |
| `plug-core/src/enrichment.rs` | plug-core | Tool annotation inference |
| `plug/src/tui/widgets/logs.rs` | plug | TUI log view widget |
| `plug/src/tui/widgets/doctor.rs` | plug | TUI doctor view widget |

### Client Config Matrix (Import/Export)

| Client | Config Path(s) | Format | Schema Key | Transport |
|--------|---------------|--------|------------|-----------|
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS), `%APPDATA%\Claude\claude_desktop_config.json` (Win) | JSON | `mcpServers` | stdio, SSE, HTTP |
| Claude Code | `~/.claude.json` (global), `.mcp.json` (project) | JSON | `mcpServers` | stdio |
| Cursor | `~/.cursor/mcp.json` (global), `.cursor/mcp.json` (project) | JSON | `mcpServers` | stdio, SSE, HTTP |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | JSON | `mcpServers` | stdio, SSE, HTTP |
| VS Code Copilot | `~/.vscode/mcp.json`, `.vscode/mcp.json` (project) | JSON | `mcp.servers` | stdio, SSE, HTTP |
| Gemini CLI | `~/.gemini/settings.json` | JSON | `mcpServers` | stdio, SSE, HTTP |
| Codex CLI | `~/.codex/config.toml` | TOML | `mcp_servers` | stdio, HTTP |
| OpenCode | `~/.config/opencode/config.json` | JSON | `mcpServers` | SSE, HTTP |
| Zed | `~/.config/zed/settings.json` | JSON | `context_servers` | stdio |
| Cline | `~/.vscode/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` | JSON | `mcpServers` | stdio |
| Factory/Droid | CLI-managed (`~/.factory/config.json`) | JSON | `mcpServers` | stdio, HTTP |
| Nanobot/ZeroClaw | `~/.nanobot/config.toml` or project `.nanobot.toml` | TOML | `mcp_servers` | stdio |

**Notes**:
- Cline stores config in VS Code's extension globalStorage directory
- OpenCode, Factory, and Nanobot config paths need runtime verification — fall back gracefully if not found
- All JSON clients use a variant of `{"mcpServers": {"name": {"command": "...", "args": [...]}}}` except VS Code (`mcp.servers`) and Zed (`context_servers`)
- Codex and Nanobot use TOML with `[mcp_servers.name]` sections

### Import Algorithm

```
plug import [source] [--all] [--yes]
```

1. **Scan**: For each source (or all sources), check if config file exists at platform-appropriate path
2. **Parse**: Extract server definitions using source-specific parser
3. **Normalize**: Convert each entry to plug's `ServerConfig` struct
   - `command` + `args` → stdio transport
   - `url`/`httpUrl`/`sseUrl` → http transport
   - `env` map → server env vars (store as `$VAR_NAME` references, warn if literal values detected)
4. **Deduplicate**: Group by `(command, args)` tuple. Same command+args = same server even if different names
   - If env vars differ for same command+args, treat as distinct servers (append source suffix to name)
5. **Merge**: Compare against existing `config.toml`. Skip already-present servers (by command+args match). Report new/skipped counts.
6. **Write**: Append new `[servers.name]` entries to `config.toml`

**Naming**: Use the source name as-is. On collision, append source suffix: `github` → `github-cursor`.

**Security**: Never store literal secret values. If a source config has `"env": {"TOKEN": "sk-abc123"}`, convert to `"env": {"TOKEN": "$TOKEN"}` and warn the user to set the env var.

### Export Algorithm

```
plug export <target> [--write] [--project]
```

1. **Load**: Read current plug `config.toml`
2. **Transform**: Convert each `ServerConfig` to the target client's format
   - Replace `plug` command/args with the plug connect command
   - Generate the JSON/TOML structure matching the target's schema
3. **Output**: Print to stdout (default) or write to target's config file (`--write`)
   - `--write` merges into existing file (preserves non-plug entries)
   - `--project` writes to project-level path instead of global

**Supported targets**: `claude-desktop`, `claude-code`, `cursor`, `windsurf`, `vscode`, `gemini`, `codex`, `opencode`, `zed`, `cline`, `factory`, `nanobot`

**What export generates**: A config snippet that points the target client at plug:
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

For HTTP-capable clients, also offer:
```json
{
  "mcpServers": {
    "plug": {
      "url": "http://localhost:3282/mcp"
    }
  }
}
```

### Config Hot-Reload

**File watcher**: `notify` crate watching `config.toml` with 500ms debounce.

**SIGHUP handler**: Unix-only. Also add `plug reload` CLI command as cross-platform alternative (sends reload request via daemon Unix socket IPC).

**Diff algorithm**:
1. Load new config, validate it. If invalid, log error and keep current config.
2. Compare server lists: `old.servers` vs `new.servers`
3. For each server:
   - **New** (in new, not in old): Start it, add to ServerManager
   - **Removed** (in old, not in new): Mark as draining → reject new requests → wait for in-flight (30s timeout) → shutdown → remove from ServerManager
   - **Changed** (same name, different command/args/env/timeout): Restart (same as remove + add)
   - **Unchanged**: No action
4. Refresh tool cache via `ToolRouter::refresh_tools()`
5. `config.store(Arc::new(new_config))` via ArcSwap
6. Emit `EngineEvent::ConfigReloaded`
7. Send `list_changed` notification to all connected clients

**Settings changes** (bind address, port, etc.): Apply where possible. Bind address change = log warning ("restart required to change bind address").

### Doctor Command

```
plug doctor [--output json]
```

**Checks** (run in parallel where possible):

| Check | Pass | Warn | Fail |
|-------|------|------|------|
| Config file exists and is valid TOML | Valid | Missing (using defaults) | Parse error |
| Config file permissions | 0600/0644 | World-readable with secrets | Cannot read |
| Port 3282 available | Available | — | In use (show PID) |
| Env vars referenced in config | All set | — | Missing vars listed |
| Server connectivity (ping each) | Responds < 5s | Responds 5-25s | Timeout/error |
| Tool collision detection | No collisions | Prefix-resolved collisions | Unresolved collisions |
| Client tool limits | All within limits | Near limit (>80%) | Over limit |
| Server binary availability | `command` found in PATH | — | Not found |
| PID file staleness | Fresh or no PID | — | Orphaned PID file |

**Exit codes**: 0 = all pass, 1 = any fail, 2 = warnings only (no fails).

**Output**: Pretty text (default) with pass/warn/fail indicators and fix suggestions. JSON mode for agent consumption.

### Tool Enrichment

**Opt-in per server** in config.toml:
```toml
[servers.github]
command = "npx"
args = ["@modelcontextprotocol/server-github"]
enrichment = true  # default: false
```

**Annotation inference rules** (fill-in only — never override upstream values):

| Pattern | Inferred Annotation |
|---------|-------------------|
| `get_*`, `list_*`, `search_*`, `read_*`, `fetch_*` | `readOnlyHint: true` |
| `delete_*`, `remove_*`, `drop_*`, `destroy_*` | `destructiveHint: true` |
| `create_*`, `add_*`, `insert_*`, `set_*`, `update_*`, `write_*` | `readOnlyHint: false` |

**Name normalization**: Generate human-readable `title` field from `snake_case` tool names. `create_github_issue` → `"Create GitHub Issue"`. Only sets `title` if not already present.

**Implementation**: Enrichment applied during `ToolRouter::refresh_tools()` after merging upstream tools. Stored in the `RouterSnapshot` alongside base tools. Changes to enrichment config trigger `list_changed`.

### Server Cards

Add `GET /.well-known/mcp.json` handler to `build_router()` in `plug-core/src/http/server.rs`.

```json
{
  "name": "plug",
  "version": "0.1.0",
  "description": "MCP multiplexer",
  "tools": 47,
  "servers": ["github", "filesystem", "slack"],
  "transports": ["stdio", "streamable-http", "sse"]
}
```

Exempt from origin validation middleware (discovery endpoint). Cache response for 60s (invalidate on tool cache refresh).

### TUI Log View (Deferred from Phase 4)

New `AppMode::Logs` with:
- Full-screen structured log viewer
- Filter by level (debug/info/warn/error), server, client
- Scrollable with j/k navigation
- Activate with `l` from dashboard
- Requires log capture ring buffer in App state (last 1000 entries)

### TUI Doctor View (Deferred from Phase 4)

New `AppMode::Doctor` with:
- Full-screen diagnostic results display
- Run checks on entry, show progress
- Activate with `d` from dashboard
- Reuse `plug-core/src/doctor.rs` check functions

### Release Pipeline

**cargo-dist**: `cargo dist init` generates:
- `dist-workspace.toml` configuration
- `.github/workflows/release.yml` GitHub Actions workflow
- Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `x86_64-pc-windows-msvc`

**Homebrew tap**:
- Repository: `plug-mcp/homebrew-tap`
- Formula auto-generated by cargo-dist on release
- `brew tap plug-mcp/tap && brew install plug`

**Shell installer**:
- `curl -fsSL https://get.plug.dev | sh`
- Detects platform (uname), downloads correct binary from GitHub releases
- Verifies SHA-256 checksum against `.sha256` file
- Places binary in `~/.local/bin` or `/usr/local/bin`

**README**: Installation instructions, quick start, config example.

**Changelog**: Auto-generated from conventional commits via `git-cliff`.

## Implementation Phases

### Sub-phase A: Config Import/Export + Doctor

**Files to create/modify**:
- [x] `plug-core/src/import.rs` — Client config scanners, parser trait, 12 client implementations, dedup algorithm
- [x] `plug-core/src/export.rs` — Config generators for each target client
- [x] `plug-core/src/doctor.rs` — Diagnostic check framework, 9 check implementations
- [x] `plug-core/src/lib.rs` — Add `pub mod import; pub mod export; pub mod doctor;`
- [x] `plug/src/main.rs` — Add `Import`, `Export`, `Doctor` commands to CLI
- [x] Tests for each parser (sample config fixtures per client)

**Success criteria**:
- `plug import --all` discovers and imports servers from all installed clients
- `plug export cursor` generates valid Cursor config
- `plug doctor` reports all checks with pass/warn/fail

### Sub-phase B: Config Hot-Reload + Tool Enrichment

**Files to create/modify**:
- [x] `plug-core/src/reload.rs` — File watcher, config diff, server drain logic
- [x] `plug-core/src/enrichment.rs` — Annotation inference, name normalization
- [x] `plug-core/src/engine.rs` — Add `reload_config()` method, spawn file watcher task
- [x] `plug-core/src/config/mod.rs` — Add `enrichment` field to `ServerConfig`
- [x] `plug-core/src/proxy/mod.rs` — Apply enrichment in `refresh_tools()`
- [x] `plug/src/daemon.rs` — Add SIGHUP handler, `Reload` IPC request
- [x] `plug/src/main.rs` — Add `plug reload` CLI command
- [ ] `Cargo.toml` (workspace) — Add `notify` dependency

**Success criteria**:
- Edit config.toml while plug is running → servers added/removed/restarted automatically
- `kill -HUP <pid>` triggers reload
- `plug reload` triggers reload via IPC
- Enrichment adds annotations to tools when enabled per-server
- Invalid config edits are rejected (keep running with previous config)

### Sub-phase C: TUI Log View + Doctor View

**Files to create/modify**:
- [x] `plug/src/tui/widgets/logs.rs` — Full-screen log viewer widget
- [x] `plug/src/tui/widgets/doctor.rs` — Doctor results widget
- [x] `plug/src/tui/widgets/mod.rs` — Register new widget modules
- [x] `plug/src/tui/app.rs` — Add `AppMode::Logs`, `AppMode::Doctor`, log ring buffer, keybindings
- [x] `plug/src/tui/mod.rs` — Wire up new modes in view dispatch

**Success criteria**:
- `l` key opens log view with level/server filtering
- `d` key opens doctor view with live check results
- Both views scrollable and dismissible with Esc

### Sub-phase D: Release Pipeline + Server Cards

**Files to create/modify**:
- [x] `plug-core/src/http/server.rs` — Add `/.well-known/mcp.json` handler
- [x] `dist-workspace.toml` — cargo-dist configuration (generated)
- [x] `.github/workflows/release.yml` — CI release workflow (generated)
- [x] `install.sh` — Shell installer script
- [ ] `README.md` — Installation + quick start docs
- [x] `CHANGELOG.md` — Auto-generated changelog

**Success criteria**:
- `GET /.well-known/mcp.json` returns valid server card
- `cargo dist build` produces binaries for all 7 targets
- GitHub Actions releases on tag push
- `brew install` works from tap

## System-Wide Impact

### Interaction Graph

- **Import** reads external files → writes `config.toml` → (if running) triggers hot-reload → starts new servers → refreshes tool cache → emits `ToolCacheRefreshed` → TUI updates
- **Hot-reload** watches `config.toml` → diffs config → starts/stops/restarts servers → refreshes tools → sends `list_changed` to connected clients → clients re-fetch tool lists
- **Enrichment** hooks into `ToolRouter::refresh_tools()` → modifies tool annotations in `RouterSnapshot` → affects what clients receive in `tools/list`
- **Doctor** reads config + queries ServerManager + checks ports + scans env → produces diagnostic report → TUI doctor view subscribes to results

### Error & Failure Propagation

- **Import parse failure**: Report which file failed, which line, continue scanning other clients
- **Hot-reload invalid config**: Reject entirely, log error, keep current config, emit `EngineEvent::Error`
- **Hot-reload server start failure**: Log error, keep other servers running, report via TUI/event bus
- **Doctor check timeout**: 5s per server ping (parallel). Total doctor timeout: 30s. Report timed-out servers as `Fail`.
- **Export write failure**: Report error, never partially write (use temp file + rename)

### State Lifecycle Risks

- **Hot-reload draining**: Server marked as draining continues processing in-flight requests for up to 30s. New requests to that server get `ServerUnavailable` error. After drain timeout, server is forcefully stopped.
- **ArcSwap config swap**: Atomic — readers always see consistent config. No torn reads possible.
- **Tool cache during reload**: Brief window where tool list may be stale. `refresh_tools()` is called after server changes complete.

## Acceptance Criteria

### Functional Requirements

- [x] `plug import claude-desktop` imports servers from Claude Desktop config
- [x] `plug import --all --yes` imports from all detected clients non-interactively
- [x] `plug export cursor` prints valid Cursor MCP config to stdout
- [ ] `plug export vscode --write` merges into existing VS Code config
- [x] `plug doctor` runs 9+ checks with pass/warn/fail output
- [x] `plug doctor --output json` returns structured results with exit code 0/1/2
- [x] Config hot-reload: add server to config.toml → server starts within 2s
- [x] Config hot-reload: remove server → drains in-flight, stops gracefully
- [x] Config hot-reload: invalid TOML → rejected, current config preserved
- [x] `plug reload` CLI command triggers reload via daemon IPC
- [ ] SIGHUP triggers config reload (Unix only)
- [x] Tool enrichment adds annotations when `enrichment = true` in server config
- [x] Enrichment never overrides upstream-provided annotations
- [x] `GET /.well-known/mcp.json` returns server card
- [x] TUI `l` key opens log view with filtering
- [x] TUI `d` key opens doctor view
- [x] Binary < 10 MB (release profile)
- [x] Startup < 1 second
- [x] Tool call overhead < 5ms for cached routes

### Quality Gates

- [x] All tests pass (`cargo test`)
- [x] Clippy clean (`cargo clippy -- -D warnings`)
- [x] `cargo fmt --check` passes
- [x] Import parsers tested with fixture files for each client format
- [x] Doctor checks tested with mock scenarios
- [ ] Hot-reload tested with concurrent requests during server removal

## Dependencies & Prerequisites

**New crate dependencies**:
- `notify` — filesystem watcher for hot-reload
- `git-cliff` — changelog generation (build-time tool, not runtime dependency)

**cargo-dist** — installed as cargo subcommand for release pipeline setup.

**External**:
- GitHub repository access for Actions workflow
- Domain `get.plug.dev` for shell installer (optional — can use GitHub raw URL)
- Homebrew tap repository (`plug-mcp/homebrew-tap`)

## Risk Analysis & Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| Client config format changes | Import breaks silently | Version-aware parsers, graceful fallback, fixture tests per client |
| `notify` cross-platform quirks | Hot-reload unreliable on some OS | Debounce (500ms), `plug reload` as manual fallback, SIGHUP on Unix |
| Enrichment false positives | Tools misannotated | Fill-in only (never override), conservative patterns, per-server opt-in |
| cargo-dist breaking changes | Release pipeline fails | Pin cargo-dist version, test in CI before merge |
| Undocumented client configs (Factory, Nanobot) | Import can't find config | Graceful skip with "not found" message, add support in patch releases |

## Institutional Learnings to Apply

From `docs/solutions/integration-issues/`:

1. **ArcSwap atomic swap** for config reload — group all config-dependent data in single struct, swap atomically (Phase 3 learning)
2. **DashMap entry API** for atomic check-and-update — use for draining server state during hot-reload (Phase 4 learning)
3. **SecretString with `#[serde(transparent)]`** — use for auth tokens in import/export, never log literal values (Phase 3 learning)
4. **UTF-8 safe env var expansion** — use `char.len_utf8()` not byte increments when parsing config (Phase 3 learning)
5. **rmcp non-exhaustive structs** — use builder methods (`fn new`, `fn with_*`) not struct literals for Tool construction in enrichment (Phase 1 learning)
6. **Figment `__` split delimiter** — already correct in current config, maintain when adding new fields (Phase 2 learning)
7. **`directories` crate (not `dirs`)** — use for platform-appropriate config paths in import scanner (Phase 4 learning)
8. **Broadcast `Arc<str>` for event strings** — maintain O(1) clone on fan-out for new events (Phase 4 learning)

## Sources & References

### Internal References

- Config loading: `plug-core/src/config/mod.rs:233-267`
- Client detection: `plug-core/src/client_detect.rs`
- Engine ArcSwap config: `plug-core/src/engine.rs:109`
- HTTP router builder: `plug-core/src/http/server.rs:38-44`
- CLI command pattern: `plug/src/main.rs:10-179`
- Client config paths: `docs/CLIENT-COMPAT.md:349-363`
- UX design: `docs/UX-DESIGN.md:58-63,278-368`
- Open questions: `docs/RESEARCH-BREADCRUMBS.md:E8,E20,E25`
- Crate stack: `docs/CRATE-STACK.md:147-193`

### Prior Phase Learnings

- `docs/solutions/integration-issues/rmcp-sdk-integration-patterns-plug-20260303.md`
- `docs/solutions/integration-issues/phase3-resilience-token-efficiency.md`
- `docs/solutions/integration-issues/phase4-tui-dashboard-daemon-patterns.md`
- `docs/solutions/integration-issues/mcp-multiplexer-http-transport-phase2.md`

### Related Work

- Phase 1: PR #1, Phase 2: PR #2, Phase 3: PR #3, Phase 4: PR #4
- PLAN.md sections 5.1-5.8
