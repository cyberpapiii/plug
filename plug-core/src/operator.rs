//! Daemon-owned operator mutations and atomic configuration persistence.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ServerConfig, TransportType, validate_config};
use crate::proxy::is_disabled_tool;

#[derive(Debug, Clone)]
pub enum OperatorMutation {
    AddServer { name: String, server: ServerConfig },
    UpdateServer { name: String, server: ServerConfig },
    RemoveServer { name: String },
    SetServerEnabled { name: String, enabled: bool },
    SetToolEnabled { tool: String, enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorServerSummary {
    pub name: String,
    pub enabled: bool,
    pub transport: TransportType,
    pub oauth: bool,
}

impl OperatorServerSummary {
    pub fn from_config(name: String, server: &ServerConfig) -> Self {
        Self {
            name,
            enabled: server.enabled,
            transport: server.transport.clone(),
            oauth: server.auth.as_deref() == Some("oauth"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorMutationResult {
    pub server: Option<OperatorServerSummary>,
    /// Disabled-tool patterns after the mutation, for `SetToolEnabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_tools: Option<Vec<String>>,
}

impl OperatorMutationResult {
    fn server(summary: Option<OperatorServerSummary>) -> Self {
        Self {
            server: summary,
            disabled_tools: None,
        }
    }
}

#[allow(clippy::result_large_err)]
pub fn load_editable_config(path: &Path) -> Result<Config, figment::Error> {
    if !path.exists() {
        return Ok(Config::default());
    }
    Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(path))
        .extract()
}

pub fn apply_operator_mutation(
    path: &Path,
    mutation: OperatorMutation,
) -> anyhow::Result<(Config, OperatorMutationResult)> {
    let mut config = load_editable_config(path)?;
    let result = match mutation {
        OperatorMutation::AddServer { name, server } => {
            if config.servers.contains_key(&name) {
                anyhow::bail!("server `{name}` already exists");
            }
            let summary = OperatorServerSummary::from_config(name.clone(), &server);
            config.servers.insert(name, server);
            OperatorMutationResult::server(Some(summary))
        }
        OperatorMutation::UpdateServer { name, server } => {
            if !config.servers.contains_key(&name) {
                anyhow::bail!("unknown server `{name}`");
            }
            let summary = OperatorServerSummary::from_config(name.clone(), &server);
            config.servers.insert(name, server);
            OperatorMutationResult::server(Some(summary))
        }
        OperatorMutation::RemoveServer { name } => {
            if config.servers.remove(&name).is_none() {
                anyhow::bail!("unknown server `{name}`");
            }
            OperatorMutationResult::server(None)
        }
        OperatorMutation::SetServerEnabled { name, enabled } => {
            let server = config
                .servers
                .get_mut(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;
            server.enabled = enabled;
            OperatorMutationResult::server(Some(OperatorServerSummary::from_config(name, server)))
        }
        OperatorMutation::SetToolEnabled { tool, enabled } => {
            set_tool_enabled(&mut config, &tool, enabled)?;
            OperatorMutationResult {
                server: None,
                disabled_tools: Some(config.disabled_tools.clone()),
            }
        }
    };
    persist_config_atomic(path, &config)?;
    Ok((config, result))
}

/// Turn one tool on or off by editing `disabled_tools`.
///
/// Disabling appends the exact merged tool name. Enabling drops every exact
/// entry for it, then fails if a surviving wildcard still covers the tool:
/// the config format cannot express "this pattern except one tool", so
/// silently widening the pattern would switch on tools nobody asked for.
fn set_tool_enabled(config: &mut Config, tool: &str, enabled: bool) -> anyhow::Result<()> {
    if tool.trim().is_empty() {
        anyhow::bail!("tool name is required");
    }
    if enabled {
        config
            .disabled_tools
            .retain(|pattern| !pattern.eq_ignore_ascii_case(tool));
        if let Some(pattern) = config
            .disabled_tools
            .iter()
            .find(|pattern| is_disabled_tool(std::slice::from_ref(*pattern), tool))
        {
            anyhow::bail!(
                "`{tool}` stays off because the pattern `{pattern}` covers it; remove that pattern to turn it back on"
            );
        }
    } else if !config
        .disabled_tools
        .iter()
        .any(|pattern| pattern.eq_ignore_ascii_case(tool))
    {
        config.disabled_tools.push(tool.to_string());
        config.disabled_tools.sort();
    }
    Ok(())
}

pub fn persist_config_atomic(path: &Path, config: &Config) -> anyhow::Result<()> {
    let errors = validate_config(config);
    if !errors.is_empty() {
        anyhow::bail!(errors.join("; "));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = PathBuf::from(format!(
        "{}.{}.tmp",
        path.display(),
        uuid::Uuid::new_v4().simple()
    ));
    let contents = toml::to_string_pretty(config)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temp, contents)?;

    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> PathBuf {
        tempfile::tempdir().unwrap().keep().join("config.toml")
    }

    #[test]
    fn tool_toggle_round_trips_through_disabled_tools() {
        let path = fixture_path();
        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: false,
            },
        )
        .unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__get_file".to_string()]);
        assert_eq!(
            result.disabled_tools,
            Some(vec!["figma__get_file".to_string()])
        );

        let (config, _) = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: true,
            },
        )
        .unwrap();
        assert!(config.disabled_tools.is_empty());
    }

    #[test]
    fn disabling_a_tool_twice_does_not_duplicate_the_pattern() {
        let path = fixture_path();
        for _ in 0..2 {
            apply_operator_mutation(
                &path,
                OperatorMutation::SetToolEnabled {
                    tool: "figma__get_file".into(),
                    enabled: false,
                },
            )
            .unwrap();
        }
        let config = load_editable_config(&path).unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__get_file".to_string()]);
    }

    #[test]
    fn enabling_one_tool_refuses_to_widen_a_covering_wildcard() {
        let path = fixture_path();
        let config = Config {
            disabled_tools: vec!["figma__*".to_string()],
            ..Default::default()
        };
        persist_config_atomic(&path, &config).unwrap();

        let error = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("figma__*"), "{error}");

        // The refusal must leave the pattern in place rather than half-applying.
        let config = load_editable_config(&path).unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__*".to_string()]);
    }

    #[test]
    fn validate_server_does_not_write_and_mutation_is_atomic_owner_only() {
        let path = fixture_path();
        let server: ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "echo"
        }))
        .unwrap();
        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: "search".into(),
                server,
            },
        )
        .unwrap();
        assert!(config.servers.contains_key("search"));
        assert_eq!(result.server.unwrap().name, "search");
        assert!(!PathBuf::from(format!("{}.tmp", path.display())).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
