//! Config export: generate client-specific MCP config pointing at plug.
//!
//! Supports 12 target clients with both stdio and HTTP transport options.

use serde::Serialize;

// ── Types ───────────────────────────────────────────────────────────────────

/// Target client for config export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ExportTarget {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    Windsurf,
    VSCodeCopilot,
    GeminiCli,
    CodexCli,
    OpenCode,
    Zed,
    Cline,
    ClineCli,
    RooCode,
    Factory,
    Nanobot,
    Junie,
    Kilo,
    Antigravity,
    Goose,
}

impl std::str::FromStr for ExportTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-desktop" => Ok(Self::ClaudeDesktop),
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "windsurf" => Ok(Self::Windsurf),
            "vscode" => Ok(Self::VSCodeCopilot),
            "gemini" | "gemini-cli" => Ok(Self::GeminiCli),
            "codex" | "codex-cli" => Ok(Self::CodexCli),
            "opencode" => Ok(Self::OpenCode),
            "zed" => Ok(Self::Zed),
            "cline" => Ok(Self::Cline),
            "cline-cli" => Ok(Self::ClineCli),
            "roocode" => Ok(Self::RooCode),
            "factory" => Ok(Self::Factory),
            "nanobot" => Ok(Self::Nanobot),
            "junie" => Ok(Self::Junie),
            "kilo" => Ok(Self::Kilo),
            "antigravity" => Ok(Self::Antigravity),
            "goose" => Ok(Self::Goose),
            _ => Err(format!("unknown export target: {s}")),
        }
    }
}

impl ExportTarget {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
            Self::VSCodeCopilot => "VS Code Copilot",
            Self::GeminiCli => "Gemini CLI",
            Self::CodexCli => "Codex CLI",
            Self::OpenCode => "OpenCode",
            Self::Zed => "Zed",
            Self::Cline => "Cline (VS Code)",
            Self::ClineCli => "Cline CLI",
            Self::RooCode => "RooCode",
            Self::Factory => "Factory",
            Self::Nanobot => "Nanobot",
            Self::Junie => "JetBrains Junie",
            Self::Kilo => "Kilo Code",
            Self::Antigravity => "Google Antigravity",
            Self::Goose => "Goose",
        }
    }

    /// All supported target names for CLI help text.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "claude-desktop",
            "claude-code",
            "cursor",
            "windsurf",
            "vscode",
            "gemini-cli",
            "codex-cli",
            "opencode",
            "zed",
            "cline",
            "cline-cli",
            "roocode",
            "factory",
            "nanobot",
            "junie",
            "kilo",
            "antigravity",
            "goose",
        ]
    }
}

/// Transport mode for the exported config.
#[derive(Debug, Clone, Copy)]
pub enum ExportTransport {
    /// stdio via `plug connect`
    Stdio,
    /// HTTP via `http://localhost:<port>/mcp`
    Http,
}

/// Options for export.
pub struct ExportOptions {
    pub target: ExportTarget,
    pub transport: ExportTransport,
    pub port: u16,
}

// ── Export ───────────────────────────────────────────────────────────────────

/// Generate the config snippet for a target client.
pub fn export_config(options: &ExportOptions) -> String {
    match options.target {
        // JSON clients with "mcpServers"
        ExportTarget::ClaudeDesktop
        | ExportTarget::ClaudeCode
        | ExportTarget::Cursor
        | ExportTarget::Windsurf
        | ExportTarget::GeminiCli
        | ExportTarget::Cline
        | ExportTarget::ClineCli
        | ExportTarget::RooCode
        | ExportTarget::Factory
        | ExportTarget::OpenCode
        | ExportTarget::Junie
        | ExportTarget::Kilo
        | ExportTarget::Antigravity => export_json_mcp_servers(options, "mcpServers"),

        // VS Code uses nested "mcp" -> "servers"
        ExportTarget::VSCodeCopilot => export_vscode(options),

        // Zed uses "context_servers"
        ExportTarget::Zed => export_json_mcp_servers(options, "context_servers"),

        // YAML clients
        ExportTarget::Goose => export_yaml_mcp_extensions(options, "extensions"),

        // TOML clients
        ExportTarget::CodexCli | ExportTarget::Nanobot => export_toml(options),
    }
}

/// Generate JSON config with standard `mcpServers` key.
fn export_json_mcp_servers(options: &ExportOptions, key: &str) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "command": "plug",
            "args": ["connect"]
        }),
        ExportTransport::Http => serde_json::json!({
            "url": format!("http://localhost:{}/mcp", options.port)
        }),
    };

    let config = serde_json::json!({
        key: {
            "plug": server_entry
        }
    });

    serde_json::to_string_pretty(&config).unwrap()
}

/// Generate a YAML MCP config snippet.
fn export_yaml_mcp_extensions(options: &ExportOptions, key: &str) -> String {
    let mut plug = serde_yml::Mapping::new();
    
    match options.transport {
        ExportTransport::Stdio => {
            plug.insert(serde_yml::Value::from("type"), serde_yml::Value::from("stdio"));
            plug.insert(serde_yml::Value::from("command"), serde_yml::Value::from("plug"));
            let mut args = serde_yml::Sequence::new();
            args.push(serde_yml::Value::from("connect"));
            plug.insert(serde_yml::Value::from("args"), serde_yml::Value::from(args));
        }
        ExportTransport::Http => {
            plug.insert(serde_yml::Value::from("type"), serde_yml::Value::from("sse"));
            plug.insert(serde_yml::Value::from("uri"), serde_yml::Value::from(format!("http://localhost:{}/mcp", options.port)));
        }
    }
    plug.insert(serde_yml::Value::from("enabled"), serde_yml::Value::from(true));

    let mut extensions = serde_yml::Mapping::new();
    extensions.insert(serde_yml::Value::from("plug"), serde_yml::Value::from(plug));

    let mut config = serde_yml::Mapping::new();
    config.insert(serde_yml::Value::from(key), serde_yml::Value::from(extensions));

    serde_yml::to_string(&config).unwrap()
}

/// Generate VS Code config with nested "mcp" -> "servers".
fn export_vscode(options: &ExportOptions) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "command": "plug",
            "args": ["connect"]
        }),
        ExportTransport::Http => serde_json::json!({
            "url": format!("http://localhost:{}/mcp", options.port)
        }),
    };

    let config = serde_json::json!({
        "mcp": {
            "servers": {
                "plug": server_entry
            }
        }
    });

    serde_json::to_string_pretty(&config).unwrap()
}

/// Generate TOML config for Codex/Nanobot.
fn export_toml(options: &ExportOptions) -> String {
    match options.transport {
        ExportTransport::Stdio => r#"[mcp_servers.plug]
command = "plug"
args = ["connect"]
"#
        .to_string(),
        ExportTransport::Http => {
            format!(
                r#"[mcp_servers.plug]
transport = "http"
url = "http://localhost:{}/mcp"
"#,
                options.port
            )
        }
    }
}

/// Get the default config file path for a target client.
pub fn default_config_path(target: ExportTarget, project: bool) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;

    match target {
        ExportTarget::ClaudeDesktop => {
            #[cfg(target_os = "macos")]
            {
                Some(home.join("Library/Application Support/Claude/claude_desktop_config.json"))
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        }
        ExportTarget::ClaudeCode => {
            if project {
                Some(std::path::PathBuf::from(".mcp.json"))
            } else {
                Some(home.join(".claude.json"))
            }
        }
        ExportTarget::Cursor => {
            if project {
                Some(std::path::PathBuf::from(".cursor/mcp.json"))
            } else {
                Some(home.join(".cursor/mcp.json"))
            }
        }
        ExportTarget::Windsurf => Some(home.join(".codeium/windsurf/mcp_config.json")),
        ExportTarget::VSCodeCopilot => {
            if project {
                Some(std::path::PathBuf::from(".vscode/mcp.json"))
            } else {
                Some(home.join(".copilot/mcp-config.json"))
            }
        }
        ExportTarget::GeminiCli => {
            if project {
                Some(std::path::PathBuf::from(".gemini/settings.json"))
            } else {
                Some(home.join(".gemini/settings.json"))
            }
        }
        ExportTarget::CodexCli => Some(home.join(".codex/config.toml")),
        ExportTarget::OpenCode => {
            if project {
                Some(std::path::PathBuf::from("opencode.json"))
            } else {
                Some(home.join(".config/opencode/opencode.json"))
            }
        }
        ExportTarget::Zed => Some(home.join(".config/zed/settings.json")),
        ExportTarget::Cline => {
            Some(home.join(
                ".vscode/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            ))
        }
        ExportTarget::ClineCli => Some(home.join(".cline/data/settings/cline_mcp_settings.json")),
        ExportTarget::RooCode => {
            if project {
                Some(std::path::PathBuf::from(".roo/mcp.json"))
            } else {
                Some(home.join(".roo/mcp.json"))
            }
        }
        ExportTarget::Factory => Some(home.join(".factory/config.json")),
        ExportTarget::Nanobot => {
            if project {
                Some(std::path::PathBuf::from(".nanobot.toml"))
            } else {
                Some(home.join(".nanobot/config.toml"))
            }
        }
        ExportTarget::Junie => {
            if project {
                Some(std::path::PathBuf::from(".junie/mcp/mcp.json"))
            } else {
                Some(home.join(".junie/mcp/mcp.json"))
            }
        }
        ExportTarget::Kilo => {
            if project {
                Some(std::path::PathBuf::from("opencode.json"))
            } else {
                Some(home.join(".config/kilo/opencode.json"))
            }
        }
        ExportTarget::Antigravity => {
            #[cfg(target_os = "macos")]
            {
                Some(home.join("Library/Application Support/Antigravity/antigravity_config.json"))
            }
            #[cfg(target_os = "windows")]
            if let Some(appdata) = std::env::var_os("APPDATA") {
                Some(std::path::PathBuf::from(appdata).join("Antigravity/antigravity_config.json"))
            } else {
                None
            }
            #[cfg(target_os = "linux")]
            {
                Some(home.join(".config/Antigravity/antigravity_config.json"))
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
            {
                None
            }
        }
        ExportTarget::Goose => {
            #[cfg(target_os = "windows")]
            if let Some(appdata) = std::env::var_os("APPDATA") {
                Some(PathBuf::from(appdata).join("Block/goose/config/config.yaml"))
            } else {
                None
            }
            #[cfg(not(target_os = "windows"))]
            {
                Some(home.join(".config/goose/config.yaml"))
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_claude_desktop_stdio() {
        let options = ExportOptions {
            target: ExportTarget::ClaudeDesktop,
            transport: ExportTransport::Stdio,
            port: 3282,
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["mcpServers"]["plug"]["command"], "plug");
        assert_eq!(parsed["mcpServers"]["plug"]["args"][0], "connect");
    }

    #[test]
    fn export_cursor_http() {
        let options = ExportOptions {
            target: ExportTarget::Cursor,
            transport: ExportTransport::Http,
            port: 3282,
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["mcpServers"]["plug"]["url"],
            "http://localhost:3282/mcp"
        );
    }

    #[test]
    fn export_vscode_nested() {
        let options = ExportOptions {
            target: ExportTarget::VSCodeCopilot,
            transport: ExportTransport::Stdio,
            port: 3282,
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["mcp"]["servers"]["plug"]["command"], "plug");
    }

    #[test]
    fn export_codex_toml_stdio() {
        let options = ExportOptions {
            target: ExportTarget::CodexCli,
            transport: ExportTransport::Stdio,
            port: 3282,
        };
        let output = export_config(&options);
        assert!(output.contains("[mcp_servers.plug]"));
        assert!(output.contains("command = \"plug\""));
    }

    #[test]
    fn export_zed_context_servers() {
        let options = ExportOptions {
            target: ExportTarget::Zed,
            transport: ExportTransport::Stdio,
            port: 3282,
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["context_servers"]["plug"]["command"], "plug");
    }

    #[test]
    fn all_names_roundtrip() {
        for name in ExportTarget::all_names() {
            assert!(
                name.parse::<ExportTarget>().is_ok(),
                "failed to parse: {name}"
            );
        }
    }
}
