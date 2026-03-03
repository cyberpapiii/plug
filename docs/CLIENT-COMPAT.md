# Client Compatibility Reference

This is the definitive reference for every AI client's MCP behavior, quirks, and requirements. fanout must handle ALL of these correctly.

---

## Client Matrix

| Client | Transport | Tool Limit | Config Format | Config Location |
|--------|-----------|-----------|---------------|-----------------|
| Claude Code | stdio | None (tool search at >10% ctx) | JSON | `.mcp.json` or `~/.claude.json` |
| Claude Desktop | stdio, SSE, Streamable HTTP | None | JSON | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Cursor | stdio, SSE, Streamable HTTP | **40** | JSON | `.cursor/mcp.json` or `~/.cursor/mcp.json` |
| Windsurf | stdio, SSE, Streamable HTTP | **100** | JSON | `~/.codeium/windsurf/mcp_config.json` |
| VS Code Copilot | stdio, SSE, Streamable HTTP | **128** | JSON | `.vscode/mcp.json` or settings |
| Gemini CLI | Direct HTTP | None documented | JSON | `~/.gemini/settings.json` |
| Codex CLI | stdio, Streamable HTTP | None documented | TOML | `~/.codex/config.toml` |
| OpenCode | SSE only | None documented | Config file | Varies |
| Zed | stdio only | None documented | JSON | `settings.json` |
| Factory/Droid | stdio, HTTP | None documented | CLI config | Varies |

---

## Detailed Client Profiles

### Claude Code

**Transport**: stdio via `fanout connect`
**Config format**: JSON
```json
// .mcp.json (project-level) or ~/.claude.json (global)
{
  "mcpServers": {
    "fanout": {
      "command": "fanout",
      "args": ["connect"]
    }
  }
}
```

**Behavior**:
- Supports roots, sampling
- Tool Search: when tool definitions exceed ~10% of context window, Claude Code switches to lazy-loading — it sends tool descriptions only (no schemas) and fetches full schemas on demand
- This is the IDEAL client for fanout — it already handles large tool sets gracefully

**fanout implications**:
- Return full tool list; Claude Code will manage token efficiency itself
- Support tool search protocol if Claude Code sends it

---

### Claude Desktop

**Transport**: stdio (local), SSE or Streamable HTTP (remote)
**Config format**: JSON
```json
// ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "fanout": {
      "command": "fanout",
      "args": ["connect"]
    }
  }
}
```

**Behavior**:
- Supports resources, prompts, tools, roots
- Can connect to remote MCP servers via HTTP
- SSE may be deprecated in future Claude Desktop versions

**fanout implications**:
- Standard stdio connection via `fanout connect`
- No special handling needed

---

### Cursor

**Transport**: stdio, SSE, Streamable HTTP
**Config format**: JSON
```json
// .cursor/mcp.json (project) or ~/.cursor/mcp.json (global)
{
  "mcpServers": {
    "fanout": {
      "command": "fanout",
      "args": ["connect"]
    }
  }
}
```

**CRITICAL BEHAVIOR — 40 TOOL HARD LIMIT**:
- Cursor has a hard limit of 40 tools across ALL connected MCP servers
- Tools beyond 40 are **silently dropped** — no error, no warning, no notification
- The user has no way to know which tools were dropped
- Per-tool toggling is NOT available in Cursor

**fanout implications**:
- MUST detect Cursor from `clientInfo.name` during initialization
- MUST filter tool list to 40 tools maximum
- SHOULD sort tools by priority (usage frequency, user-configured priority_tools)
- SHOULD emit a warning event: "Cursor: serving 40/65 tools (25 filtered)"
- SHOULD log which tools were filtered so the user can adjust priorities

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

**Transport**: Direct HTTP (NOT stdio)
**Config format**: JSON
```json
// ~/.gemini/settings.json
{
  "mcpServers": {
    "fanout": {
      "httpUrl": "http://localhost:3282/mcp",
      "authHeaders": {
        "Authorization": "Bearer <token>"
      }
    }
  }
}
```

**CRITICAL BEHAVIORS**:
1. **Calls `list_prompts` FIRST** — before `tools/list`. If `prompts/list` hangs or times out, tools are NEVER discovered. Sequential discovery, not parallel.
2. **Hardcoded 60s timeout** — Gemini CLI has a hardcoded 60-second timeout for MCP discovery that ignores any configured timeout. If all list operations don't complete within 60s, the server is considered unavailable.
3. **Random OAuth callback ports** — if OAuth is used, callback ports are random and may not stay alive, causing `ERR_CONNECTION_REFUSED`

**fanout implications**:
- `prompts/list` MUST respond instantly (< 100ms). Return cached or empty.
- `tools/list` MUST also respond quickly. Pre-cache tool lists at startup.
- Gemini connects via HTTP, not stdio — must have HTTP server running
- Test with Gemini CLI as part of compatibility validation

**Bug reference**: https://github.com/google-gemini/gemini-cli/issues/7324

---

### Codex CLI

**Transport**: stdio (primary), Streamable HTTP
**Config format**: TOML
```toml
# ~/.codex/config.toml
[mcp_servers.fanout]
type = "stdio"
command = "fanout"
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

---

### OpenCode

**Transport**: SSE only (no Streamable HTTP support)
**Issue**: https://github.com/anomalyco/opencode/issues/8058

**CRITICAL BEHAVIOR**:
- Sends GET expecting SSE protocol (old-style: `endpoint` event followed by POST to that endpoint)
- Returns `405` or `ERR_CONNECTION_REFUSED` for HTTP-only servers
- No Streamable HTTP support (as of March 2026)

**fanout implications**:
- MUST serve legacy SSE endpoint for OpenCode compatibility
- Use the backwards-compatibility procedure from the MCP spec:
  - Serve old SSE endpoint at `/sse` (GET returns `endpoint` event)
  - Serve Streamable HTTP at `/mcp` (POST endpoint)
  - Clients auto-negotiate based on what works

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
| Tool Search | Yes | -- | -- | -- | -- | -- | -- | -- | -- |

**fanout implications**:
- Tools must work for ALL clients (universal support)
- Resources and Prompts must pass through but gracefully degrade (return empty lists for clients that don't request them)
- Advanced features (sampling, tasks, elicitation) are pass-through only — forward to the client that initiated the request

---

## Client Detection Strategy

Detect client type from `clientInfo.name` in `InitializeRequest`:

```rust
fn detect_client(client_info: &ClientInfo) -> ClientType {
    let name = client_info.name.to_lowercase();
    match () {
        _ if name.contains("claude code") || name.contains("claude-code") => ClientType::ClaudeCode,
        _ if name.contains("claude") && name.contains("desktop") => ClientType::ClaudeDesktop,
        _ if name.contains("cursor") => ClientType::Cursor,
        _ if name.contains("windsurf") || name.contains("codeium") => ClientType::Windsurf,
        _ if name.contains("copilot") || name.contains("vscode") => ClientType::VSCodeCopilot,
        _ if name.contains("gemini") => ClientType::GeminiCli,
        _ if name.contains("codex") => ClientType::CodexCli,
        _ if name.contains("opencode") => ClientType::OpenCode,
        _ if name.contains("zed") => ClientType::Zed,
        _ if name.contains("factory") || name.contains("droid") => ClientType::FactoryDroid,
        _ => ClientType::Unknown,
    }
}
```

**Unknown clients get conservative defaults**: no tool limit, full tool list, standard timeouts.

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
