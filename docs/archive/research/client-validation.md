# MCP Client Compatibility Validation Report

**Date**: March 3, 2026
**Purpose**: Validate documented client behaviors in `CLIENT-COMPAT.md` against current (March 2026) state.
**Methodology**: Web searches, official documentation, GitHub issues, forum posts, and the [Apify MCP Client Capabilities Index](https://github.com/apify/mcp-client-capabilities).

---

## Summary of Changes Found

| Client | Major Changes Since Doc Written |
|--------|-------------------------------|
| Cursor | Tool limit effectively removed (Dynamic Context Discovery). `clientInfo.name` is `cursor-vscode`. |
| Windsurf | No major changes. 100-tool limit confirmed. `clientInfo.name` is `windsurf-client`. |
| VS Code Copilot | Virtual Tools feature partially addresses 128 limit. `clientInfo.name` is `Visual-Studio-Code`. |
| Gemini CLI | Now supports stdio, SSE, AND Streamable HTTP (doc said "Direct HTTP only"). 60s timeout bug confirmed open. `clientInfo.name` is `gemini-cli-mcp-client`. |
| Codex CLI | resources/list issue still open. Supports stdio + Streamable HTTP (no SSE). `clientInfo.name` UNCONFIRMED (likely `codex-cli`). |
| OpenCode | NOW supports both SSE and Streamable HTTP (auto-negotiation). Doc is WRONG saying SSE-only. `clientInfo.name` is `opencode`. |
| Claude Code | No major changes. `clientInfo.name` is `claude-code`. |
| Claude Desktop | Remote connectors support SSE + Streamable HTTP (beta). Local is still stdio-only. `clientInfo.name` is `claude-ai`. |
| Zed | Still stdio only. Community requesting HTTP support. `clientInfo.name` is `Zed` (capital Z). |
| Factory/Droid | Supports stdio + HTTP. `clientInfo.name` UNCONFIRMED (not in Apify index). |

---

## Detailed Validation by Client

### 1. Cursor

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `cursor-vscode` | NEW INFO |
| Tool Limit | 40 (hard limit) | **Effectively removed** via Dynamic Context Discovery | CHANGED |
| Transport | stdio, SSE, Streamable HTTP | stdio, SSE, Streamable HTTP (Cursor 1.0+ native) | CONFIRMED |
| Config (macOS) | `.cursor/mcp.json` or `~/.cursor/mcp.json` | `~/.cursor/mcp.json` (global) or `.cursor/mcp.json` (project) | CONFIRMED |
| Config (Linux) | Same | `~/.cursor/mcp.json` (global) or `.cursor/mcp.json` (project) | CONFIRMED |
| Config (Windows) | Same | `%USERPROFILE%\.cursor\mcp.json` (global) or `.cursor\mcp.json` (project) | CONFIRMED |
| Protocol Version | Not documented | `2025-06-18` | NEW INFO |

**Key Finding**: Cursor released "Dynamic Context Discovery" in January 2026 ([blog post](https://cursor.com/blog/dynamic-context-discovery)). This fundamentally changes how Cursor handles MCP tools. Instead of loading all tool definitions into context upfront (which imposed the 40-tool practical limit), Cursor now syncs tool descriptions to a folder and loads them on-demand. Users report running 80+ tools without warnings. The A/B test showed a 46.9% reduction in total agent token usage.

**Impact on fanout**: The hard 40-tool filter documented in `CLIENT-COMPAT.md` may no longer be necessary for recent Cursor versions. However, older Cursor versions (pre-2.3) still enforce the 40-tool limit. fanout should detect the Cursor version and apply the limit only for older versions, or use a conservative approach.

**MCP Capabilities** (from Apify index): resources, roots, elicitation, prompts, tools (listChanged)

**Sources**:
- [Cursor Dynamic Context Discovery Blog](https://cursor.com/blog/dynamic-context-discovery)
- [InfoQ Coverage](https://www.infoq.com/news/2026/01/cursor-dynamic-context-discovery/)
- [Forum Discussion on Limit](https://forum.cursor.com/t/regarding-the-quantity-limit-of-mcp-tools/153432)
- [Original 40-Tool Limit Issue](https://github.com/cursor/cursor/issues/3369)
- [Forum: MCP Streamable HTTP Support](https://forum.cursor.com/t/mcp-streamable-http-support/96770)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 2. Windsurf

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `windsurf-client` | NEW INFO |
| Tool Limit | 100 | 100 | CONFIRMED |
| Transport | stdio, SSE, Streamable HTTP | stdio, SSE, Streamable HTTP (+ OAuth per transport) | CONFIRMED |
| Config (macOS) | `~/.codeium/windsurf/mcp_config.json` | `~/.codeium/windsurf/mcp_config.json` | CONFIRMED |
| Config (Linux) | Not documented | `~/.codeium/windsurf/mcp_config.json` | CONFIRMED |
| Config (Windows) | Not documented | `%USERPROFILE%\.codeium\windsurf\mcp_config.json` | CONFIRMED |
| Protocol Version | Not documented | `2025-03-26` | NEW INFO |

**Key Finding**: The 100-tool limit is confirmed per [official Windsurf docs](https://docs.windsurf.com/windsurf/cascade/mcp): "Cascade has a limit of 100 total tools that it has access to at any given time." Per-tool toggling IS available via the MCP settings page. OAuth support is now available for all transport types.

**MCP Capabilities** (from Apify index): prompts, resources, tools (listChanged)

**Quirk**: Windsurf supports environment variable interpolation in mcp_config.json for fields: `command`, `args`, `env`, `serverUrl`, `url`, and `headers`.

**Sources**:
- [Windsurf MCP Docs](https://docs.windsurf.com/windsurf/cascade/mcp)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 3. VS Code Copilot

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `Visual-Studio-Code` | NEW INFO |
| Tool Limit | 128 (hard) | 128 (hard, but Virtual Tools feature mitigates) | PARTIALLY CHANGED |
| Transport | stdio, SSE, Streamable HTTP | stdio, SSE, Streamable HTTP | CONFIRMED |
| Config (macOS) | `.vscode/mcp.json` or settings | `.vscode/mcp.json` (workspace) or `$HOME/Library/Application Support/Code/User/mcp.json` (user) | CONFIRMED |
| Config (Linux) | Not documented | `.vscode/mcp.json` (workspace) or `$HOME/.config/Code/User/mcp.json` (user) | CONFIRMED |
| Config (Windows) | Not documented | `.vscode\mcp.json` (workspace) or `%APPDATA%\Code\User\mcp.json` (user) | CONFIRMED |
| Protocol Version | Not documented | `2025-06-18` | NEW INFO |

**Key Finding**: The 128-tool hard limit at the API request level remains ([Issue #290356](https://github.com/microsoft/vscode/issues/290356)). However, VS Code has introduced "Virtual Tools" as a mitigation: when the `github.copilot.chat.virtualTools.threshold` setting (default: 128) is exceeded, VS Code groups tools into "virtual tools" that the model can activate on demand. This allows configurations with more than 128 tools to function, though the underlying API limit is still 128 per request.

**MCP Capabilities** (from Apify index): resources, sampling, elicitation (form, url), roots (listChanged), tools (listChanged), tasks

**Config Note**: VS Code uses a different JSON schema than other clients. The top-level key is `servers` (not `mcpServers`):
```json
{
  "servers": {
    "fanout": {
      "command": "fanout",
      "args": ["connect"]
    }
  }
}
```

**Sources**:
- [VS Code MCP Docs](https://code.visualstudio.com/docs/copilot/customization/mcp-servers)
- [Issue #290356: Hard Tool Limit](https://github.com/microsoft/vscode/issues/290356)
- [Issue #13065: 128 Tool Limit](https://github.com/microsoft/vscode-copilot-release/issues/13065)
- [MCP Configuration Reference](https://code.visualstudio.com/docs/copilot/reference/mcp-configuration)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 4. Gemini CLI

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `gemini-cli-mcp-client` | NEW INFO |
| Tool Limit | None documented | None documented | CONFIRMED |
| Transport | "Direct HTTP (NOT stdio)" | stdio, SSE, AND Streamable HTTP | CHANGED (doc was wrong) |
| Config (macOS) | `~/.gemini/settings.json` | `~/.gemini/settings.json` (global) or `.gemini/settings.json` (project) | CONFIRMED |
| Config (Linux) | Not documented | `~/.gemini/settings.json` (global) or `.gemini/settings.json` (project) | CONFIRMED |
| Config (Windows) | Not documented | `~/.gemini/settings.json` (global) or `.gemini/settings.json` (project) | CONFIRMED |
| Protocol Version | Not documented | `2025-06-18` | NEW INFO |
| 60s Hardcoded Timeout | Yes | Yes (still open bug) | CONFIRMED |
| Sequential Discovery | Yes (prompts first) | Yes | CONFIRMED |

**Key Finding**: The document's claim that Gemini CLI only supports "Direct HTTP (NOT stdio)" is INCORRECT. Gemini CLI actually supports ALL three transports:
- **stdio**: `gemini mcp add <name> <command> [args...]` (default)
- **SSE**: `gemini mcp add --transport sse <name> <url>`
- **Streamable HTTP**: `gemini mcp add --transport http <name> <url>`

Transport selection is based on config properties: `httpUrl` -> Streamable HTTP, `url` -> SSE, `command` -> stdio.

**Timeout Bug**: The 60-second hardcoded timeout for MCP discovery remains an open bug as of January 29, 2026 ([Issue #17787](https://github.com/google-gemini/gemini-cli/issues/17787)). The CLI ignores configured timeout values during the initial discovery call. A related issue ([#6763](https://github.com/google-gemini/gemini-cli/issues/6763)) also reports this behavior.

**Sequential Discovery**: Confirmed. The discoverMcpTools() function iterates through servers sequentially. Prompts are discovered first (available as slash commands), then tools. A February 2026 fix ([Issue #18585](https://github.com/google-gemini/gemini-cli/issues/18585)) addressed an issue where prompts would queue indefinitely when all configured MCP servers are skipped.

**MCP Capabilities** (from Apify index): tools, prompts (notably NO resources or roots)

**Sources**:
- [Gemini CLI MCP Docs](https://google-gemini.github.io/gemini-cli/docs/tools/mcp-server.html)
- [Issue #17787: Timeout Bug](https://github.com/google-gemini/gemini-cli/issues/17787)
- [Issue #18585: Prompts Queue Indefinitely](https://github.com/google-gemini/gemini-cli/issues/18585)
- [Issue #6763: Timeout Not Respected](https://github.com/google-gemini/gemini-cli/issues/6763)
- [Gemini CLI Configuration Docs](https://geminicli.com/docs/reference/configuration/)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 5. Codex CLI

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | UNCONFIRMED (TUI reports `codex-tui`, not in Apify index) | UNCONFIRMED |
| Tool Limit | None documented | None documented | CONFIRMED |
| Transport | stdio, Streamable HTTP | stdio, Streamable HTTP (NO native SSE) | CONFIRMED |
| Config (macOS) | `~/.codex/config.toml` | `~/.codex/config.toml` (global) or `.codex/config.toml` (project) | CONFIRMED |
| Config (Linux) | Not documented | `~/.codex/config.toml` (global) or `.codex/config.toml` (project) | CONFIRMED |
| Config (Windows) | Not documented | `%USERPROFILE%\.codex\config.toml` (global) or `.codex\config.toml` (project) | CONFIRMED |
| resources/list behavior | Calls first, errors break server | Yes, still open issue | CONFIRMED |

**Key Finding**: The resources/list issue is still open as of February 2026 ([Issue #8565](https://github.com/openai/codex/issues/8565)). Codex-CLI heavily depends on the `resources/list` response for checking MCP server availability, even though MCP servers might yield an empty array while other features (tools, prompts) are functional. Workaround: always return `{"resources": []}` and never an error.

**SSE Support**: Codex does NOT natively support SSE transport. Users needing SSE must use an adapter like `mcp-proxy` ([Issue #2129](https://github.com/openai/codex/issues/2129)). Streamable HTTP support exists but has had reported issues ([Issue #4707](https://github.com/openai/codex/issues/4707)).

**clientInfo.name Note**: OpenAI docs state "The TUI reports codex-tui, and the app server reports the clientInfo.name value from initialize." The exact clientInfo.name for the CLI is not definitively confirmed in public sources. The match pattern `name.contains("codex")` in the detection code should still work.

**Sources**:
- [Codex MCP Docs](https://developers.openai.com/codex/mcp/)
- [Issue #8565: resources/list Bug](https://github.com/openai/codex/issues/8565)
- [Issue #2129: SSE Support Request](https://github.com/openai/codex/issues/2129)
- [Issue #4707: Streamable HTTP Issues](https://github.com/openai/codex/issues/4707)
- [Codex Config Docs](https://github.com/openai/codex/blob/main/docs/config.md)

---

### 6. OpenCode

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `opencode` | NEW INFO |
| Tool Limit | None documented | None documented | CONFIRMED |
| Transport | "SSE only (no Streamable HTTP)" | SSE AND Streamable HTTP (auto-negotiation) | CHANGED |
| Config (macOS) | "Varies" | `~/.config/opencode/opencode.json` (global) or `opencode.json` (project root) | CONFIRMED |
| Config (Linux) | Not documented | `~/.config/opencode/opencode.json` (global) or `opencode.json` (project root) | CONFIRMED |
| Config (Windows) | Not documented | `%USERPROFILE%\.config\opencode\opencode.json` (global) or `opencode.json` (project root) | UNCONFIRMED |
| Protocol Version | Not documented | `2025-06-18` | NEW INFO |

**CRITICAL CHANGE**: The doc states OpenCode is "SSE only (no Streamable HTTP support)" and references Issue #8058. However, **Issue #8058 was CLOSED AS COMPLETED on January 14, 2026** ([source](https://github.com/anomalyco/opencode/issues/8058)). A collaborator confirmed: "we already support both actually rn it just infers the correct one by attempting to connect to sse and http streamable." Additionally, Issue #6242 (requesting HTTP/Streamable negotiation before deprecated SSE) was also **closed as completed** on December 27, 2025 ([source](https://github.com/anomalyco/opencode/issues/6242)).

OpenCode now auto-negotiates between SSE and Streamable HTTP transports. However, error messaging is still poor -- if both fail, users see an SSE error rather than a clear message about transport negotiation failure.

**Impact on fanout**: The fallback SSE endpoint documented in `CLIENT-COMPAT.md` may still be needed for error handling, but OpenCode can now connect to Streamable HTTP servers directly.

**MCP Capabilities** (from Apify index): tools (listChanged) only -- notably no resources, prompts, or roots

**Sources**:
- [Issue #8058: Streamable HTTP Support (CLOSED)](https://github.com/anomalyco/opencode/issues/8058)
- [Issue #6242: HTTP/Streamable Negotiation (CLOSED)](https://github.com/anomalyco/opencode/issues/6242)
- [OpenCode MCP Docs](https://opencode.ai/docs/mcp-servers/)
- [OpenCode Config Docs](https://opencode.ai/docs/config/)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 7. Claude Code

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `claude-code` | CONFIRMED |
| Tool Limit | None (tool search at >10% ctx) | None (tool search auto-enabled at >10% context) | CONFIRMED |
| Transport | stdio | stdio, SSE, Streamable HTTP | EXPANDED |
| Config (macOS) | `.mcp.json` or `~/.claude.json` | `.mcp.json` (project) or `~/.claude.json` (global) | CONFIRMED |
| Config (Linux) | Not documented | `.mcp.json` (project) or `~/.claude.json` (global) | CONFIRMED |
| Config (Windows) | Not documented | `.mcp.json` (project) or `~/.claude.json` (global) | CONFIRMED |
| Protocol Version | Not documented | `2025-06-18` | NEW INFO |

**Tool Search Behavior**: MCP Tool Search was announced January 14, 2026 and is now enabled by default for all users. When MCP tool descriptions would use more than 10% of the context window, Claude Code switches to lazy-loading: it shows tool names/descriptions only and fetches full schemas on demand. This reduced context consumption from ~77K tokens to ~8.7K tokens with 50+ MCP tools (an 85% reduction).

**Transport**: Claude Code supports all three transports (stdio, SSE, Streamable HTTP) via the `--transport` flag: `claude mcp add --transport sse <name> <url>`.

**MCP Capabilities** (from Apify index): prompts, roots, resources, tools

**Config Note**: MCP servers are ONLY recognized in `.mcp.json` (project) or `~/.claude.json` (global). The `~/.claude/settings.json` file is NOT used for MCP server configuration.

**Sources**:
- [Claude Code MCP Docs](https://code.claude.com/docs/en/mcp)
- [MCP Tool Search Announcement](https://medium.com/@joe.njenga/claude-code-just-cut-mcp-context-bloat-by-46-9-51k-tokens-down-to-8-5k-with-new-tool-search-ddf9e905f734)
- [Tool Search Feature Explainer](https://www.atcyrus.com/stories/mcp-tool-search-claude-code-context-pollution-guide)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)

---

### 8. Claude Desktop

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `claude-ai` | CONFIRMED |
| Tool Limit | None | None documented | CONFIRMED |
| Transport | stdio (local), SSE or Streamable HTTP (remote) | stdio (local config), SSE + Streamable HTTP (remote connectors, beta) | CONFIRMED |
| Config (macOS) | `~/Library/Application Support/Claude/claude_desktop_config.json` | `~/Library/Application Support/Claude/claude_desktop_config.json` | CONFIRMED |
| Config (Linux) | Not documented | `~/.config/Claude/claude_desktop_config.json` | CONFIRMED |
| Config (Windows) | Not documented | `%APPDATA%\Claude\claude_desktop_config.json` | CONFIRMED |

**Key Finding**: Claude Desktop's transport support has a nuance:
- **Local servers** (configured in `claude_desktop_config.json`): stdio ONLY. The client launches MCP servers as child processes.
- **Remote servers** (configured via Settings > Connectors): SSE and Streamable HTTP supported. Available on Pro, Max, Team, and Enterprise plans. This is still in beta.
- SSE support may be deprecated in the coming months.
- Remote servers configured directly in `claude_desktop_config.json` will NOT connect -- you must use Settings > Connectors.

**clientInfo.name**: Confirmed as `claude-ai` (NOT `claude-desktop`). This was verified from MCP server logs showing the initialize request: `"clientInfo":{"name":"claude-ai","version":"0.1.0"}`.

**Important**: The `clientInfo.name` is `claude-ai` for BOTH Claude Desktop and Claude.ai web. The detection code in `CLIENT-COMPAT.md` uses `name.contains("claude") && name.contains("desktop")` which will NOT match `claude-ai`. This needs to be fixed.

**Sources**:
- [Claude Desktop MCP Docs](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)
- [Remote Connectors Docs](https://support.claude.com/en/articles/11503834-building-custom-connectors-via-remote-mcp-servers)
- [Netdata Claude Desktop Guide](https://learn.netdata.cloud/docs/netdata-ai/mcp/mcp-clients/claude-desktop)
- [MCP Discussion #16: HTTP Transport](https://github.com/orgs/modelcontextprotocol/discussions/16)

---

### 9. Zed

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | `Zed` (capital Z) | NEW INFO |
| Tool Limit | None documented | None documented | CONFIRMED |
| Transport | stdio only | stdio only (HTTP/Streamable requested, not yet implemented) | CONFIRMED |
| Config (macOS) | `settings.json` | `~/.config/zed/settings.json` (key: `context_servers`) | CONFIRMED |
| Config (Linux) | Not documented | `~/.config/zed/settings.json` (key: `context_servers`) | CONFIRMED |
| Config (Windows) | Not documented | UNCONFIRMED (likely `%APPDATA%\Zed\settings.json` based on [Issue #42692](https://github.com/zed-industries/zed/issues/42692)) | UNCONFIRMED |
| Protocol Version | Not documented | `2025-03-26` | NEW INFO |

**Key Finding**: Zed still only supports stdio transport. Active community discussions requesting HTTP/Streamable HTTP support:
- [Discussion #29370](https://github.com/zed-industries/zed/discussions/29370): Support 2025-03-26 MCP spec and MCPs from URLs
- [Discussion #26115](https://github.com/zed-industries/zed/discussions/26115): MCP with SSE
- [Discussion #34719](https://github.com/zed-industries/zed/discussions/34719): Support for HTTP MCP servers

Implementation is reportedly blocked by the need to implement OAuth, which adds significant effort.

**Config Note**: Zed uses `context_servers` (not `mcpServers`) as the configuration key.

**MCP Capabilities** (from Apify index): tools, prompts only

**Quirk**: Zed handles `notifications/tools/list_changed` and auto-reloads the tool list without requiring a server restart.

**Sources**:
- [Zed MCP Docs](https://zed.dev/docs/ai/mcp)
- [Apify MCP Client Capabilities](https://github.com/apify/mcp-client-capabilities)
- [Discussion #29370](https://github.com/zed-industries/zed/discussions/29370)
- [Discussion #34719](https://github.com/zed-industries/zed/discussions/34719)

---

### 10. Factory/Droid

| Field | Documented Value | Validated Value (March 2026) | Status |
|-------|-----------------|------------------------------|--------|
| `clientInfo.name` | Not documented | UNCONFIRMED (not in Apify index, closed-source) | UNCONFIRMED |
| Tool Limit | None documented | None documented | CONFIRMED |
| Transport | stdio, HTTP | stdio, HTTP (Streamable HTTP since v1.3.220) | CONFIRMED |
| Config (macOS) | "CLI config" / "Varies" | `~/.factory/mcp.json` (global) or `.factory/mcp.json` (project) | CONFIRMED |
| Config (Linux) | Not documented | `~/.factory/mcp.json` (global) or `.factory/mcp.json` (project) | CONFIRMED |
| Config (Windows) | Not documented | `%USERPROFILE%\.factory\mcp.json` (global) or `.factory\mcp.json` (project) | UNCONFIRMED |

**Key Finding**: Factory/Droid is a closed-source product and the `clientInfo.name` is not publicly documented or indexed. The detection pattern `name.contains("factory") || name.contains("droid")` is a reasonable guess but UNCONFIRMED. Factory released Streamable HTTP-based MCP server support in addition to stdio-based servers, enabling connection to MCP servers running as web services.

**Management**: Servers can be added via `droid mcp add --type stdio <name> "<command>"` or via the interactive `/mcp` UI. Configuration auto-reloads when the config file changes.

**Sources**:
- [Factory MCP Docs](https://docs.factory.ai/cli/configuration/mcp)
- [Factory v1.3.220 Release](https://github.com/Factory-AI/factory/discussions/108)
- [PulseMCP Factory Entry](https://www.pulsemcp.com/clients/factory)

---

## Corrected Client Matrix

| Client | `clientInfo.name` | Transport | Tool Limit | Config Location (macOS) | Config Location (Linux) | Config Location (Windows) | Protocol Version |
|--------|------------------|-----------|-----------|------------------------|------------------------|--------------------------|-----------------|
| Claude Code | `claude-code` | stdio, SSE, Streamable HTTP | None (tool search) | `~/.claude.json` or `.mcp.json` | `~/.claude.json` or `.mcp.json` | `~/.claude.json` or `.mcp.json` | 2025-06-18 |
| Claude Desktop | `claude-ai` | stdio (local), SSE + Streamable HTTP (remote beta) | None | `~/Library/Application Support/Claude/claude_desktop_config.json` | `~/.config/Claude/claude_desktop_config.json` | `%APPDATA%\Claude\claude_desktop_config.json` | UNCONFIRMED |
| Cursor | `cursor-vscode` | stdio, SSE, Streamable HTTP | **~None** (Dynamic Context Discovery; legacy: 40) | `~/.cursor/mcp.json` or `.cursor/mcp.json` | `~/.cursor/mcp.json` or `.cursor/mcp.json` | `%USERPROFILE%\.cursor\mcp.json` | 2025-06-18 |
| Windsurf | `windsurf-client` | stdio, SSE, Streamable HTTP | **100** | `~/.codeium/windsurf/mcp_config.json` | `~/.codeium/windsurf/mcp_config.json` | `%USERPROFILE%\.codeium\windsurf\mcp_config.json` | 2025-03-26 |
| VS Code Copilot | `Visual-Studio-Code` | stdio, SSE, Streamable HTTP | **128** (Virtual Tools mitigates) | `.vscode/mcp.json` or `~/Library/Application Support/Code/User/mcp.json` | `.vscode/mcp.json` or `~/.config/Code/User/mcp.json` | `.vscode\mcp.json` or `%APPDATA%\Code\User\mcp.json` | 2025-06-18 |
| Gemini CLI | `gemini-cli-mcp-client` | stdio, SSE, Streamable HTTP | None documented | `~/.gemini/settings.json` or `.gemini/settings.json` | `~/.gemini/settings.json` or `.gemini/settings.json` | `~/.gemini/settings.json` or `.gemini/settings.json` | 2025-06-18 |
| Codex CLI | UNCONFIRMED | stdio, Streamable HTTP | None documented | `~/.codex/config.toml` or `.codex/config.toml` | `~/.codex/config.toml` or `.codex/config.toml` | `%USERPROFILE%\.codex\config.toml` | UNCONFIRMED |
| OpenCode | `opencode` | SSE, Streamable HTTP (auto-negotiation) | None documented | `~/.config/opencode/opencode.json` or `opencode.json` | `~/.config/opencode/opencode.json` or `opencode.json` | UNCONFIRMED | 2025-06-18 |
| Zed | `Zed` | stdio only | None documented | `~/.config/zed/settings.json` | `~/.config/zed/settings.json` | UNCONFIRMED | 2025-03-26 |
| Factory/Droid | UNCONFIRMED | stdio, HTTP | None documented | `~/.factory/mcp.json` or `.factory/mcp.json` | `~/.factory/mcp.json` or `.factory/mcp.json` | UNCONFIRMED | UNCONFIRMED |

---

## Corrected Client Detection Code

The current detection code in `CLIENT-COMPAT.md` needs updates based on actual `clientInfo.name` values:

```rust
fn detect_client(client_info: &ClientInfo) -> ClientType {
    match client_info.name.as_str() {
        // Exact matches first (preferred)
        "claude-code" => ClientType::ClaudeCode,
        "claude-ai" => ClientType::ClaudeDesktop,  // NOTE: also used by Claude.ai web
        "cursor-vscode" => ClientType::Cursor,
        "windsurf-client" => ClientType::Windsurf,
        "Visual-Studio-Code" => ClientType::VSCodeCopilot,
        "gemini-cli-mcp-client" => ClientType::GeminiCli,
        "opencode" => ClientType::OpenCode,
        "Zed" => ClientType::Zed,
        // Fuzzy fallbacks for unconfirmed/future clients
        name => {
            let lower = name.to_lowercase();
            if lower.contains("codex") {
                ClientType::CodexCli
            } else if lower.contains("factory") || lower.contains("droid") {
                ClientType::FactoryDroid
            } else if lower.contains("cursor") {
                ClientType::Cursor
            } else if lower.contains("windsurf") || lower.contains("codeium") {
                ClientType::Windsurf
            } else if lower.contains("claude") {
                ClientType::ClaudeDesktop // conservative fallback
            } else {
                ClientType::Unknown
            }
        }
    }
}
```

---

## Recommended Updates to CLIENT-COMPAT.md

### High Priority (Incorrect Information)

1. **Cursor tool limit**: Change from "40 HARD LIMIT" to "effectively removed (Dynamic Context Discovery, Jan 2026)". Add note that pre-v2.3 Cursor still has the 40 limit. Consider keeping a conservative 40-tool filter as an option.

2. **Gemini CLI transport**: Change from "Direct HTTP (NOT stdio)" to "stdio, SSE, Streamable HTTP". Gemini CLI supports all three transports. The config example using `httpUrl` is still valid but should not imply it is the ONLY option.

3. **OpenCode transport**: Change from "SSE only (no Streamable HTTP)" to "SSE and Streamable HTTP (auto-negotiation)". Both Issues #8058 and #6242 are now closed as completed.

4. **Claude Desktop clientInfo.name**: Fix the detection code. `claude-ai` does NOT match the pattern `name.contains("claude") && name.contains("desktop")`. Use exact match on `"claude-ai"`.

### Medium Priority (New Information)

5. **Add `clientInfo.name` values** for all clients to the matrix table and detection code.

6. **Add protocol versions** where known.

7. **Add cross-platform config paths** (Linux and Windows) to the config import table.

8. **VS Code Copilot**: Document the Virtual Tools feature and `github.copilot.chat.virtualTools.threshold` setting.

9. **Claude Code transport**: Document that it supports SSE and Streamable HTTP (not just stdio).

### Low Priority (Refinements)

10. **Zed config key**: Note that Zed uses `context_servers` not `mcpServers`.

11. **Windsurf OAuth**: Note OAuth support per transport type.

12. **Factory/Droid**: Add actual config paths (`~/.factory/mcp.json`).

---

## Sources Index

| Source | URL | Date Accessed |
|--------|-----|---------------|
| Apify MCP Client Capabilities | https://github.com/apify/mcp-client-capabilities | 2026-03-03 |
| Apify mcp-clients.json | https://github.com/apify/mcp-client-capabilities/blob/master/src/mcp_client_capabilities/mcp-clients.json | 2026-03-03 |
| Cursor Dynamic Context Discovery | https://cursor.com/blog/dynamic-context-discovery | 2026-01-xx |
| Cursor Forum: Tool Limit Discussion | https://forum.cursor.com/t/regarding-the-quantity-limit-of-mcp-tools/153432 | 2026-03-03 |
| Cursor 40-Tool Limit GitHub Issue | https://github.com/cursor/cursor/issues/3369 | 2026-03-03 |
| Windsurf MCP Docs | https://docs.windsurf.com/windsurf/cascade/mcp | 2026-03-03 |
| VS Code MCP Docs | https://code.visualstudio.com/docs/copilot/customization/mcp-servers | 2026-03-03 |
| VS Code Issue #290356 | https://github.com/microsoft/vscode/issues/290356 | 2026-03-03 |
| Gemini CLI MCP Docs | https://google-gemini.github.io/gemini-cli/docs/tools/mcp-server.html | 2026-03-03 |
| Gemini CLI Timeout Bug #17787 | https://github.com/google-gemini/gemini-cli/issues/17787 | 2026-01-29 |
| Gemini CLI Prompts Bug #18585 | https://github.com/google-gemini/gemini-cli/issues/18585 | 2026-02-xx |
| Codex CLI MCP Docs | https://developers.openai.com/codex/mcp/ | 2026-03-03 |
| Codex resources/list Bug #8565 | https://github.com/openai/codex/issues/8565 | 2026-02-xx |
| OpenCode Issue #8058 (CLOSED) | https://github.com/anomalyco/opencode/issues/8058 | 2026-01-14 |
| OpenCode Issue #6242 (CLOSED) | https://github.com/anomalyco/opencode/issues/6242 | 2025-12-27 |
| OpenCode MCP Docs | https://opencode.ai/docs/mcp-servers/ | 2026-03-03 |
| Claude Code MCP Docs | https://code.claude.com/docs/en/mcp | 2026-03-03 |
| Claude Desktop MCP Docs | https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop | 2026-03-03 |
| Claude Desktop Remote Connectors | https://support.claude.com/en/articles/11503834-building-custom-connectors-via-remote-mcp-servers | 2026-03-03 |
| Zed MCP Docs | https://zed.dev/docs/ai/mcp | 2026-03-03 |
| Zed HTTP Discussion #29370 | https://github.com/zed-industries/zed/discussions/29370 | 2026-03-03 |
| Factory MCP Docs | https://docs.factory.ai/cli/configuration/mcp | 2026-03-03 |
| MCP Tool Search (Claude Code) | https://medium.com/@joe.njenga/claude-code-just-cut-mcp-context-bloat-by-46-9-51k-tokens-down-to-8-5k-with-new-tool-search-ddf9e905f734 | 2026-01-14 |
| InfoQ: Cursor Dynamic Context Discovery | https://www.infoq.com/news/2026/01/cursor-dynamic-context-discovery/ | 2026-01-xx |
