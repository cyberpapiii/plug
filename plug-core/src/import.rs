//! Config import: read other AI clients' settings files for MCP servers.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::clients::{ClientTarget, ServerEntry};
use crate::config::{Config, ServerConfig, TransportType};

/// A server discovered from a client config file.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredServer {
    /// Server name as defined in the source config.
    pub name: String,
    pub config: ServerConfig,
    #[serde(skip)]
    pub source: ClientTarget,
    /// `source`'s slug, as `plug link` takes it.
    pub source_target: &'static str,
    pub source_name: &'static str,
}

/// Result of scanning a single client.
#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    #[serde(skip)]
    pub source: ClientTarget,
    pub source_target: &'static str,
    pub source_name: &'static str,
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

pub fn scan_client(source: ClientTarget) -> ScanResult {
    let mut servers = Vec::new();
    let mut error = None;

    for path in source.import_paths() {
        if !path.exists() {
            continue;
        }
        let found = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|content| source.layout().read_servers(&content));
        match found {
            Ok(found) => servers.extend(
                found
                    .into_iter()
                    .map(|(name, entry)| discovered(source, name, entry)),
            ),
            Err(e) => error = Some(format!("{}: {e}", path.display())),
        }
    }

    ScanResult {
        source,
        source_target: source.slug(),
        source_name: source.display_name(),
        servers,
        error,
    }
}

/// Scan, deduplicate, and keep what `existing` does not already have.
pub fn import(existing: &HashMap<String, ServerConfig>, sources: &[ClientTarget]) -> ImportReport {
    report(existing, sources.iter().map(|s| scan_client(*s)).collect())
}

fn report(existing: &HashMap<String, ServerConfig>, scanned: Vec<ScanResult>) -> ImportReport {
    let mut all_discovered: Vec<DiscoveredServer> =
        scanned.iter().flat_map(|r| r.servers.clone()).collect();

    let duplicates_before = all_discovered.len();
    let mut seen = HashSet::new();
    all_discovered.retain(|s| seen.insert(server_signature(&s.config)));
    let duplicates_merged = duplicates_before - all_discovered.len();

    let existing_sigs: HashSet<String> = existing.values().map(server_signature).collect();
    let new_servers: Vec<DiscoveredServer> = all_discovered
        .into_iter()
        .filter(|d| !existing_sigs.contains(&server_signature(&d.config)))
        .collect();

    let skipped = duplicates_before - duplicates_merged - new_servers.len();

    ImportReport {
        scanned,
        new_servers,
        skipped,
        duplicates_merged,
    }
}

/// Add `servers` to `config`. A name already taken gets the client's slug
/// after it, and a number after that if it is taken too. Returns the names
/// they were added under.
pub fn add_to_config(config: &mut Config, servers: &[DiscoveredServer]) -> Vec<String> {
    servers
        .iter()
        .map(|server| {
            let renamed = format!("{}-{}", server.name, server.source.slug());
            let name = std::iter::once(server.name.clone())
                .chain(std::iter::once(renamed.clone()))
                .chain((2..).map(|n| format!("{renamed}-{n}")))
                .find(|name| !config.servers.contains_key(name))
                .expect("an unused name");
            config.servers.insert(name.clone(), server.config.clone());
            name
        })
        .collect()
}

fn discovered(source: ClientTarget, name: String, entry: ServerEntry) -> DiscoveredServer {
    DiscoveredServer {
        name,
        config: ServerConfig {
            command: entry.command,
            args: entry.args.unwrap_or_default(),
            env: entry.env.into_iter().collect(),
            enabled: entry.enabled,
            transport: entry.transport,
            url: entry.url,
            ..Default::default()
        },
        source,
        source_target: source.slug(),
        source_name: source.display_name(),
    }
}

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
        TransportType::OpenApi => {
            let spec = config.spec.as_deref().unwrap_or("");
            format!("openapi:{spec}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(target: ClientTarget, content: &str) -> Vec<DiscoveredServer> {
        target
            .layout()
            .read_servers(content)
            .unwrap()
            .into_iter()
            .map(|(name, entry)| discovered(target, name, entry))
            .collect()
    }

    fn scanned(servers: Vec<DiscoveredServer>) -> ScanResult {
        ScanResult {
            source: ClientTarget::Cursor,
            source_target: "cursor",
            source_name: "Cursor",
            servers,
            error: None,
        }
    }

    #[test]
    fn a_literal_secret_is_kept_for_the_secret_store() {
        let json = r#"{"mcpServers":{"github":{"command":"npx","args":["server-github"],"env":{"GITHUB_TOKEN":"ghp_abc123"}}}}"#;
        let servers = read(ClientTarget::ClaudeDesktop, json);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].config.env["GITHUB_TOKEN"], "ghp_abc123");
        assert_eq!(servers[0].source_target, "claude-desktop");
        assert_eq!(servers[0].source_name, "Claude Desktop");
    }

    #[test]
    fn vscode_settings_from_before_mcp_json_are_still_read() {
        let json = r#"{"mcp":{"servers":{"myserver":{"command":"node","args":["server.js"]}}}}"#;
        let servers = read(ClientTarget::VSCodeCopilot, json);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "myserver");
    }

    #[test]
    fn hermes_yaml_skips_plug_whatever_its_case() {
        let yaml = "mcp_servers:\n  Plug:\n    command: plug\n    args: [connect]\n  files:\n    command: npx\n    args: [\"-y\", \"server-filesystem\"]\n  company:\n    url: https://mcp.example.com\n";
        let servers = read(ClientTarget::Hermes, yaml);
        assert_eq!(servers.len(), 2);
        let company = servers.iter().find(|s| s.name == "company").unwrap();
        assert_eq!(company.config.transport, TransportType::Http);
    }

    #[test]
    fn goose_sse_extensions_stay_sse() {
        let yaml = "extensions:\n  github:\n    type: stdio\n    command: npx\n    env:\n      GITHUB_TOKEN: \"$GITHUB_TOKEN\"\n    enabled: true\n  remote:\n    type: sse\n    uri: https://remote.example.com/sse\n    enabled: false\n";
        let servers = read(ClientTarget::Goose, yaml);
        let remote = servers.iter().find(|s| s.name == "remote").unwrap();
        assert_eq!(remote.config.transport, TransportType::Sse);
        assert!(!remote.config.enabled);
    }

    #[test]
    fn a_json_sse_entry_is_imported_as_sse() {
        let json = r#"{"mcpServers":{"old":{"type":"sse","url":"https://example.com/sse"},"new":{"url":"https://example.com/mcp"}}}"#;
        let servers = read(ClientTarget::ClaudeCode, json);
        let kind = |name: &str| {
            servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .config
                .transport
                .clone()
        };
        assert_eq!(kind("old"), TransportType::Sse);
        assert_eq!(kind("new"), TransportType::Http);
    }

    #[test]
    fn opencode_and_kilo_servers_are_read_from_mcp() {
        let json = r#"{"mcp":{"files":{"type":"local","command":["npx","-y","server-filesystem"],"enabled":true}}}"#;
        for target in [ClientTarget::OpenCode, ClientTarget::Kilo] {
            let servers = read(target, json);
            assert_eq!(servers.len(), 1, "{target:?}");
            assert_eq!(servers[0].config.command.as_deref(), Some("npx"));
            assert_eq!(servers[0].config.args, ["-y", "server-filesystem"]);
        }
    }

    #[test]
    fn a_duplicated_plug_table_does_not_hide_codex_servers() {
        let toml = "[mcp_servers.plug]\ncommand = \"plug\"\n\n[mcp_servers.plug]\ncommand = \"plug\"\n\n[mcp_servers.github]\ncommand = \"npx\"\n\n[mcp_servers.github.env]\nGITHUB_TOKEN = \"$GITHUB_TOKEN\"\n";
        let servers = read(ClientTarget::CodexCli, toml);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].config.env["GITHUB_TOKEN"], "$GITHUB_TOKEN");
    }

    #[test]
    fn duplicates_merge_and_known_servers_are_skipped() {
        let github = |source| DiscoveredServer {
            source,
            ..read(
                ClientTarget::Cursor,
                r#"{"mcpServers":{"github":{"command":"npx","args":["server-github"]}}}"#,
            )
            .remove(0)
        };
        let fresh = report(
            &HashMap::new(),
            vec![scanned(vec![
                github(ClientTarget::ClaudeDesktop),
                github(ClientTarget::Cursor),
            ])],
        );
        assert_eq!(fresh.duplicates_merged, 1);
        assert_eq!(fresh.new_servers.len(), 1);
        assert_eq!(fresh.new_servers[0].source, ClientTarget::ClaudeDesktop);

        let existing = HashMap::from([("github".to_string(), fresh.new_servers[0].config.clone())]);
        let known = report(&existing, vec![scanned(vec![github(ClientTarget::Cursor)])]);
        assert!(known.new_servers.is_empty());
        assert_eq!(known.skipped, 1);
    }

    #[test]
    fn a_name_already_taken_gets_the_clients_slug() {
        let found = read(
            ClientTarget::QwenCode,
            r#"{"mcpServers":{"github":{"command":"npx"},"my.server":{"url":"https://example.com/mcp"}}}"#,
        );
        let mut config = Config::default();
        config
            .servers
            .insert("github".to_string(), ServerConfig::default());
        let mut names = add_to_config(&mut config, &found);
        names.sort();
        assert_eq!(names, ["github-qwen-code", "my.server"]);
        // The file Plug writes reads back with every server.
        let written = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&written).unwrap();
        assert!(back.servers.contains_key("my.server"));
        assert!(back.servers.contains_key("github-qwen-code"));
    }

    #[test]
    fn import_json_names_each_source_by_slug_and_name() {
        let source = ClientTarget::KimiCode;
        let scanned = ScanResult {
            source,
            source_target: source.slug(),
            source_name: source.display_name(),
            servers: Vec::new(),
            error: None,
        };
        let json = serde_json::to_value(report(&HashMap::new(), vec![scanned])).unwrap();
        assert_eq!(json["scanned"][0]["source_target"], "kimi-code");
        assert_eq!(json["scanned"][0]["source_name"], "Kimi Code");
        assert!(json["scanned"][0].get("source").is_none());
    }

    #[test]
    fn a_renamed_server_never_replaces_one_already_there() {
        let found = read(
            ClientTarget::Cursor,
            r#"{"mcpServers":{"github":{"command":"npx"}}}"#,
        );
        let mut config = Config::default();
        for taken in ["github", "github-cursor", "github-cursor-2"] {
            config
                .servers
                .insert(taken.to_string(), ServerConfig::default());
        }
        assert_eq!(add_to_config(&mut config, &found), ["github-cursor-3"]);
        assert_eq!(config.servers.len(), 4);
    }
}
