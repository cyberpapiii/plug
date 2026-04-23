# Client Compatibility Reference

This is the definitive reference for every AI client's MCP behavior, quirks, and requirements. `plug` must handle ALL of these correctly.

## Current Naming Contract

`plug` intentionally keeps the current mixed routed naming model:

- most servers use `ServerName__tool_name`
- the `workspace` server is decomposed into sub-service prefixes such as `Gmail__...` and `GoogleDocs__...`

Treat this as current truth when reasoning about client display behavior.

---

## Client Matrix

| Client | Transport | Tool Limit | Config Format | Config Location |
|--------|-----------|-----------|---------------|-----------------|
| Claude Code | stdio | None (tool search at >10% ctx) | JSON | `.mcp.json` or `~/.claude.json` |
| Claude Desktop | stdio, SSE, Streamable HTTP | None | JSON | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Cursor | stdio, SSE, Streamable HTTP | **None** (Dynamic Context Discovery) | JSON | `.cursor/mcp.json` or `~/.cursor/mcp.json` |
| Windsurf | stdio, SSE, Streamable HTTP | **100** | JSON | `~/.codeium/windsurf/mcp_config.json` |
| VS Code Copilot | stdio, SSE, Streamable HTTP | **128** | JSON | `.vscode/mcp.json` or settings |
| Gemini CLI | stdio, SSE, Streamable HTTP | None documented | JSON | `~/.gemini/settings.json` |
| Codex CLI | stdio, Streamable HTTP | None documented | TOML | `~/.codex/config.toml` |
| OpenCode | SSE, Streamable HTTP | None documented | Config file | Varies |
| Zed | stdio only | None documented | JSON | `settings.json` |
| Factory/Droid | stdio, HTTP | None documented | CLI config | Varies |

---

## Lazy Tool Discovery Defaults

`plug` resolves a lazy tool policy per client target. The defaults are intentionally conservative:

| Client | Default lazy mode | Why |
|--------|-------------------|-----|
| Claude Code | `native` | Claude Code has native deferred tool discovery and should receive the normal routed catalog. |
| Cursor | `native` | Cursor has Dynamic Context Discovery and should receive the normal routed catalog. |
| Codex CLI | `native` | Codex has native lazy/deferred tool search semantics and should receive the normal routed catalog. |
| OpenCode | `bridge` | OpenCode currently benefits from a `plug`-owned small bridge surface instead of seeing hundreds of tools eagerly. |
| Claude Desktop | `standard` | No proven lazy/deferred path in `plug`; keep normal full-tool behavior. |
| Windsurf | `standard` | Keep normal behavior with existing tool-limit filtering. |
| VS Code Copilot | `standard` | Keep normal behavior with existing tool-limit filtering. |
| Gemini CLI | `standard` | Keep normal behavior and prioritize fast discovery responses. |
| Zed | `standard` | Keep normal behavior. |
| Unknown clients | `standard` | Safe fallback: do not hide tools unless the client target is known or configured. |

Operators can inspect resolved modes with `plug clients` and override them in config:

```toml
[lazy_tools]
mode = "auto" # auto, standard, native, bridge

[lazy_tools.clients]
opencode = "bridge"
"claude-code" = "native"
```

Mode meanings:

- `standard`: expose the normal routed tool catalog, subject to existing client-specific filtering.
- `native`: expose the normal routed tool catalog and rely on the client to perform its own deferred/lazy loading.
- `bridge`: expose only compact `plug__*` discovery tools until a session loads specific real routed tools.

Bridge clients initially see:

- `plug__list_servers`
- `plug__list_tools`
- `plug__search_tools`
- `plug__load_tool`
- `plug__evict_tool`
- `plug__list_loaded_tools`
- `plug__invoke_tool`

The intended bridge flow is `plug__search_tools` -> `plug__load_tool` -> direct call to the real routed tool name. `plug__invoke_tool` remains available for fallback/debug use, but it is not the primary lazy-loading UX.

---

## Detailed Client Profiles

### Claude Code

**Transport**: stdio via `plug connect`
**Config format**: JSON
```json
// .mcp.json (project-level) or ~/.claude.json (global)
{
  "mcpServers": {
    "plug": {
      "command": "plug",
      "args": ["connect"]
    }
  }
}
```

**Behavior**:
- Supports roots, sampling
- Tool Search: when tool definitions exceed ~10% of context window, Claude Code switches to lazy-loading — it sends tool descriptions only (no schemas) and fetches full schemas on demand
- This is the ideal client for `plug` — it already handles large tool sets gracefully

**plug implications**:
- Default lazy mode: `native`
- Return the normal routed tool catalog; Claude Code manages token efficiency itself
- Support native tool search behavior if Claude Code sends it

---

### Claude Desktop

**Transport**: stdio (local), SSE or Streamable HTTP (remote)
**Config format**: JSON
```json
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "plug": {
      "command": "plug",
      "args": ["connect"]
    }
  }
}
```

**Behavior**:
- Supports resources, prompts, tools, roots
- Can connect to remote MCP servers via HTTP
- SSE may be deprecated in future Claude Desktop versions

**CRITICAL: Remote connector limitations (discovered 2026-03-10)**:
- Remote MCP connector sends `tools/list` once and ignores `nextCursor` — does NOT follow pagination
- Does NOT open SSE stream (GET /mcp), so never receives `tools/list_changed` notifications
- Workaround: set PAGE_SIZE large enough (500+) so all tools fit in a single page
- After config changes (enabling/disabling upstream servers), remote clients must disconnect and reconnect to see updated tool lists
- See: `docs/bug-reports/pagination-cursor-forwarding-and-remote-client-blanking.md`

**plug implications**:
- Standard stdio connection via `plug connect`
- For remote HTTP: ensure all tools fit in one page (PAGE_SIZE >= total tool count)
- Cannot rely on `tools/list_changed` notifications reaching remote Claude Desktop clients

---

### Cursor

**Transport**: stdio, SSE, Streamable HTTP
**Config format**: JSON
```json
// .cursor/mcp.json (project) or ~/.cursor/mcp.json (global)
{
  "mcpServers": {
    "plug": {
      "command": "plug",
      "args": ["connect"]
    }
  }
}
```

**UPDATED (2026-03-03): 40-TOOL LIMIT ELIMINATED**:
- Cursor released "Dynamic Context Discovery" in January 2026
- Users report 80+ tools working without warnings
- 46.9% reduction in agent token usage
- The old 40-tool limit only applies to pre-v2.3 Cursor
- `clientInfo.name`: `cursor-vscode`

**fanout implications**:
- No tool limit filtering needed for current Cursor versions
- Still detect Cursor via `clientInfo.name` for any future version-aware behavior
- Keep configurable tool limits as a safety valve
- Default lazy mode: `native`

**Forum reference**: https://forum.cursor.com/t/mcp-server-40-tool-limit-in-cursor-is-this-frustrating-your-workflow/81627

---

### Windsurf

**Transport**: stdio, SSE, Streamable HTTP
**Config format**: JSON
```json
// ~/.codeium/windsurf/mcp_config.json
```

**BEHAVIOR — 100 TOOL LIMIT**:
- Hard limit of 100 tools across all servers
- Per-tool toggling IS available (Settings > Cascade)

**fanout implications**:
- Detect from `clientInfo.name`
- Filter to 100 tools maximum
- Less critical than Cursor (100 is usually enough) but still enforce

---

### VS Code Copilot

**Transport**: stdio, SSE, Streamable HTTP
**Config format**: JSON
```json
// .vscode/mcp.json or VS Code settings
{
  "mcp": {
    "servers": {
      "fanout": {
        "command": "fanout",
        "args": ["connect"]
      }
    }
  }
}
```

**BEHAVIOR — 128 TOOL LIMIT**:
- Hard limit of 128 tools
- OAuth 2.1 supported for remote servers
- GA since July 2025
- Auto-discovers tools from other installed extensions
- `Configure Tools` UI allows budget management

**fanout implications**:
- Detect and filter to 128
- Support OAuth 2.1 passthrough for remote connections

---

### Gemini CLI

**Transport**: stdio, SSE, AND Streamable HTTP (all three supported)
**Config format**: JSON
**`clientInfo.name`**: `gemini-cli-mcp-client`
```json
// ~/.gemini/settings.json
{
  "mcpServers": {
    "plug": {
      "command": "plug",
      "args": ["connect"]
    }
  }
}
```

**UPDATED (2026-03-03)**: Gemini CLI supports ALL transports — stdio, SSE, and Streamable HTTP — not just "Direct HTTP" as previously documented. Can be configured as either stdio or HTTP.

**CRITICAL BEHAVIORS**:
1. **Calls `list_prompts` FIRST** — before `tools/list`. If `prompts/list` hangs or times out, tools are NEVER discovered. Sequential discovery, not parallel.
2. **Hardcoded 60s timeout** — Gemini CLI has a hardcoded 60-second timeout for MCP discovery that ignores any configured timeout. If all list operations don't complete within 60s, the server is considered unavailable.
3. **Random OAuth callback ports** — if OAuth is used, callback ports are random and may not stay alive, causing `ERR_CONNECTION_REFUSED`

**fanout implications**:
- `prompts/list` MUST respond instantly (< 100ms). Return cached or empty.
- `tools/list` MUST also respond quickly. Pre-cache tool lists at startup.
- Gemini can connect via stdio OR HTTP — both paths must work
- Test with Gemini CLI as part of compatibility validation

**Bug reference**: https://github.com/google-gemini/gemini-cli/issues/7324

---

### Codex CLI

**Transport**: stdio (primary), Streamable HTTP
**Config format**: TOML
```toml
# ~/.codex/config.toml
[mcp_servers.plug]
type = "stdio"
command = "plug"
args = ["connect"]
# bearer_token_env_var = "FANOUT_TOKEN"  # for HTTP mode
```

**CRITICAL BEHAVIOR**:
- Calls `resources/list` FIRST — before tools/list
- If `resources/list` returns an ERROR (not empty list), Codex marks the entire server as unavailable
- Must return `{"resources": []}` — NOT an error

**Other behaviors**:
- Configurable `startup_timeout_sec` and `tool_timeout_sec`
- Supports elicitation

**fanout implications**:
- ALWAYS return `{"resources": []}` for `resources/list` if no upstream servers provide resources
- Never return an error for `resources/list`
- Test with Codex as part of compatibility validation
- Default lazy mode: `native`

---

### OpenCode

**Transport**: SSE and Streamable HTTP (auto-negotiation)
**`clientInfo.name`**: `opencode`

**UPDATED (2026-03-03)**: OpenCode now supports Streamable HTTP with auto-negotiation. Legacy SSE support remains for backwards compatibility but is no longer the only option.

**fanout implications**:
- Default lazy mode: `bridge`
- OpenCode can connect via Streamable HTTP (preferred) or legacy SSE
- Legacy SSE endpoint (`/sse`) still useful for older OpenCode versions
- Lower priority for legacy SSE implementation since OpenCode now auto-negotiates
- Bridge mode keeps the initial `tools/list` small and lets the agent search/load real tools into its session working set.
- Once loaded, tools appear under the same routed names they use in standard mode, so downstream permission/approval behavior remains as specific as the client allows.

---

### Zed

**Transport**: stdio only
**Config format**: JSON in Zed settings

**BEHAVIOR**:
- No HTTP/SSE support at all
- Cannot connect to remote servers
- Protocol version may lag behind latest spec

**fanout implications**:
- Connect via `fanout connect` (stdio)
- Standard operation, no special handling

---

### Factory/Droid

**Transport**: stdio (local), HTTP (remote)
**Auth**: OAuth for HTTP, API key for stdio

**BEHAVIOR**:
- Tokens stored in system keyring (global, not per-project)
- Short default timeouts
- Interactive `/mcp` UI for configuration

**fanout implications**:
- Standard stdio connection
- Be aware of short timeouts — respond quickly

---

## MCP Feature Support Matrix

This shows which features each client actually supports (not just what the spec defines):

| Feature | Claude Code | Claude Desktop | Cursor | Windsurf | VS Code | Gemini | Codex | OpenCode | Zed |
|---------|------------|---------------|--------|----------|---------|--------|-------|----------|-----|
| Tools | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Resources | Yes | Yes | Yes | -- | -- | -- | -- | -- | -- |
| Prompts | Yes | Yes | Yes | -- | -- | Yes | -- | -- | Yes |
| Sampling | -- | -- | Yes | -- | Yes | -- | -- | -- | -- |
| Tasks | -- | -- | Yes | -- | Yes | -- | -- | -- | -- |
| Roots | Yes | Yes | Yes | -- | Yes | -- | -- | -- | -- |
| Elicitation | -- | -- | Yes | -- | Yes | -- | Yes | -- | -- |
| Discovery | -- | -- | Yes | Yes | Yes | -- | -- | -- | -- |
| Tool Search | Yes | -- | -- | -- | -- | -- | Native | Bridge | -- |

**fanout implications**:
- Tools must work for ALL clients (universal support)
- Resources and Prompts must pass through but gracefully degrade (return empty lists for clients that don't request them)
- Advanced features (sampling, tasks, elicitation) are pass-through only — forward to the client that initiated the request

---

## Client Detection Strategy

Detect client type from `clientInfo.name` in `InitializeRequest` using exact match as primary, fuzzy fallback as secondary (ADR-007):

**Confirmed `clientInfo.name` values** (verified 2026-03-03):

| Client | `clientInfo.name` |
|--------|-------------------|
| Claude Code | `claude-code` |
| Claude Desktop | `claude-ai` |
| Cursor | `cursor-vscode` |
| Windsurf | `windsurf-client` |
| VS Code Copilot | `Visual-Studio-Code` |
| Gemini CLI | `gemini-cli-mcp-client` |
| OpenCode | `opencode` |
| Zed | `Zed` |

```rust
fn detect_client(client_info: &ClientInfo) -> ClientType {
    // Tier 1: Exact match (preferred — verified values)
    match client_info.name.as_str() {
        "claude-code" => return ClientType::ClaudeCode,
        "claude-ai" => return ClientType::ClaudeDesktop,
        "cursor-vscode" => return ClientType::Cursor,
        "windsurf-client" => return ClientType::Windsurf,
        "Visual-Studio-Code" => return ClientType::VSCodeCopilot,
        "gemini-cli-mcp-client" => return ClientType::GeminiCli,
        "opencode" => return ClientType::OpenCode,
        "Zed" => return ClientType::Zed,
        _ => {}
    }

    // Tier 2: Fuzzy fallback (for unknown client versions)
    let name = client_info.name.to_lowercase();
    match () {
        _ if name.contains("claude-code") || name.contains("claude code") => ClientType::ClaudeCode,
        _ if name.contains("claude") => ClientType::ClaudeDesktop,
        _ if name.contains("cursor") => ClientType::Cursor,
        _ if name.contains("windsurf") || name.contains("codeium") => ClientType::Windsurf,
        _ if name.contains("copilot") || name.contains("vscode") => ClientType::VSCodeCopilot,
        _ if name.contains("gemini") => ClientType::GeminiCli,
        _ if name.contains("codex") => ClientType::CodexCli,
        _ if name.contains("opencode") => ClientType::OpenCode,
        _ if name.contains("zed") => ClientType::Zed,
        _ => ClientType::Unknown,
    }
}
```

**Unknown clients get conservative defaults**: no tool limit, full tool list, standard timeouts.

**Source**: Apify MCP Client Capabilities Index, `docs/research/client-validation.md`

---

## Config Auto-Import Locations

fanout should scan these on first run:

| Client | Path | Format |
|--------|------|--------|
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) | JSON |
| Claude Code | `~/.claude.json` or `.mcp.json` in cwd | JSON |
| Cursor | `~/.cursor/mcp.json` | JSON |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | JSON |
| VS Code | `~/.vscode/mcp.json` or settings.json | JSON |
| Codex | `~/.codex/config.toml` | TOML |
| Gemini | `~/.gemini/settings.json` | JSON |

All of these store MCP server configs in slightly different JSON/TOML schemas. fanout needs a parser for each to extract server definitions and normalize into its own TOML format.
