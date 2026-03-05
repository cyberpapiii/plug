pub(crate) mod expand;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use expand::expand_env_vars;

/// Top-level configuration for plug.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Log level (trace, debug, info, warn, error).
    pub log_level: String,
    /// Tool name prefix delimiter.
    pub prefix_delimiter: String,
    /// Whether to prefix tool names with server name.
    pub enable_prefix: bool,
    /// How many servers to start in parallel.
    pub startup_concurrency: usize,
    /// Enable client-aware tool filtering (default: true).
    #[serde(default = "default_true")]
    pub tool_filter_enabled: bool,
    /// Max chars for tool descriptions (None = no truncation).
    #[serde(default)]
    pub tool_description_max_chars: Option<usize>,
    /// Tool count threshold to activate search_tools meta-tool (default: 50).
    #[serde(default = "default_tool_search_threshold")]
    pub tool_search_threshold: usize,
    /// Priority tools served first when filtering (tool names).
    #[serde(default)]
    pub priority_tools: Vec<String>,
    /// HTTP server configuration.
    #[serde(default)]
    pub http: HttpConfig,
    /// Seconds to keep daemon alive after last client disconnects (default: 60).
    /// Set to 0 to disable auto-shutdown (daemon stays alive indefinitely).
    #[serde(default = "default_grace_period")]
    pub daemon_grace_period_secs: u64,
    /// Upstream server definitions.
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            prefix_delimiter: "__".to_string(),
            enable_prefix: true,
            startup_concurrency: 3,
            tool_filter_enabled: true,
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            priority_tools: Vec::new(),
            http: HttpConfig::default(),
            daemon_grace_period_secs: 60,
            servers: HashMap::new(),
        }
    }
}

/// HTTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// Bind address for HTTP server.
    pub bind_address: String,
    /// Port for HTTP server.
    pub port: u16,
    /// Session timeout in seconds.
    pub session_timeout_secs: u64,
    /// Maximum number of concurrent sessions.
    pub max_sessions: usize,
    /// SSE channel buffer capacity per session.
    pub sse_channel_capacity: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".to_string(),
            port: 3282,
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: 32,
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
    /// Bearer token for HTTP upstream authentication.
    pub auth_token: Option<crate::types::SecretString>,
    /// Startup timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Tool call timeout in seconds (default: 300). Set higher for slow tools.
    #[serde(default = "default_call_timeout")]
    pub call_timeout_secs: u64,
    /// Max concurrent requests to this server (default: 1 for stdio, 10 for HTTP).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Health check interval in seconds (default: 60).
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,
    /// Enable circuit breaker for this server (default: true).
    #[serde(default = "default_true")]
    pub circuit_breaker_enabled: bool,
    /// Enable tool enrichment (annotation inference + title normalization).
    #[serde(default)]
    pub enrichment: bool,
    /// Manual tool renames (original_name -> new_name).
    #[serde(default)]
    pub tool_renames: HashMap<String, String>,
    }

    fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    30
}

fn default_call_timeout() -> u64 {
    300
}

fn default_max_concurrent() -> usize {
    1
}

fn default_health_interval() -> u64 {
    60
}

fn default_tool_search_threshold() -> usize {
    50
}

fn default_grace_period() -> u64 {
    60
}

/// Transport type for upstream servers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

    if config.http.port == 0 {
        errors.push("http.port must be in range 1-65535".to_string());
    }

    if config.startup_concurrency == 0 {
        errors.push("startup_concurrency must be > 0".to_string());
    }

    if config.tool_search_threshold < 10 {
        errors.push("tool_search_threshold must be >= 10".to_string());
    }

    for (name, server) in &config.servers {
        if name.is_empty() {
            errors.push("server name must not be empty".to_string());
        }

        if name.contains(&config.prefix_delimiter) {
            errors.push(format!(
                "server '{name}': name must not contain the prefix delimiter '{}'",
                config.prefix_delimiter
            ));
        }

        if server.timeout_secs == 0 {
            errors.push(format!("server '{name}': timeout must be > 0"));
        }

        if server.max_concurrent == 0 {
            errors.push(format!("server '{name}': max_concurrent must be > 0"));
        }

        if server.health_check_interval_secs < 5 {
            errors.push(format!(
                "server '{name}': health_check_interval_secs must be >= 5"
            ));
        }

        if server.max_concurrent > 1 && matches!(server.transport, TransportType::Stdio) {
            tracing::warn!(
                server = %name,
                max_concurrent = server.max_concurrent,
                "max_concurrent > 1 for stdio transport — stdio is serial, this may not behave as expected"
            );
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

/// Extract all `$VAR_NAME` references from the config without expanding them.
///
/// Used by `auto_start_daemon` to forward referenced env vars to the daemon process.
#[allow(clippy::result_large_err)]
pub fn load_raw_config(config_path: Option<PathBuf>) -> Result<Vec<String>, figment::Error> {
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};

    let path = config_path.unwrap_or_else(default_config_path);

    let config: Config = Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(&path))
        .extract()?;

    let mut vars = Vec::new();
    for server in config.servers.values() {
        if let Some(ref cmd) = server.command {
            vars.extend(expand::extract_env_refs(cmd));
        }
        for arg in &server.args {
            vars.extend(expand::extract_env_refs(arg));
        }
        for val in server.env.values() {
            vars.extend(expand::extract_env_refs(val));
        }
        if let Some(ref url) = server.url {
            vars.extend(expand::extract_env_refs(url));
        }
        if let Some(ref token) = server.auth_token {
            vars.extend(expand::extract_env_refs(token.as_str()));
        }
    }
    vars.sort();
    vars.dedup();
    Ok(vars)
}

/// Load config with Figment layered resolution.
#[allow(clippy::result_large_err)]
pub fn load_config(config_path: Option<&PathBuf>) -> Result<Config, figment::Error> {
    use figment::Figment;
    use figment::providers::{Env, Format, Serialized, Toml};

    let path = config_path.cloned().unwrap_or_else(default_config_path);

    let mut config: Config = Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(&path))
        .merge(Env::prefixed("PLUG_").split("__"))
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
        if let Some(ref mut token) = server.auth_token {
            *token = expand_env_vars(token.as_str()).into();
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};

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
        assert_eq!(cfg.http.bind_address, "127.0.0.1");
        assert_eq!(cfg.http.port, 3282);
        assert_eq!(cfg.http.session_timeout_secs, 1800);
        assert_eq!(cfg.http.max_sessions, 100);
        assert_eq!(cfg.http.sse_channel_capacity, 32);
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
            log_level = "debug"
            startup_concurrency = 5

            [http]
            port = 8080
            "#,
        );
        assert_eq!(cfg.http.port, 8080);
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.startup_concurrency, 5);
        // Non-overridden fields keep defaults
        assert_eq!(cfg.http.bind_address, "127.0.0.1");
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
            .merge(Toml::string("startup_concurrency = 7"))
            .merge(Env::prefixed("PLUG_").split("__"));

        let cfg: Config = fig.extract().expect("extract failed");
        // TOML override should win over default when no env var is actually set
        assert_eq!(cfg.startup_concurrency, 7);
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
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
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
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("stdio") && e.contains("command"))
        );
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
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("http") && e.contains("url"))
        );
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
                auth_token: None,
                timeout_secs: 0,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("timeout")));
    }

    #[test]
    fn validate_zero_port() {
        let mut cfg = Config::default();
        cfg.http.port = 0;
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
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("empty")));
    }

    #[test]
    fn validate_multiple_errors() {
        let mut cfg = Config {
            startup_concurrency: 0,
            ..Config::default()
        };
        cfg.http.port = 0;
        cfg.servers.insert(
            "bad_stdio".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                timeout_secs: 0,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
            },
        );
        let errors = validate_config(&cfg);
        // Should catch port, startup_concurrency, missing command, and zero timeout
        assert!(errors.len() >= 4, "expected >= 4 errors, got: {errors:?}");
    }
}
