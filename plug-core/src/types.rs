use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a client session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Known AI client types that connect to plug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientType {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Windsurf,
    VSCodeCopilot,
    GeminiCli,
    CodexCli,
    OpenCode,
    Zed,
    Unknown,
}

impl ClientType {
    /// Returns the maximum number of tools this client supports, if known.
    pub fn tool_limit(&self) -> Option<usize> {
        match self {
            ClientType::Windsurf => Some(100),
            ClientType::VSCodeCopilot => Some(128),
            _ => None,
        }
    }
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ClientType::ClaudeCode => "Claude Code",
            ClientType::ClaudeDesktop => "Claude Desktop",
            ClientType::Cursor => "Cursor",
            ClientType::Windsurf => "Windsurf",
            ClientType::VSCodeCopilot => "VS Code Copilot",
            ClientType::GeminiCli => "Gemini CLI",
            ClientType::CodexCli => "Codex CLI",
            ClientType::OpenCode => "OpenCode",
            ClientType::Zed => "Zed",
            ClientType::Unknown => "Unknown",
        };
        write!(f, "{name}")
    }
}

/// Health state of an upstream server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerHealth {
    /// Server is responding normally.
    Healthy,
    /// Server is responding but with degraded performance (timeouts, partial failures).
    Degraded,
    /// Server is not responding.
    Failed,
}

/// Status information for an upstream server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub server_id: String,
    pub health: ServerHealth,
    pub tool_count: usize,
    #[serde(skip)]
    pub last_seen: Option<std::time::Instant>,
}
