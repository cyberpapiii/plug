//! Config export: generate client-specific MCP config pointing at plug.
//!
//! Supports target clients with both stdio and HTTP transport options.

use serde::Serialize;

// ── Types ───────────────────────────────────────────────────────────────────

/// Target client for config export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ExportTarget {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    Devin,
    VSCodeCopilot,
    CopilotCli,
    GeminiCli,
    CodexCli,
    GrokBuild,
    OpenCode,
    Zed,
    Cline,
    ClineCli,
    RooCode,
    Factory,
    Nanobot,
    Junie,
    Kilo,
    Pi,
    Warp,
    Kiro,
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
            // `windsurf` was the target name until Cognition renamed the
            // product; it stays accepted so old scripts keep working.
            "devin" | "windsurf" => Ok(Self::Devin),
            "vscode" => Ok(Self::VSCodeCopilot),
            "copilot" | "copilot-cli" => Ok(Self::CopilotCli),
            "gemini" | "gemini-cli" => Ok(Self::GeminiCli),
            "codex" | "codex-cli" => Ok(Self::CodexCli),
            "grok" | "grok-build" => Ok(Self::GrokBuild),
            "opencode" => Ok(Self::OpenCode),
            "zed" => Ok(Self::Zed),
            "cline" => Ok(Self::Cline),
            "cline-cli" => Ok(Self::ClineCli),
            "roocode" => Ok(Self::RooCode),
            "factory" => Ok(Self::Factory),
            "nanobot" => Ok(Self::Nanobot),
            "junie" => Ok(Self::Junie),
            "kilo" => Ok(Self::Kilo),
            "pi" => Ok(Self::Pi),
            "warp" => Ok(Self::Warp),
            "kiro" => Ok(Self::Kiro),
            "antigravity" => Ok(Self::Antigravity),
            "goose" => Ok(Self::Goose),
            _ => Err(format!("unknown export target: {s}")),
        }
    }
}

impl ExportTarget {
    /// The one name this target goes by in config, in `plug link`, and in the
    /// `--client` argument a link carries.
    pub fn target_name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "claude-desktop",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::Devin => "devin",
            Self::VSCodeCopilot => "vscode",
            Self::CopilotCli => "copilot-cli",
            Self::GeminiCli => "gemini-cli",
            Self::CodexCli => "codex-cli",
            Self::GrokBuild => "grok-build",
            Self::OpenCode => "opencode",
            Self::Zed => "zed",
            Self::Cline => "cline",
            Self::ClineCli => "cline-cli",
            Self::RooCode => "roocode",
            Self::Factory => "factory",
            Self::Nanobot => "nanobot",
            Self::Junie => "junie",
            Self::Kilo => "kilo",
            Self::Pi => "pi",
            Self::Warp => "warp",
            Self::Kiro => "kiro",
            Self::Antigravity => "antigravity",
            Self::Goose => "goose",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Devin => "Devin",
            Self::VSCodeCopilot => "VS Code Copilot",
            Self::CopilotCli => "GitHub Copilot CLI",
            Self::GeminiCli => "Gemini CLI",
            Self::CodexCli => "Codex CLI",
            Self::GrokBuild => "Grok Build",
            Self::OpenCode => "OpenCode",
            Self::Zed => "Zed",
            Self::Cline => "Cline (VS Code)",
            Self::ClineCli => "Cline CLI",
            Self::RooCode => "RooCode",
            Self::Factory => "Factory",
            Self::Nanobot => "Nanobot",
            Self::Junie => "JetBrains Junie",
            Self::Kilo => "Kilo Code",
            Self::Pi => "Pi",
            Self::Warp => "Warp",
            Self::Kiro => "Kiro",
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
            "devin",
            "vscode",
            "copilot-cli",
            "gemini-cli",
            "codex-cli",
            "grok-build",
            "opencode",
            "zed",
            "cline",
            "cline-cli",
            "roocode",
            "factory",
            "nanobot",
            "junie",
            "kilo",
            "pi",
            "warp",
            "kiro",
            "antigravity",
            "goose",
        ]
    }
}

/// Transport mode for the exported config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Explicit HTTP endpoint to export. Falls back to localhost:<port>/mcp when absent.
    pub http_url: Option<String>,
    /// Command to use for stdio transport (e.g., "plug" or absolute path).
    pub command: String,
}

fn resolved_http_url(options: &ExportOptions) -> String {
    options
        .http_url
        .clone()
        .unwrap_or_else(|| format!("http://localhost:{}/mcp", options.port))
}

/// The arguments a stdio link runs Plug with. `--client` names the target the
/// link was written for, so the daemon knows which client a connector serves
/// without trusting the name the client reports.
pub fn connect_args(options: &ExportOptions) -> [&'static str; 3] {
    ["connect", "--client", options.target.target_name()]
}

/// Whether `args` is how a link runs the connector: `connect`, alone as older
/// links wrote it or followed by `--client <target>`.
pub fn is_connect_args<S: AsRef<str>>(args: &[S]) -> bool {
    match args {
        [connect] => connect.as_ref() == "connect",
        [connect, flag, _target] => connect.as_ref() == "connect" && flag.as_ref() == "--client",
        _ => false,
    }
}

// ── Export ───────────────────────────────────────────────────────────────────

/// Generate the config snippet for a target client.
pub fn export_config(options: &ExportOptions) -> String {
    match options.target {
        // JSON clients with "mcpServers"
        ExportTarget::ClaudeDesktop
        | ExportTarget::ClaudeCode
        | ExportTarget::Cursor
        | ExportTarget::Devin
        | ExportTarget::GeminiCli
        | ExportTarget::Cline
        | ExportTarget::ClineCli
        | ExportTarget::RooCode
        | ExportTarget::Factory
        | ExportTarget::OpenCode
        | ExportTarget::Junie
        | ExportTarget::Kilo
        | ExportTarget::Pi
        | ExportTarget::Warp
        | ExportTarget::Kiro
        | ExportTarget::Antigravity => export_json_mcp_servers(options, "mcpServers"),

        // VS Code's own files use a top-level "servers"
        ExportTarget::VSCodeCopilot => export_vscode(options),

        // Copilot CLI wants a type and a tool list on every entry
        ExportTarget::CopilotCli => export_copilot_cli(options),

        // Zed uses "context_servers"
        ExportTarget::Zed => export_json_mcp_servers(options, "context_servers"),

        // YAML clients
        ExportTarget::Goose => export_yaml_mcp_extensions(options, "extensions"),

        // Nanobot uses tools.mcpServers
        ExportTarget::Nanobot => export_nanobot(options),

        // TOML clients
        ExportTarget::CodexCli | ExportTarget::GrokBuild => export_toml(options),
    }
}

/// Generate Nanobot config with nested "tools" -> "mcpServers".
fn export_nanobot(options: &ExportOptions) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "command": options.command,
            "args": connect_args(options)
        }),
        ExportTransport::Http => serde_json::json!({
            "url": resolved_http_url(options)
        }),
    };

    let config = serde_json::json!({
        "tools": {
            "mcpServers": {
                "plug": server_entry
            }
        }
    });

    serde_json::to_string_pretty(&config).unwrap()
}

/// Generate JSON config with standard `mcpServers` key.
fn export_json_mcp_servers(options: &ExportOptions, key: &str) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "command": options.command,
            "args": connect_args(options)
        }),
        ExportTransport::Http => serde_json::json!({
            "url": resolved_http_url(options)
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
    let mut plug = serde_norway::Mapping::new();

    match options.transport {
        ExportTransport::Stdio => {
            plug.insert(
                serde_norway::Value::from("type"),
                serde_norway::Value::from("stdio"),
            );
            plug.insert(
                serde_norway::Value::from("command"),
                serde_norway::Value::from(options.command.clone()),
            );
            let args: Vec<_> = connect_args(options)
                .into_iter()
                .map(serde_norway::Value::from)
                .collect();
            plug.insert(
                serde_norway::Value::from("args"),
                serde_norway::Value::from(args),
            );
        }
        ExportTransport::Http => {
            plug.insert(
                serde_norway::Value::from("type"),
                serde_norway::Value::from("sse"),
            );
            plug.insert(
                serde_norway::Value::from("uri"),
                serde_norway::Value::from(resolved_http_url(options)),
            );
        }
    }
    plug.insert(
        serde_norway::Value::from("enabled"),
        serde_norway::Value::from(true),
    );

    let mut extensions = serde_norway::Mapping::new();
    extensions.insert(
        serde_norway::Value::from("plug"),
        serde_norway::Value::from(plug),
    );

    let mut config = serde_norway::Mapping::new();
    config.insert(
        serde_norway::Value::from(key),
        serde_norway::Value::from(extensions),
    );

    serde_norway::to_string(&config).unwrap()
}

/// Generate VS Code's `mcp.json`: a top-level `servers` object, in the
/// workspace file and the user profile file alike.
/// https://code.visualstudio.com/docs/copilot/customization/mcp-servers
fn export_vscode(options: &ExportOptions) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "type": "stdio",
            "command": options.command,
            "args": connect_args(options)
        }),
        ExportTransport::Http => serde_json::json!({
            "type": "http",
            "url": resolved_http_url(options)
        }),
    };

    let config = serde_json::json!({
        "servers": {
            "plug": server_entry
        }
    });

    serde_json::to_string_pretty(&config).unwrap()
}

/// Generate GitHub Copilot CLI's `mcp-config.json`. `type` and `tools` are
/// required on every entry; `"*"` allows every tool.
/// https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers
fn export_copilot_cli(options: &ExportOptions) -> String {
    let server_entry = match options.transport {
        ExportTransport::Stdio => serde_json::json!({
            "type": "local",
            "command": options.command,
            "args": connect_args(options),
            "tools": ["*"]
        }),
        ExportTransport::Http => serde_json::json!({
            "type": "http",
            "url": resolved_http_url(options),
            "tools": ["*"]
        }),
    };

    let config = serde_json::json!({
        "mcpServers": {
            "plug": server_entry
        }
    });

    serde_json::to_string_pretty(&config).unwrap()
}

/// Generate TOML config for Codex CLI and Grok Build, which share the
/// `[mcp_servers.<name>]` shape.
fn export_toml(options: &ExportOptions) -> String {
    match options.transport {
        ExportTransport::Stdio => format!(
            r#"[mcp_servers.plug]
command = "{}"
args = ["connect", "--client", "{}"]
"#,
            options.command,
            options.target.target_name()
        ),
        ExportTransport::Http => {
            format!(
                r#"[mcp_servers.plug]
transport = "http"
url = "{}"
"#,
                resolved_http_url(options)
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
        // Devin's own file since Devin CLI v3000.3; Devin Desktop's default
        // agent reads it too. The Cascade file the Windsurf target wrote,
        // ~/.codeium/windsurf/mcp_config.json, is import-only now.
        // https://docs.devin.ai/cli/extensibility/mcp/configuration
        ExportTarget::Devin => {
            if project {
                Some(std::path::PathBuf::from(".devin/mcp_config.json"))
            } else {
                Some(home.join(".config/devin/mcp_config.json"))
            }
        }
        // The user file lives in the default profile folder. Until 0.8.14 this
        // target wrote ~/.copilot/mcp-config.json, which belongs to Copilot
        // CLI and takes a different shape.
        ExportTarget::VSCodeCopilot => {
            if project {
                Some(std::path::PathBuf::from(".vscode/mcp.json"))
            } else {
                Some(dirs::config_dir()?.join("Code/User/mcp.json"))
            }
        }
        // One user file. The project files Copilot CLI reads, `.mcp.json` and
        // `.github/mcp.json`, are shared with other clients that reject its
        // extra fields, so there is no project path.
        ExportTarget::CopilotCli => Some(home.join(".copilot/mcp-config.json")),
        ExportTarget::GeminiCli => {
            if project {
                Some(std::path::PathBuf::from(".gemini/settings.json"))
            } else {
                Some(home.join(".gemini/settings.json"))
            }
        }
        ExportTarget::CodexCli => Some(home.join(".codex/config.toml")),
        // https://docs.x.ai/build/settings: the project file wins over the
        // user file for `[mcp_servers]`.
        ExportTarget::GrokBuild => {
            if project {
                Some(std::path::PathBuf::from(".grok/config.toml"))
            } else {
                Some(home.join(".grok/config.toml"))
            }
        }
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
                Some(std::path::PathBuf::from(".nanobot/config.json"))
            } else {
                Some(home.join(".nanobot/config.json"))
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
        // Built in since Pi 0.99.0.
        // https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/mcp.md
        ExportTarget::Pi => {
            if project {
                Some(std::path::PathBuf::from(".pi/mcp.json"))
            } else {
                Some(home.join(".pi/agent/mcp.json"))
            }
        }
        // Warp starts the servers in these files on its own.
        // https://docs.warp.dev/agent-platform/capabilities/mcp/
        ExportTarget::Warp => {
            if project {
                Some(std::path::PathBuf::from(".warp/.mcp.json"))
            } else {
                Some(home.join(".warp/.mcp.json"))
            }
        }
        // https://kiro.dev/docs/mcp/configuration/
        ExportTarget::Kiro => {
            if project {
                Some(std::path::PathBuf::from(".kiro/settings/mcp.json"))
            } else {
                Some(home.join(".kiro/settings/mcp.json"))
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
            http_url: None,
            command: "plug".to_string(),
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
            http_url: None,
            command: "plug".to_string(),
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["mcpServers"]["plug"]["url"],
            "http://localhost:3282/mcp"
        );
    }

    #[test]
    fn export_vscode_uses_top_level_servers() {
        let options = |transport| ExportOptions {
            target: ExportTarget::VSCodeCopilot,
            transport,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
        };
        let stdio: serde_json::Value =
            serde_json::from_str(&export_config(&options(ExportTransport::Stdio))).unwrap();
        assert_eq!(stdio["servers"]["plug"]["type"], "stdio");
        assert_eq!(stdio["servers"]["plug"]["command"], "plug");
        assert!(stdio.get("mcp").is_none());
        let http: serde_json::Value =
            serde_json::from_str(&export_config(&options(ExportTransport::Http))).unwrap();
        assert_eq!(http["servers"]["plug"]["type"], "http");
        assert_eq!(http["servers"]["plug"]["url"], "http://localhost:3282/mcp");
    }

    #[test]
    fn export_copilot_cli_carries_the_fields_it_requires() {
        let options = |transport| ExportOptions {
            target: "copilot-cli".parse().unwrap(),
            transport,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
        };
        let stdio: serde_json::Value =
            serde_json::from_str(&export_config(&options(ExportTransport::Stdio))).unwrap();
        let plug = &stdio["mcpServers"]["plug"];
        assert_eq!(plug["type"], "local");
        assert_eq!(plug["command"], "plug");
        assert_eq!(plug["tools"], serde_json::json!(["*"]));
        let http: serde_json::Value =
            serde_json::from_str(&export_config(&options(ExportTransport::Http))).unwrap();
        let plug = &http["mcpServers"]["plug"];
        assert_eq!(plug["type"], "http");
        assert_eq!(plug["tools"], serde_json::json!(["*"]));

        for (name, user, project) in [
            ("pi", ".pi/agent/mcp.json", ".pi/mcp.json"),
            ("warp", ".warp/.mcp.json", ".warp/.mcp.json"),
            ("kiro", ".kiro/settings/mcp.json", ".kiro/settings/mcp.json"),
        ] {
            let target: ExportTarget = name.parse().unwrap();
            assert!(default_config_path(target, false).unwrap().ends_with(user));
            assert_eq!(
                default_config_path(target, true).unwrap(),
                std::path::PathBuf::from(project)
            );
        }
        // VS Code and Copilot CLI must not write the same user file.
        assert_ne!(
            default_config_path(ExportTarget::VSCodeCopilot, false),
            default_config_path(ExportTarget::CopilotCli, false)
        );
    }

    #[test]
    fn export_codex_toml_stdio() {
        let options = ExportOptions {
            target: ExportTarget::CodexCli,
            transport: ExportTransport::Stdio,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
        };
        let output = export_config(&options);
        assert!(output.contains("[mcp_servers.plug]"));
        assert!(output.contains("command = \"plug\""));
    }

    /// Every stdio link says which target it was written for, in the name
    /// `plug connect --client` and the config both accept.
    #[test]
    fn every_stdio_link_names_its_target() {
        let targets = [
            ExportTarget::ClaudeDesktop,
            ExportTarget::ClaudeCode,
            ExportTarget::Cursor,
            ExportTarget::Devin,
            ExportTarget::VSCodeCopilot,
            ExportTarget::CopilotCli,
            ExportTarget::GeminiCli,
            ExportTarget::CodexCli,
            ExportTarget::GrokBuild,
            ExportTarget::OpenCode,
            ExportTarget::Zed,
            ExportTarget::Cline,
            ExportTarget::ClineCli,
            ExportTarget::RooCode,
            ExportTarget::Factory,
            ExportTarget::Nanobot,
            ExportTarget::Junie,
            ExportTarget::Kilo,
            ExportTarget::Pi,
            ExportTarget::Warp,
            ExportTarget::Kiro,
            ExportTarget::Antigravity,
            ExportTarget::Goose,
        ];
        for target in &targets {
            let options = ExportOptions {
                target: *target,
                transport: ExportTransport::Stdio,
                port: 3282,
                http_url: None,
                command: "plug".to_string(),
            };
            let name = target.target_name();
            assert_eq!(name.parse::<ExportTarget>(), Ok(*target));
            assert_eq!(connect_args(&options), ["connect", "--client", name]);
            let output = export_config(&options);
            let position = |needle: &str| {
                output
                    .find(needle)
                    .unwrap_or_else(|| panic!("{name} link lacks {needle}: {output}"))
            };
            assert!(position("connect") < position("--client"));
            assert!(position("--client") < position(name));
        }
    }

    #[test]
    fn connect_args_are_recognised_with_and_without_a_target() {
        assert!(is_connect_args(&["connect"]));
        assert!(is_connect_args(&["connect", "--client", "cursor"]));
        assert!(!is_connect_args(&["serve"]));
        assert!(!is_connect_args(&["connect", "--client"]));
        assert!(!is_connect_args(&["connect", "--config", "/tmp/x.toml"]));
        assert!(!is_connect_args::<&str>(&[]));
    }

    #[test]
    fn export_grok_build_toml_http() {
        let options = ExportOptions {
            target: ExportTarget::GrokBuild,
            transport: ExportTransport::Http,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
        };
        let output = export_config(&options);
        let parsed: toml::Value = toml::from_str(&output).unwrap();
        let plug = &parsed["mcp_servers"]["plug"];
        assert_eq!(plug["transport"].as_str(), Some("http"));
        assert_eq!(plug["url"].as_str(), Some("http://localhost:3282/mcp"));
    }

    #[test]
    fn devin_target_accepts_its_former_name() {
        assert_eq!("windsurf".parse::<ExportTarget>(), Ok(ExportTarget::Devin));
        assert_eq!("devin".parse::<ExportTarget>(), Ok(ExportTarget::Devin));
        assert_eq!(ExportTarget::Devin.display_name(), "Devin");
        assert_eq!("grok".parse::<ExportTarget>(), Ok(ExportTarget::GrokBuild));
    }

    #[test]
    fn export_zed_context_servers() {
        let options = ExportOptions {
            target: ExportTarget::Zed,
            transport: ExportTransport::Stdio,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
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

    #[test]
    fn export_http_uses_explicit_url_when_provided() {
        let options = ExportOptions {
            target: ExportTarget::Cursor,
            transport: ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        };
        let output = export_config(&options);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["mcpServers"]["plug"]["url"],
            "https://plug.example.com/mcp"
        );
    }

    #[test]
    fn export_goose_http_yaml_has_expected_shape() {
        let options = ExportOptions {
            target: ExportTarget::Goose,
            transport: ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        };
        let output = export_config(&options);
        let parsed: serde_norway::Value = serde_norway::from_str(&output).unwrap();
        let plug = parsed
            .get("extensions")
            .and_then(|value| value.get("plug"))
            .expect("plug extension");

        assert_eq!(
            plug.get("type").and_then(|value| value.as_str()),
            Some("sse")
        );
        assert_eq!(
            plug.get("uri").and_then(|value| value.as_str()),
            Some("https://plug.example.com/mcp")
        );
        assert_eq!(
            plug.get("enabled").and_then(|value| value.as_bool()),
            Some(true)
        );
    }
}
