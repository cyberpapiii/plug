use crate::clients::ClientTarget;
use crate::types::ClientType;

/// Clients Plug recognises but cannot link, so they have no row in
/// `crate::clients`. Neither xAI client's `clientInfo.name` is documented:
/// Grok Bot is the one with "bot" in its name.
const UNLINKED: &[(ClientType, &[&[&str]])] = &[(ClientType::GrokBot, &[&["grok", "bot"]])];

/// The client a `clientInfo.name` from an MCP initialize request names: one
/// a client is known to send, or else the most specific words it holds.
pub fn detect_client(client_info_name: &str) -> ClientType {
    let rows = || {
        ClientTarget::all()
            .map(ClientTarget::info)
            .filter_map(|info| Some((info.live?, info.reports, info.reports_like)))
    };
    if let Some((client, _, _)) = rows().find(|(_, reports, _)| reports.contains(&client_info_name))
    {
        return client;
    }
    let name = client_info_name.to_lowercase();
    let matched = |groups: &[&[&str]]| {
        groups
            .iter()
            .filter(|words| words.iter().all(|word| name.contains(word)))
            .map(|words| words.iter().map(|word| word.len()).sum::<usize>())
            .max()
    };
    rows()
        .map(|(client, _, like)| (client, like))
        .chain(UNLINKED.iter().copied())
        .filter_map(|(client, like)| Some((matched(like)?, client)))
        .fold(
            None,
            |best: Option<(usize, ClientType)>, (length, client)| match best {
                Some((longest, _)) if longest >= length => best,
                _ => Some((length, client)),
            },
        )
        .map_or(ClientType::Unknown, |(_, client)| client)
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
}
