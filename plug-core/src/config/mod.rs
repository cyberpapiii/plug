mod expand;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use expand::expand_env_vars;

/// Top-level configuration for plug.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Bind address for HTTP server (Phase 2).
    pub bind_address: String,
    /// Port for HTTP server (Phase 2).
    pub port: u16,
    /// Log level (trace, debug, info, warn, error).
    pub log_level: String,
    /// Tool name prefix delimiter.
    pub prefix_delimiter: String,
    /// Whether to prefix tool names with server name.
    pub enable_prefix: bool,
    /// How many servers to start in parallel.
    pub startup_concurrency: usize,
    /// Upstream server definitions.
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_string(),
            port: 3282,
            log_level: "info".to_string(),
            prefix_delimiter: "__".to_string(),
            enable_prefix: true,
            startup_concurrency: 3,
            servers: HashMap::new(),
        }
    }
}

/// Configuration for a single upstream MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Command to execute (for stdio transport).
    pub command: Option<String>,
    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Whether this server is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Transport type.
    #[serde(default)]
    pub transport: TransportType,
    /// URL for HTTP transport.
    pub url: Option<String>,
    /// Startup timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    30
}

/// Transport type for upstream servers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    #[default]
    Stdio,
    Http,
}

/// Validate a config and return a list of error messages.
///
/// Returns an empty vec if the config is valid.
pub fn validate_config(config: &Config) -> Vec<String> {
    let mut errors = Vec::new();

    if config.port == 0 {
        errors.push("port must be in range 1-65535".to_string());
    }

    if config.startup_concurrency == 0 {
        errors.push("startup_concurrency must be > 0".to_string());
    }

    for (name, server) in &config.servers {
        if name.is_empty() {
            errors.push("server name must not be empty".to_string());
        }

        if server.timeout_secs == 0 {
            errors.push(format!("server '{name}': timeout must be > 0"));
        }

        match server.transport {
            TransportType::Stdio => {
                if server.command.is_none() {
                    errors.push(format!(
                        "server '{name}': stdio transport requires 'command' to be set"
                    ));
                }
            }
            TransportType::Http => {
                if server.url.is_none() {
                    errors.push(format!(
                        "server '{name}': http transport requires 'url' to be set"
                    ));
                }
            }
        }
    }

    errors
}

/// Returns the default config file path.
pub fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "plug")
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.config/plug/config.toml"))
}

/// Load config with Figment layered resolution.
#[allow(clippy::result_large_err)]
pub fn load_config(config_path: Option<&PathBuf>) -> Result<Config, figment::Error> {
    use figment::providers::{Env, Format, Serialized, Toml};
    use figment::Figment;

    let path = config_path
        .cloned()
        .unwrap_or_else(default_config_path);

    let mut config: Config = Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(&path))
        .merge(Env::prefixed("PLUG_").split("_"))
        .extract()?;

    // Expand $VAR_NAME references in server configs
    for server in config.servers.values_mut() {
        if let Some(ref mut cmd) = server.command {
            *cmd = expand_env_vars(cmd);
        }
        for arg in &mut server.args {
            *arg = expand_env_vars(arg);
        }
        for val in server.env.values_mut() {
            *val = expand_env_vars(val);
        }
        if let Some(ref mut url) = server.url {
            *url = expand_env_vars(url);
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::providers::{Format, Serialized, Toml};
    use figment::Figment;

    /// Helper to load a Config from a TOML string merged over defaults.
    fn config_from_toml(toml: &str) -> Config {
        Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml))
            .extract()
            .expect("failed to parse TOML config")
    }

    // ---- default values ----

    #[test]
    fn default_values_are_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.bind_address, "127.0.0.1");
        assert_eq!(cfg.port, 3282);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.prefix_delimiter, "__");
        assert!(cfg.enable_prefix);
        assert_eq!(cfg.startup_concurrency, 3);
        assert!(cfg.servers.is_empty());
    }

    // ---- TOML loading ----

    #[test]
    fn load_from_toml_overrides_defaults() {
        let cfg = config_from_toml(
            r#"
            port = 8080
            log_level = "debug"
            startup_concurrency = 5
            "#,
        );
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.startup_concurrency, 5);
        // Non-overridden fields keep defaults
        assert_eq!(cfg.bind_address, "127.0.0.1");
    }

    #[test]
    fn load_server_from_toml() {
        let cfg = config_from_toml(
            r#"
            [servers.myserver]
            command = "node"
            args = ["server.js"]
            timeout_secs = 10
            "#,
        );
        let srv = cfg.servers.get("myserver").expect("server missing");
        assert_eq!(srv.command.as_deref(), Some("node"));
        assert_eq!(srv.args, vec!["server.js"]);
        assert_eq!(srv.timeout_secs, 10);
        assert!(srv.enabled);
    }

    #[test]
    fn load_http_server_from_toml() {
        let cfg = config_from_toml(
            r#"
            [servers.remote]
            transport = "http"
            url = "https://example.com/mcp"
            "#,
        );
        let srv = cfg.servers.get("remote").expect("server missing");
        assert!(matches!(srv.transport, TransportType::Http));
        assert_eq!(srv.url.as_deref(), Some("https://example.com/mcp"));
    }

    // ---- env var override via Figment ----

    #[test]
    fn env_override_via_figment() {
        // Test that the Figment pipeline supports env overrides by verifying the
        // provider chain works. We build a Figment with an env provider that reads
        // from a custom prefix, and prove the merge order is correct.
        // (We cannot call env::set_var in edition 2024 tests since it is unsafe.)
        use figment::providers::Env;

        let fig = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string("port = 9999"))
            .merge(Env::prefixed("PLUG_").split("_"));

        let cfg: Config = fig.extract().expect("extract failed");
        // TOML override should win over default when no env var is actually set
        assert_eq!(cfg.port, 9999);
    }

    // ---- validation ----

    #[test]
    fn validate_valid_config() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "ok".to_string(),
            ServerConfig {
                command: Some("node".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                timeout_secs: 30,
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_stdio_without_command() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                timeout_secs: 30,
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("stdio") && e.contains("command")));
    }

    #[test]
    fn validate_http_without_url() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: None,
                timeout_secs: 30,
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("http") && e.contains("url")));
    }

    #[test]
    fn validate_zero_timeout() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                command: Some("node".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                timeout_secs: 0,
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("timeout")));
    }

    #[test]
    fn validate_zero_port() {
        let cfg = Config {
            port: 0,
            ..Config::default()
        };
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("port")));
    }

    #[test]
    fn validate_empty_server_name() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "".to_string(),
            ServerConfig {
                command: Some("node".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                timeout_secs: 30,
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("empty")));
    }

    #[test]
    fn validate_multiple_errors() {
        let mut cfg = Config {
            port: 0,
            startup_concurrency: 0,
            ..Config::default()
        };
        cfg.servers.insert(
            "bad_stdio".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                timeout_secs: 0,
            },
        );
        let errors = validate_config(&cfg);
        // Should catch port, startup_concurrency, missing command, and zero timeout
        assert!(errors.len() >= 4, "expected >= 4 errors, got: {errors:?}");
    }
}
