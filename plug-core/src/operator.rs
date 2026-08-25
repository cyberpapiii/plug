//! Daemon-owned operator mutations and atomic configuration persistence.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ServerConfig, TransportType, validate_config};

#[derive(Debug, Clone)]
pub enum OperatorMutation {
    AddServer { name: String, server: ServerConfig },
    UpdateServer { name: String, server: ServerConfig },
    RemoveServer { name: String },
    SetServerEnabled { name: String, enabled: bool },
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
            OperatorMutationResult {
                server: Some(summary),
            }
        }
        OperatorMutation::UpdateServer { name, server } => {
            if !config.servers.contains_key(&name) {
                anyhow::bail!("unknown server `{name}`");
            }
            let summary = OperatorServerSummary::from_config(name.clone(), &server);
            config.servers.insert(name, server);
            OperatorMutationResult {
                server: Some(summary),
            }
        }
        OperatorMutation::RemoveServer { name } => {
            if config.servers.remove(&name).is_none() {
                anyhow::bail!("unknown server `{name}`");
            }
            OperatorMutationResult { server: None }
        }
        OperatorMutation::SetServerEnabled { name, enabled } => {
            let server = config
                .servers
                .get_mut(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;
            server.enabled = enabled;
            OperatorMutationResult {
                server: Some(OperatorServerSummary::from_config(name, server)),
            }
        }
    };
    persist_config_atomic(path, &config)?;
    Ok((config, result))
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
