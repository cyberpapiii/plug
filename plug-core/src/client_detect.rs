use crate::types::ClientType;

/// Detect the client type from the `clientInfo.name` field in an MCP InitializeRequest.
///
/// Uses exact match as primary (verified values from Apify MCP Client Capabilities Index),
/// with fuzzy fallback for unknown client versions.
pub fn detect_client(client_info_name: &str) -> ClientType {
    // Verified `clientInfo.name` values.
    match client_info_name {
        "claude-code" => return ClientType::ClaudeCode,
        "claude-ai" => return ClientType::ClaudeDesktop,
        "cursor-vscode" => return ClientType::Cursor,
        "windsurf-client" => return ClientType::Devin,
        "Visual-Studio-Code" => return ClientType::VSCodeCopilot,
        "gemini-cli-mcp-client" => return ClientType::GeminiCli,
        "opencode" => return ClientType::OpenCode,
        "Zed" => return ClientType::Zed,
        _ => {}
    }

    let name = client_info_name.to_lowercase();
    if name.contains("claude-code") || name.contains("claude code") {
        ClientType::ClaudeCode
    } else if name.contains("claude") {
        ClientType::ClaudeDesktop
    } else if name.contains("cursor") {
        ClientType::Cursor
    } else if name.contains("devin")
        || name.contains("cascade")
        || name.contains("windsurf")
        || name.contains("codeium")
    {
        ClientType::Devin
    } else if name.contains("copilot") && name.contains("cli") {
        // Copilot CLI's `clientInfo.name` is not documented. Anything else
        // Copilot is taken for VS Code, which keeps its 128-tool ceiling.
        ClientType::CopilotCli
    } else if name.contains("copilot") || name.contains("vscode") {
        ClientType::VSCodeCopilot
    } else if name.contains("gemini") {
        ClientType::GeminiCli
    } else if name.contains("codex") {
        ClientType::CodexCli
    } else if name.contains("grok") {
        // Neither xAI client's `clientInfo.name` is documented. Grok Bot is
        // the one with "bot" in its name; everything else Grok is the CLI.
        if name.contains("bot") {
            ClientType::GrokBot
        } else {
            ClientType::GrokBuild
        }
    } else if name.contains("opencode") {
        ClientType::OpenCode
    } else if name.contains("zed") {
        ClientType::Zed
    } else {
        ClientType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_all_known_clients() {
        assert_eq!(detect_client("claude-code"), ClientType::ClaudeCode);
        assert_eq!(detect_client("claude-ai"), ClientType::ClaudeDesktop);
        assert_eq!(detect_client("cursor-vscode"), ClientType::Cursor);
        assert_eq!(detect_client("windsurf-client"), ClientType::Devin);
        assert_eq!(
            detect_client("Visual-Studio-Code"),
            ClientType::VSCodeCopilot
        );
        assert_eq!(
            detect_client("gemini-cli-mcp-client"),
            ClientType::GeminiCli
        );
        assert_eq!(detect_client("opencode"), ClientType::OpenCode);
        assert_eq!(detect_client("Zed"), ClientType::Zed);
    }

    #[test]
    fn fuzzy_fallback() {
        assert_eq!(detect_client("Claude Code v2"), ClientType::ClaudeCode);
        assert_eq!(detect_client("claude-desktop"), ClientType::ClaudeDesktop);
        assert_eq!(detect_client("cursor-next"), ClientType::Cursor);
        assert_eq!(detect_client("codeium-editor"), ClientType::Devin);
        assert_eq!(detect_client("devin-cli"), ClientType::Devin);
        assert_eq!(detect_client("Cascade (Devin Desktop)"), ClientType::Devin);
        assert_eq!(detect_client("github-copilot"), ClientType::VSCodeCopilot);
        assert_eq!(detect_client("github-copilot-cli"), ClientType::CopilotCli);
        assert_eq!(detect_client("Copilot CLI"), ClientType::CopilotCli);
        assert_eq!(detect_client("gemini-cli-v2"), ClientType::GeminiCli);
        assert_eq!(detect_client("codex-cli"), ClientType::CodexCli);
        assert_eq!(detect_client("grok-cli"), ClientType::GrokBuild);
        assert_eq!(detect_client("Grok Build"), ClientType::GrokBuild);
        assert_eq!(detect_client("grok-bot"), ClientType::GrokBot);
        assert_eq!(detect_client("Grok Bot"), ClientType::GrokBot);
        assert_eq!(detect_client("opencode-v2"), ClientType::OpenCode);
        assert_eq!(detect_client("zed-preview"), ClientType::Zed);
    }

    #[test]
    fn unknown_client() {
        assert_eq!(detect_client("some-random-client"), ClientType::Unknown);
        assert_eq!(detect_client(""), ClientType::Unknown);
    }

    #[test]
    fn tool_limit_known_clients() {
        assert_eq!(ClientType::Devin.tool_limit(), Some(100));
        assert_eq!(ClientType::VSCodeCopilot.tool_limit(), Some(128));
    }
}
