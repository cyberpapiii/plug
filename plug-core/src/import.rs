//! Config import: scan AI client config files and extract MCP server definitions.
//!
//! Supports 12 clients: Claude Desktop, Claude Code, Cursor, Windsurf,
//! VS Code Copilot, Gemini CLI, Codex CLI, OpenCode, Zed, Cline, Factory, Nanobot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{ServerConfig, TransportType};

// ── Types ───────────────────────────────────────────────────────────────────

/// A server discovered from a client config file.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredServer {
    /// Server name as defined in the source config.
    pub name: String,
    /// Converted server config.
    pub config: ServerConfig,
    /// Which client this was discovered from.
    pub source: ClientSource,
}

/// Identifies which client config a server was imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ClientSource {
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

impl ClientSource {
    /// Human-readable name for display.
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

    /// All known client sources.
    pub fn all() -> &'static [ClientSource] {
        &[
            Self::ClaudeDesktop,
            Self::ClaudeCode,
            Self::Cursor,
            Self::Windsurf,
            Self::VSCodeCopilot,
            Self::GeminiCli,
            Self::CodexCli,
            Self::OpenCode,
            Self::Zed,
            Self::Cline,
            Self::ClineCli,
            Self::RooCode,
            Self::Factory,
            Self::Nanobot,
            Self::Junie,
            Self::Kilo,
            Self::Antigravity,
            Self::Goose,
        ]
    }
}

impl std::fmt::Display for ClientSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Result of scanning a single client.
#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub source: ClientSource,
    pub servers: Vec<DiscoveredServer>,
    pub error: Option<String>,
}

/// Result of a full import operation.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub scanned: Vec<ScanResult>,
    pub new_servers: Vec<DiscoveredServer>,
    pub skipped: usize,
    pub duplicates_merged: usize,
}

// ── Scanner ─────────────────────────────────────────────────────────────────

/// Scan a specific client source for MCP server definitions.
pub fn scan_client(source: ClientSource) -> ScanResult {
    let paths = config_paths(source);
    let mut servers = Vec::new();
    let mut error = None;

    for path in paths {
        if !path.exists() {
            continue;
        }
        match parse_config(source, &path) {
            Ok(mut found) => {
                // Ignore any server named "plug" to avoid recursion
                found.retain(|s| s.name != "plug");
                servers.append(&mut found);
            }
            Err(e) => {
                error = Some(format!("{}: {e}", path.display()));
            }
        }
    }

    ScanResult {
        source,
        servers,
        error,
    }
}

/// Scan all clients and return results.
pub fn scan_all() -> Vec<ScanResult> {
    ClientSource::all()
        .iter()
        .map(|s| scan_client(*s))
        .collect()
}

/// Run the full import: scan, deduplicate, and compute what's new.
pub fn import(existing: &HashMap<String, ServerConfig>, sources: &[ClientSource]) -> ImportReport {
    let scanned: Vec<ScanResult> = sources.iter().map(|s| scan_client(*s)).collect();

    // Collect all discovered servers
    let mut all_discovered: Vec<DiscoveredServer> =
        scanned.iter().flat_map(|r| r.servers.clone()).collect();

    // Deduplicate by (command, args) signature
    let duplicates_before = all_discovered.len();
    dedup_servers(&mut all_discovered);
    let duplicates_merged = duplicates_before - all_discovered.len();

    // Filter out servers already in existing config (match by command+args)
    let new_servers: Vec<DiscoveredServer> = all_discovered
        .into_iter()
        .filter(|d| !is_existing_server(d, existing))
        .collect();

    let skipped = duplicates_before - duplicates_merged - new_servers.len();

    ImportReport {
        scanned,
        new_servers,
        skipped,
        duplicates_merged,
    }
}

// ── Config Paths ────────────────────────────────────────────────────────────

/// Returns the platform-appropriate config paths for a client.
fn config_paths(source: ClientSource) -> Vec<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    match source {
        ClientSource::ClaudeDesktop => claude_desktop_paths(&home),
        ClientSource::ClaudeCode => {
            vec![home.join(".claude.json"), PathBuf::from(".mcp.json")]
        }
        ClientSource::Cursor => {
            vec![
                home.join(".cursor/mcp.json"),
                PathBuf::from(".cursor/mcp.json"),
            ]
        }
        ClientSource::Windsurf => {
            vec![home.join(".codeium/windsurf/mcp_config.json")]
        }
        ClientSource::VSCodeCopilot => {
            vec![
                home.join(".copilot/mcp-config.json"),
                home.join(".vscode/mcp.json"),
                PathBuf::from(".vscode/mcp.json"),
                PathBuf::from(".mcp.json"),
            ]
        }
        ClientSource::GeminiCli => {
            vec![
                home.join(".gemini/settings.json"),
                PathBuf::from(".gemini/settings.json"),
            ]
        }
        ClientSource::CodexCli => {
            vec![
                home.join(".codex/config.toml"),
                PathBuf::from(".codex/config.toml"),
            ]
        }
        ClientSource::OpenCode => {
            vec![
                home.join(".config/opencode/opencode.json"),
                PathBuf::from("opencode.json"),
            ]
        }
        ClientSource::Zed => {
            vec![home.join(".config/zed/settings.json")]
        }
        ClientSource::Cline => {
            vec![home.join(
                ".vscode/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            )]
        }
        ClientSource::ClineCli => {
            vec![home.join(".cline/data/settings/cline_mcp_settings.json")]
        }
        ClientSource::RooCode => {
            vec![
                home.join(".vscode/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json"),
                home.join(".roo/mcp.json"),
                PathBuf::from(".roo/mcp.json"),
            ]
        }
        ClientSource::Factory => {
            vec![home.join(".factory/config.json")]
        }
        ClientSource::Nanobot => {
            vec![
                home.join(".nanobot/config.json"),
                PathBuf::from(".nanobot/config.json"),
            ]
        }
        ClientSource::Junie => {
            vec![
                home.join(".junie/mcp/mcp.json"),
                PathBuf::from(".junie/mcp/mcp.json"),
            ]
        }
        ClientSource::Kilo => {
            vec![
                home.join(".config/kilo/opencode.json"),
                PathBuf::from("opencode.json"),
            ]
        }
        ClientSource::Antigravity => antigravity_paths(&home),
        ClientSource::Goose => goose_paths(&home),
    }
}

/// Platform-specific config paths for Goose.
#[allow(unused_variables)]
fn goose_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        paths.push(home.join(".config/goose/config.yaml"));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(home.join(".config/goose/config.yaml"));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("Block/goose/config/config.yaml"));
    }
    paths
}

/// Platform-specific config paths for Claude Desktop.
/// Extracted to its own function to satisfy clippy's `vec_init_then_push` lint
/// when using `#[cfg]` attributes on individual push calls.
#[allow(unused_variables)]
fn claude_desktop_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        paths.push(home.join("Library/Application Support/Claude/claude_desktop_config.json"));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("Claude/claude_desktop_config.json"));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(home.join(".config/claude/claude_desktop_config.json"));
    }
    paths
}

/// Platform-specific config paths for Google Antigravity.
#[allow(unused_variables)]
fn antigravity_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        paths.push(home.join("Library/Application Support/Antigravity/antigravity_config.json"));
        paths.push(home.join("Library/Application Support/Google/Antigravity/config.json"));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("Antigravity/antigravity_config.json"));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(home.join(".config/Antigravity/antigravity_config.json"));
    }
    paths
}

// ── Parsers ─────────────────────────────────────────────────────────────────

/// Parse a client config file and extract server definitions.
fn parse_config(source: ClientSource, path: &Path) -> Result<Vec<DiscoveredServer>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;

    match source {
        // TOML clients: cleanse "plug" entries first to avoid parse errors from duplicates
        ClientSource::CodexCli => {
            let cleansed = unlink_toml(&content);
            parse_toml_mcp_servers(&cleansed, source, "mcp_servers")
        }

        // JSON clients with "mcpServers" key
        ClientSource::ClaudeDesktop
        | ClientSource::ClaudeCode
        | ClientSource::Cursor
        | ClientSource::Windsurf
        | ClientSource::GeminiCli
        | ClientSource::Cline
        | ClientSource::ClineCli
        | ClientSource::RooCode
        | ClientSource::Factory
        | ClientSource::OpenCode
        | ClientSource::Junie
        | ClientSource::Kilo
        | ClientSource::Antigravity => parse_json_mcp_servers(&content, source, "mcpServers"),

        // Nanobot uses tools.mcpServers
        ClientSource::Nanobot => parse_nanobot_config(&content, source),

        // VS Code uses nested "mcp.servers" (or "servers" under "mcp" key)
        ClientSource::VSCodeCopilot => parse_vscode_config(&content, source),

        // Zed uses "context_servers"
        ClientSource::Zed => parse_json_mcp_servers(&content, source, "context_servers"),

        // YAML clients
        ClientSource::Goose => parse_yaml_mcp_extensions(&content, source, "extensions"),
    }
}

/// Parse YAML config with a top-level key containing server definitions.
fn parse_yaml_mcp_extensions(
    content: &str,
    source: ClientSource,
    key: &str,
) -> Result<Vec<DiscoveredServer>, String> {
    let value: serde_yml::Value =
        serde_yml::from_str(content).map_err(|e| format!("YAML parse error: {e}"))?;

    let servers_obj = match value.get(key).and_then(|v| v.as_mapping()) {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let mut servers = Vec::new();
    for (name_val, entry) in servers_obj {
        let name = match name_val.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        if name == "plug" {
            continue;
        }
        if let Some(config) = yaml_entry_to_server_config(entry) {
            servers.push(DiscoveredServer {
                name,
                config,
                source,
            });
        }
    }

    Ok(servers)
}

/// Convert a YAML entry to a ServerConfig.
fn yaml_entry_to_server_config(entry: &serde_yml::Value) -> Option<ServerConfig> {
    let obj = entry.as_mapping()?;

    // Goose uses "type: stdio" or "sse"
    let transport_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("stdio");
    let transport = match transport_type {
        "stdio" => crate::config::TransportType::Stdio,
        "sse" => crate::config::TransportType::Sse,
        "http" => crate::config::TransportType::Http,
        _ => return None,
    };

    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .map(String::from);
    let args = obj
        .get("args")
        .and_then(|v| v.as_sequence())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let url = obj
        .get("uri")
        .or_else(|| obj.get("url"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let env = obj
        .get("env")
        .and_then(|v| v.as_mapping())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let key = k.as_str()?.to_string();
                    let val = v.as_str()?.to_string();
                    Some((key, val))
                })
                .collect()
        })
        .unwrap_or_default();

    let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    Some(ServerConfig {
        command,
        args,
        env,
        enabled,
        transport,
        url,
        auth_token: None,
        timeout_secs: 30,
        call_timeout_secs: 300,
        max_concurrent: 1,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: true,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    })
}

/// Parse JSON config with a top-level key containing server definitions.
fn parse_json_mcp_servers(
    content: &str,
    source: ClientSource,
    key: &str,
) -> Result<Vec<DiscoveredServer>, String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("JSON parse error: {e}"))?;

    let servers_obj = match value.get(key).and_then(|v| v.as_object()) {
        Some(obj) => obj,
        None => return Ok(Vec::new()),
    };

    let mut servers = Vec::new();
    for (name, entry) in servers_obj {
        if name == "plug" {
            continue;
        }
        if let Some(config) = json_entry_to_server_config(entry) {
            servers.push(DiscoveredServer {
                name: name.clone(),
                config,
                source,
            });
        }
    }
    Ok(servers)
}

/// Parse Nanobot config which nests servers under "tools" -> "mcpServers".
fn parse_nanobot_config(
    content: &str,
    source: ClientSource,
) -> Result<Vec<DiscoveredServer>, String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("JSON parse error: {e}"))?;

    let servers = value
        .get("tools")
        .and_then(|t| t.get("mcpServers"))
        .and_then(|s| s.as_object());

    if let Some(obj) = servers {
        let mut result = Vec::new();
        for (name, entry) in obj {
            if name == "plug" {
                continue;
            }
            if let Some(config) = json_entry_to_server_config(entry) {
                result.push(DiscoveredServer {
                    name: name.clone(),
                    config,
                    source,
                });
            }
        }
        return Ok(result);
    }

    Ok(Vec::new())
}

/// Parse VS Code config which nests servers under "mcp" -> "servers".
fn parse_vscode_config(
    content: &str,
    source: ClientSource,
) -> Result<Vec<DiscoveredServer>, String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("JSON parse error: {e}"))?;

    // Try "mcp" -> "servers" first (settings.json style)
    if let Some(servers) = value
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(|s| s.as_object())
    {
        let mut result = Vec::new();
        for (name, entry) in servers {
            if name == "plug" {
                continue;
            }
            if let Some(config) = json_entry_to_server_config(entry) {
                result.push(DiscoveredServer {
                    name: name.clone(),
                    config,
                    source,
                });
            }
        }
        return Ok(result);
    }

    // Fallback: try top-level "servers"
    if let Some(servers) = value.get("servers").and_then(|s| s.as_object()) {
        let mut result = Vec::new();
        for (name, entry) in servers {
            if name == "plug" {
                continue;
            }
            if let Some(config) = json_entry_to_server_config(entry) {
                result.push(DiscoveredServer {
                    name: name.clone(),
                    config,
                    source,
                });
            }
        }
        return Ok(result);
    }

    Ok(Vec::new())
}

/// Convert a JSON server entry to our ServerConfig.
fn json_entry_to_server_config(entry: &serde_json::Value) -> Option<ServerConfig> {
    let obj = entry.as_object()?;

    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .map(String::from);
    let args: Vec<String> = obj
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let env: HashMap<String, String> = obj
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let val = v.as_str()?;
                    // Security: never store literal secrets — convert to env var reference
                    let safe_val = sanitize_env_value(k, val);
                    Some((k.clone(), safe_val))
                })
                .collect()
        })
        .unwrap_or_default();

    // Determine transport: url present = HTTP, command present = stdio
    let url = obj
        .get("url")
        .or_else(|| obj.get("httpUrl"))
        .or_else(|| obj.get("sseUrl"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let (transport, command, url) = if let Some(url) = url {
        (TransportType::Http, None, Some(url))
    } else if command.is_some() {
        (TransportType::Stdio, command, None)
    } else {
        return None; // Neither command nor URL — skip
    };

    Some(ServerConfig {
        command,
        args,
        env,
        enabled: true,
        transport,
        url,
        auth_token: None,
        timeout_secs: 30,
        call_timeout_secs: 300,
        max_concurrent: 1,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: true,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    })
}

/// Parse TOML config with a section key containing server definitions.
fn parse_toml_mcp_servers(
    content: &str,
    source: ClientSource,
    key: &str,
) -> Result<Vec<DiscoveredServer>, String> {
    let value: toml::Value = content
        .parse::<toml::Value>()
        .map_err(|e| format!("TOML parse error: {e}"))?;

    let servers_table = match value.get(key).and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut servers = Vec::new();
    for (name, entry) in servers_table {
        if name == "plug" {
            continue;
        }
        if let Some(config) = toml_entry_to_server_config(entry) {
            servers.push(DiscoveredServer {
                name: name.clone(),
                config,
                source,
            });
        }
    }
    Ok(servers)
}

/// Convert a TOML server entry to our ServerConfig.
fn toml_entry_to_server_config(entry: &toml::Value) -> Option<ServerConfig> {
    let table = entry.as_table()?;

    let command = table
        .get("command")
        .and_then(|v| v.as_str())
        .map(String::from);
    let args: Vec<String> = table
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let env: HashMap<String, String> = table
        .get("env")
        .and_then(|v| v.as_table())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| {
                    let val = v.as_str()?;
                    let safe_val = sanitize_env_value(k, val);
                    Some((k.clone(), safe_val))
                })
                .collect()
        })
        .unwrap_or_default();

    let url = table
        .get("url")
        .or_else(|| table.get("http_url"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let (transport, command, url) = if let Some(url) = url {
        (TransportType::Http, None, Some(url))
    } else if command.is_some() {
        (TransportType::Stdio, command, None)
    } else {
        return None;
    };

    Some(ServerConfig {
        command,
        args,
        env,
        enabled: true,
        transport,
        url,
        auth_token: None,
        timeout_secs: 30,
        call_timeout_secs: 300,
        max_concurrent: 1,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: true,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    })
}

// ── Security ────────────────────────────────────────────────────────────────

/// Sanitize environment variable values: if a value looks like a literal secret,
/// convert it to an env var reference ($VAR_NAME).
fn sanitize_env_value(key: &str, value: &str) -> String {
    // If it already looks like a var reference, keep it
    if value.starts_with('$') {
        return value.to_string();
    }
    // Heuristic: if the key suggests it's a secret and the value looks like one,
    // replace with env var reference
    let secret_keys = [
        "token", "key", "secret", "password", "api_key", "apikey", "auth",
    ];
    let key_lower = key.to_lowercase();
    if secret_keys.iter().any(|k| key_lower.contains(k)) && value.len() > 8 {
        format!("${key}")
    } else {
        value.to_string()
    }
}

// ── Deduplication ───────────────────────────────────────────────────────────

/// Dedup key: (command, args) for stdio, (url) for HTTP.
fn server_signature(config: &ServerConfig) -> String {
    match config.transport {
        TransportType::Stdio => {
            let cmd = config.command.as_deref().unwrap_or("");
            let args = config.args.join(" ");
            format!("stdio:{cmd} {args}")
        }
        TransportType::Http => {
            let url = config.url.as_deref().unwrap_or("");
            format!("http:{url}")
        }
        TransportType::Sse => {
            let url = config.url.as_deref().unwrap_or("");
            format!("sse:{url}")
        }
    }
}

/// Deduplicate servers by their signature. First occurrence wins.
fn dedup_servers(servers: &mut Vec<DiscoveredServer>) {
    let mut seen = std::collections::HashSet::new();
    servers.retain(|s| {
        let sig = server_signature(&s.config);
        seen.insert(sig)
    });
}

/// Check if a discovered server already exists in the config.
fn is_existing_server(
    discovered: &DiscoveredServer,
    existing: &HashMap<String, ServerConfig>,
) -> bool {
    let sig = server_signature(&discovered.config);
    existing
        .values()
        .any(|existing_cfg| server_signature(existing_cfg) == sig)
}

/// Resolve name collisions: if name already exists, append source suffix.
pub fn resolve_name(name: &str, source: ClientSource, existing_names: &[String]) -> String {
    if !existing_names.contains(&name.to_string()) {
        return name.to_string();
    }
    let suffix = match source {
        ClientSource::ClaudeDesktop => "claude-desktop",
        ClientSource::ClaudeCode => "claude-code",
        ClientSource::Cursor => "cursor",
        ClientSource::Windsurf => "windsurf",
        ClientSource::VSCodeCopilot => "vscode",
        ClientSource::GeminiCli => "gemini",
        ClientSource::CodexCli => "codex",
        ClientSource::OpenCode => "opencode",
        ClientSource::Zed => "zed",
        ClientSource::Cline => "cline",
        ClientSource::ClineCli => "cline-cli",
        ClientSource::RooCode => "roocode",
        ClientSource::Factory => "factory",
        ClientSource::Nanobot => "nanobot",
        ClientSource::Junie => "junie",
        ClientSource::Kilo => "kilo",
        ClientSource::Antigravity => "antigravity",
        ClientSource::Goose => "goose",
    };
    format!("{name}-{suffix}")
}

/// Remove any "plug" MCP server section from a TOML string.
/// This helps avoid parse errors if multiple plug sections were accidentally appended.
pub fn unlink_toml(content: &str) -> String {
    let mut output = Vec::new();
    let mut skipping = false;
    for line in content.lines() {
        let trimmed = line.trim();

        // Start skipping if we hit any variation of the plug header
        if trimmed == "[mcp_servers.plug]"
            || trimmed == "[[mcp_servers.plug]]"
            || trimmed == "[mcp_servers.\"plug\"]"
        {
            skipping = true;
            continue;
        }

        if skipping {
            // Stop skipping ONLY if we hit a new header that is NOT a plug header
            if trimmed.starts_with('[')
                && !trimmed.contains(".plug")
                && !trimmed.contains("\"plug\"")
            {
                skipping = false;
            } else {
                // Still skipping: either it's more plug config or another duplicate plug header
                continue;
            }
        }
        output.push(line);
    }
    output.join("\n")
}

/// Generate TOML entries for new servers to append to config.toml.
pub fn servers_to_toml(servers: &[DiscoveredServer], existing_names: &[String]) -> String {
    let mut output = String::new();
    let mut used_names: Vec<String> = existing_names.to_vec();

    for server in servers {
        let name = resolve_name(&server.name, server.source, &used_names);
        used_names.push(name.clone());

        output.push_str(&format!("\n[servers.{name}]\n"));

        match server.config.transport {
            TransportType::Stdio => {
                if let Some(ref cmd) = server.config.command {
                    output.push_str(&format!("command = {}\n", toml_quote(cmd)));
                }
                if !server.config.args.is_empty() {
                    let args: Vec<String> =
                        server.config.args.iter().map(|a| toml_quote(a)).collect();
                    output.push_str(&format!("args = [{}]\n", args.join(", ")));
                }
            }
            TransportType::Http => {
                if let Some(ref url) = server.config.url {
                    output.push_str(&format!(
                        "transport = \"http\"\nurl = {}\n",
                        toml_quote(url)
                    ));
                }
            }
            TransportType::Sse => {
                if let Some(ref url) = server.config.url {
                    output.push_str(&format!("transport = \"sse\"\nurl = {}\n", toml_quote(url)));
                }
            }
        }

        if !server.config.env.is_empty() {
            output.push_str("[servers.");
            output.push_str(&name);
            output.push_str(".env]\n");
            for (k, v) in &server.config.env {
                output.push_str(&format!("{k} = {}\n", toml_quote(v)));
            }
        }
    }

    output
}

fn toml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_desktop_json() {
        let json = r#"{
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["@modelcontextprotocol/server-github"],
                    "env": { "GITHUB_TOKEN": "ghp_abc123" }
                },
                "filesystem": {
                    "command": "npx",
                    "args": ["@modelcontextprotocol/server-filesystem", "/home"]
                }
            }
        }"#;
        let servers =
            parse_json_mcp_servers(json, ClientSource::ClaudeDesktop, "mcpServers").unwrap();
        assert_eq!(servers.len(), 2);

        let github = servers.iter().find(|s| s.name == "github").unwrap();
        assert_eq!(github.config.command.as_deref(), Some("npx"));
        assert_eq!(github.config.args.len(), 1);
        // Secret should be sanitized to env var reference
        assert_eq!(
            github.config.env.get("GITHUB_TOKEN").unwrap(),
            "$GITHUB_TOKEN"
        );
    }

    #[test]
    fn parse_vscode_nested_config() {
        let json = r#"{
            "mcp": {
                "servers": {
                    "myserver": {
                        "command": "node",
                        "args": ["server.js"]
                    }
                }
            }
        }"#;
        let servers = parse_vscode_config(json, ClientSource::VSCodeCopilot).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "myserver");
    }

    #[test]
    fn parse_zed_context_servers() {
        let json = r#"{
            "context_servers": {
                "zed-server": {
                    "command": "zed-mcp",
                    "args": []
                }
            }
        }"#;
        let servers = parse_json_mcp_servers(json, ClientSource::Zed, "context_servers").unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "zed-server");
    }

    #[test]
    fn parse_codex_toml() {
        let toml = r#"
[mcp_servers.github]
command = "npx"
args = ["@modelcontextprotocol/server-github"]

[mcp_servers.github.env]
GITHUB_TOKEN = "$GITHUB_TOKEN"
"#;
        let servers = parse_toml_mcp_servers(toml, ClientSource::CodexCli, "mcp_servers").unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].config.env.get("GITHUB_TOKEN").unwrap(),
            "$GITHUB_TOKEN"
        );
    }

    #[test]
    fn parse_http_transport_json() {
        let json = r#"{
            "mcpServers": {
                "remote": {
                    "url": "http://localhost:8080/mcp"
                }
            }
        }"#;
        let servers = parse_json_mcp_servers(json, ClientSource::Cursor, "mcpServers").unwrap();
        assert_eq!(servers.len(), 1);
        assert!(matches!(servers[0].config.transport, TransportType::Http));
        assert_eq!(
            servers[0].config.url.as_deref(),
            Some("http://localhost:8080/mcp")
        );
    }

    #[test]
    fn dedup_removes_duplicates() {
        let mut servers = vec![
            DiscoveredServer {
                name: "github".into(),
                config: ServerConfig {
                    command: Some("npx".into()),
                    args: vec!["server-github".into()],
                    ..test_config()
                },
                source: ClientSource::ClaudeDesktop,
            },
            DiscoveredServer {
                name: "github".into(),
                config: ServerConfig {
                    command: Some("npx".into()),
                    args: vec!["server-github".into()],
                    ..test_config()
                },
                source: ClientSource::Cursor,
            },
        ];
        dedup_servers(&mut servers);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].source, ClientSource::ClaudeDesktop); // first wins
    }

    #[test]
    fn sanitize_env_converts_secrets() {
        assert_eq!(
            sanitize_env_value("GITHUB_TOKEN", "ghp_abc123def456"),
            "$GITHUB_TOKEN"
        );
        assert_eq!(sanitize_env_value("PATH", "/usr/bin"), "/usr/bin");
        assert_eq!(sanitize_env_value("API_KEY", "$API_KEY"), "$API_KEY"); // already a reference
    }

    #[test]
    fn resolve_name_handles_collisions() {
        let existing = vec!["github".to_string()];
        assert_eq!(
            resolve_name("github", ClientSource::Cursor, &existing),
            "github-cursor"
        );
        assert_eq!(
            resolve_name("slack", ClientSource::Cursor, &existing),
            "slack"
        );
    }

    #[test]
    fn import_filters_existing() {
        let mut existing = HashMap::new();
        existing.insert(
            "github".to_string(),
            ServerConfig {
                command: Some("npx".into()),
                args: vec!["server-github".into()],
                ..test_config()
            },
        );

        // This will scan real filesystem, so we test the filtering logic directly
        let discovered = vec![DiscoveredServer {
            name: "github".into(),
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec!["server-github".into()],
                ..test_config()
            },
            source: ClientSource::ClaudeDesktop,
        }];

        let filtered: Vec<_> = discovered
            .into_iter()
            .filter(|d| !is_existing_server(d, &existing))
            .collect();
        assert!(filtered.is_empty());
    }

    #[test]
    fn servers_to_toml_output() {
        let servers = vec![DiscoveredServer {
            name: "github".into(),
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec!["@modelcontextprotocol/server-github".into()],
                ..test_config()
            },
            source: ClientSource::ClaudeDesktop,
        }];
        let toml = servers_to_toml(&servers, &[]);
        assert!(toml.contains("[servers.github]"));
        assert!(toml.contains("command = \"npx\""));
        assert!(toml.contains("@modelcontextprotocol/server-github"));
    }

    fn test_config() -> ServerConfig {
        ServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        }
    }
}
