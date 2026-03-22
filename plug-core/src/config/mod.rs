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
    /// Legacy setting retained for compatibility.
    /// Tool names are always prefixed in v0.1 to avoid collisions.
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
    /// Expose only plug meta-tools instead of the full merged tool catalog.
    #[serde(default)]
    pub meta_tool_mode: bool,
    /// Priority tools served first when filtering (tool names).
    #[serde(default)]
    pub priority_tools: Vec<String>,
    /// Disabled tool names or wildcard patterns (e.g. "Slack__*" or "plug__search_tools").
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// HTTP server configuration.
    #[serde(default)]
    pub http: HttpConfig,
    /// Seconds to keep daemon alive after last client disconnects.
    /// Default is 0, which disables auto-shutdown and keeps the daemon alive
    /// until explicit shutdown.
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
            meta_tool_mode: false,
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            http: HttpConfig::default(),
            daemon_grace_period_secs: 0,
            servers: HashMap::new(),
        }
    }
}

/// HTTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// Downstream authentication mode for remote HTTP clients.
    #[serde(default)]
    pub auth_mode: DownstreamAuthMode,
    /// Public base URL used by remote clients to reach this server.
    pub public_base_url: Option<String>,
    /// OAuth client ID for downstream remote connectors.
    pub oauth_client_id: Option<String>,
    /// OAuth client secret for downstream remote connectors.
    pub oauth_client_secret: Option<crate::types::SecretString>,
    /// OAuth scopes to request for downstream remote connectors.
    #[serde(default)]
    pub oauth_scopes: Option<Vec<String>>,
    /// Bind address for HTTP server.
    pub bind_address: String,
    /// Port for HTTP server.
    pub port: u16,
    /// Explicitly allowed Origin header values for HTTP MCP requests.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// PEM-encoded certificate chain for HTTPS serving.
    pub tls_cert_path: Option<PathBuf>,
    /// PEM-encoded private key for HTTPS serving.
    pub tls_key_path: Option<PathBuf>,
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
            auth_mode: DownstreamAuthMode::default(),
            public_base_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            oauth_scopes: None,
            bind_address: "127.0.0.1".to_string(),
            port: 3282,
            allowed_origins: Vec::new(),
            tls_cert_path: None,
            tls_key_path: None,
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: 32,
        }
    }
}

/// Authentication mode for downstream HTTP clients.
///
/// `Auto` preserves the current v0.1 behavior as a compatibility path:
/// - loopback bind => no downstream auth
/// - non-loopback bind => bearer auth
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownstreamAuthMode {
    #[default]
    Auto,
    None,
    Bearer,
    Oauth,
}

impl DownstreamAuthMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Bearer => "bearer",
            Self::Oauth => "oauth",
        }
    }
}

pub fn downstream_mcp_url(http: &HttpConfig) -> String {
    if let Some(public_base_url) = &http.public_base_url {
        format!("{}/mcp", public_base_url.trim_end_matches('/'))
    } else {
        format!("http://localhost:{}/mcp", http.port)
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
    /// Authentication mode for upstream server ("oauth" for OAuth 2.1 + PKCE).
    /// Mutually exclusive with `auth_token`.
    pub auth: Option<String>,
    /// OAuth client ID for pre-registered clients. If absent, dynamic client registration is used.
    pub oauth_client_id: Option<String>,
    /// OAuth scopes to request during authorization.
    #[serde(default)]
    pub oauth_scopes: Option<Vec<String>>,
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
    /// Tool group classification for sub-service decomposition.
    /// Maps group prefix (e.g. "Gmail") to match rules.
    /// When set, tools matching a rule get `GroupPrefix__tool_name` instead of `ServerName__tool_name`.
    #[serde(default)]
    pub tool_groups: Vec<ToolGroupRule>,
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
    0
}

/// Check whether a bind address refers to loopback (localhost-only).
pub fn http_bind_is_loopback(bind_address: &str) -> bool {
    matches!(bind_address, "127.0.0.1" | "::1" | "[::1]" | "localhost")
}

/// Transport type for upstream servers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    #[default]
    Stdio,
    Http,
    Sse,
}

/// A rule for classifying tools into named groups (sub-services).
///
/// Tools whose name contains any of the `contains` keywords are assigned
/// the `prefix` as their group name. Rules are evaluated in order; first match wins.
///
/// Only keywords listed in `strip` are removed from the tool name to avoid redundancy.
/// Classification keywords that aren't in `strip` are kept (e.g., "event" classifies
/// a tool as Calendar but shouldn't be stripped from `manage_event`).
///
/// Example TOML:
/// ```toml
/// [[servers.workspace.tool_groups]]
/// prefix = "Gmail"
/// contains = ["gmail"]
/// strip = ["gmail"]
///
/// [[servers.workspace.tool_groups]]
/// prefix = "GoogleCalendar"
/// contains = ["event", "calendar", "freebusy"]
/// strip = ["calendar"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolGroupRule {
    /// The group prefix (e.g. "Gmail", "GoogleDrive").
    pub prefix: String,
    /// Keywords to match in the tool name. Any match triggers this rule.
    pub contains: Vec<String>,
    /// Keywords to strip from the tool name to avoid redundancy.
    /// Only these are removed; other `contains` keywords are kept.
    /// If empty, no stripping is performed.
    #[serde(default)]
    pub strip: Vec<String>,
}

/// Sanitize server name for use in filesystem paths (e.g. token storage).
/// Rejects path separators, parent directory references, hidden file prefixes,
/// and null bytes. Caps length to 255 bytes.
pub fn sanitize_server_name_for_path(name: &str) -> Result<&str, String> {
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!(
            "server name '{name}' contains invalid path characters"
        ));
    }
    if name.starts_with('.') || name.contains("..") {
        return Err(format!("server name '{name}' contains directory traversal"));
    }
    if name.len() > 255 {
        return Err(format!("server name '{name}' exceeds 255 bytes"));
    }
    Ok(name)
}

/// Validate a config and return a list of error messages.
///
/// Returns an empty vec if the config is valid.
pub fn validate_config(config: &Config) -> Vec<String> {
    let mut errors = Vec::new();

    if config.http.port == 0 {
        errors.push("http.port must be in range 1-65535".to_string());
    }
    if matches!(config.http.auth_mode, DownstreamAuthMode::Oauth)
        && config.http.public_base_url.is_none()
    {
        errors.push("http.public_base_url is required when http.auth_mode = \"oauth\"".to_string());
    }
    if matches!(config.http.auth_mode, DownstreamAuthMode::Oauth)
        && config.http.oauth_client_id.is_none()
    {
        errors.push("http.oauth_client_id is required when http.auth_mode = \"oauth\"".to_string());
    }
    if !http_bind_is_loopback(&config.http.bind_address)
        && matches!(config.http.auth_mode, DownstreamAuthMode::None)
    {
        errors.push(
            "http.auth_mode = \"none\" is not allowed when binding a non-loopback downstream address"
                .to_string(),
        );
    }

    if !http_bind_is_loopback(&config.http.bind_address) && config.http.tls_cert_path.is_none() {
        errors.push(
            "http.tls_cert_path and http.tls_key_path are required when binding a non-loopback downstream address".to_string(),
        );
    }

    match (&config.http.tls_cert_path, &config.http.tls_key_path) {
        (Some(_), None) | (None, Some(_)) => {
            errors.push("http.tls_cert_path and http.tls_key_path must be set together".to_string())
        }
        (Some(cert), Some(key)) => {
            if !cert.exists() {
                errors.push(format!(
                    "http.tls_cert_path '{}' does not exist",
                    cert.display()
                ));
            }
            if !key.exists() {
                errors.push(format!(
                    "http.tls_key_path '{}' does not exist",
                    key.display()
                ));
            }
            if cert.exists() && std::fs::File::open(cert).is_err() {
                errors.push(format!(
                    "http.tls_cert_path '{}' is not readable",
                    cert.display()
                ));
            }
            if key.exists() && std::fs::File::open(key).is_err() {
                errors.push(format!(
                    "http.tls_key_path '{}' is not readable",
                    key.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                if let Ok(metadata) = std::fs::metadata(key) {
                    let mode = metadata.permissions().mode();
                    if mode & 0o077 != 0 {
                        errors.push(format!(
                            "http.tls_key_path '{}' must not be group/world readable",
                            key.display()
                        ));
                    }
                }
            }
        }
        (None, None) => {}
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

        // Server name must be filesystem-safe for token storage
        if let Err(e) = sanitize_server_name_for_path(name) {
            errors.push(format!("server '{name}': {e}"));
        }

        // OAuth validation
        if server.auth.as_deref() == Some("oauth") {
            if server.auth_token.is_some() {
                errors.push(format!(
                    "server '{name}': auth = \"oauth\" is mutually exclusive with auth_token"
                ));
            }
            if matches!(server.transport, TransportType::Stdio) {
                errors.push(format!(
                    "server '{name}': auth = \"oauth\" requires http or sse transport, not stdio"
                ));
            }
        }
        if let Some(ref auth) = server.auth {
            if auth != "oauth" {
                errors.push(format!(
                    "server '{name}': auth must be \"oauth\" if set (got \"{auth}\")"
                ));
            }
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
            TransportType::Sse => {
                if server.url.is_none() {
                    errors.push(format!(
                        "server '{name}': sse transport requires 'url' to be set"
                    ));
                }
            }
        }
    }

    errors
}

fn resolved_env_source(config_path: &std::path::Path) -> HashMap<String, String> {
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    env_map.extend(crate::dotenv::read_dotenv_vars_for_config(Some(config_path)));
    env_map
}

fn transform_path_buf(path: &mut PathBuf, mut transform: impl FnMut(String) -> String) {
    *path = PathBuf::from(transform(path.to_string_lossy().into_owned()));
}

fn transform_env_expandable_strings(
    config: &mut Config,
    mut transform: impl FnMut(String) -> String,
) {
    config.log_level = transform(config.log_level.clone());
    config.prefix_delimiter = transform(config.prefix_delimiter.clone());
    for tool in &mut config.priority_tools {
        *tool = transform(tool.clone());
    }
    for tool in &mut config.disabled_tools {
        *tool = transform(tool.clone());
    }

    config.http.bind_address = transform(config.http.bind_address.clone());
    if let Some(ref mut public_base_url) = config.http.public_base_url {
        *public_base_url = transform(public_base_url.clone());
    }
    if let Some(ref mut client_id) = config.http.oauth_client_id {
        *client_id = transform(client_id.clone());
    }
    if let Some(ref mut secret) = config.http.oauth_client_secret {
        *secret = transform(secret.as_str().to_string()).into();
    }
    if let Some(ref mut scopes) = config.http.oauth_scopes {
        for scope in scopes.iter_mut() {
            *scope = transform(scope.clone());
        }
    }
    for origin in &mut config.http.allowed_origins {
        *origin = transform(origin.clone());
    }
    if let Some(ref mut cert_path) = config.http.tls_cert_path {
        transform_path_buf(cert_path, &mut transform);
    }
    if let Some(ref mut key_path) = config.http.tls_key_path {
        transform_path_buf(key_path, &mut transform);
    }

    for server in config.servers.values_mut() {
        if let Some(ref mut cmd) = server.command {
            *cmd = transform(cmd.clone());
        }
        for arg in &mut server.args {
            *arg = transform(arg.clone());
        }
        for val in server.env.values_mut() {
            *val = transform(val.clone());
        }
        if let Some(ref mut url) = server.url {
            *url = transform(url.clone());
        }
        if let Some(ref mut token) = server.auth_token {
            *token = transform(token.as_str().to_string()).into();
        }
        if let Some(ref mut auth) = server.auth {
            *auth = transform(auth.clone());
        }
        if let Some(ref mut client_id) = server.oauth_client_id {
            *client_id = transform(client_id.clone());
        }
        if let Some(ref mut scopes) = server.oauth_scopes {
            for scope in scopes.iter_mut() {
                *scope = transform(scope.clone());
            }
        }
        if !server.tool_renames.is_empty() {
            let remapped = server
                .tool_renames
                .drain()
                .map(|(key, value)| (transform(key), transform(value)))
                .collect();
            server.tool_renames = remapped;
        }
        for group in &mut server.tool_groups {
            group.prefix = transform(group.prefix.clone());
            for item in &mut group.contains {
                *item = transform(item.clone());
            }
            for item in &mut group.strip {
                *item = transform(item.clone());
            }
        }
    }
}

pub(crate) fn visit_env_expandable_fields(
    config: &Config,
    mut visit: impl FnMut(Option<&str>, &str, &str),
) {
    visit(None, "log_level", &config.log_level);
    visit(None, "prefix_delimiter", &config.prefix_delimiter);
    for tool in &config.priority_tools {
        visit(None, "priority_tools", tool);
    }
    for tool in &config.disabled_tools {
        visit(None, "disabled_tools", tool);
    }

    visit(None, "http.bind_address", &config.http.bind_address);
    for origin in &config.http.allowed_origins {
        visit(None, "http.allowed_origins", origin);
    }
    if let Some(ref cert_path) = config.http.tls_cert_path {
        visit(None, "http.tls_cert_path", cert_path.to_string_lossy().as_ref());
    }
    if let Some(ref key_path) = config.http.tls_key_path {
        visit(None, "http.tls_key_path", key_path.to_string_lossy().as_ref());
    }
    if let Some(ref public_base_url) = config.http.public_base_url {
        visit(None, "http.public_base_url", public_base_url);
    }
    if let Some(ref client_id) = config.http.oauth_client_id {
        visit(None, "http.oauth_client_id", client_id);
    }
    if let Some(ref secret) = config.http.oauth_client_secret {
        visit(None, "http.oauth_client_secret", secret.as_str());
    }
    if let Some(ref scopes) = config.http.oauth_scopes {
        for scope in scopes {
            visit(None, "http.oauth_scopes", scope);
        }
    }

    for (server_name, server) in &config.servers {
        if let Some(ref cmd) = server.command {
            visit(Some(server_name), "command", cmd);
        }
        for arg in &server.args {
            visit(Some(server_name), "args", arg);
        }
        for (key, val) in &server.env {
            let field = format!("env.{key}");
            visit(Some(server_name), &field, val);
        }
        if let Some(ref url) = server.url {
            visit(Some(server_name), "url", url);
        }
        if let Some(ref token) = server.auth_token {
            visit(Some(server_name), "auth_token", token.as_str());
        }
        if let Some(ref auth) = server.auth {
            visit(Some(server_name), "auth", auth);
        }
        if let Some(ref client_id) = server.oauth_client_id {
            visit(Some(server_name), "oauth_client_id", client_id);
        }
        if let Some(ref scopes) = server.oauth_scopes {
            for scope in scopes {
                visit(Some(server_name), "oauth_scopes", scope);
            }
        }
        for (key, value) in &server.tool_renames {
            visit(Some(server_name), "tool_renames.key", key);
            visit(Some(server_name), "tool_renames.value", value);
        }
        for group in &server.tool_groups {
            visit(Some(server_name), "tool_groups.prefix", &group.prefix);
            for item in &group.contains {
                visit(Some(server_name), "tool_groups.contains", item);
            }
            for item in &group.strip {
                visit(Some(server_name), "tool_groups.strip", item);
            }
        }
    }
}

pub(crate) fn visit_env_expandable_strings(config: &Config, mut visit: impl FnMut(&str)) {
    visit_env_expandable_fields(config, |_, _, value| visit(value));
}

fn expand_config_env_refs(config: &mut Config, env_map: &HashMap<String, String>) {
    use expand::expand_env_vars_with_source;

    transform_env_expandable_strings(config, |value| expand_env_vars_with_source(&value, env_map));
}

/// Returns the plug config directory (e.g. `~/.config/plug/`).
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "plug")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~/.config/plug"))
}

/// Returns the default config file path.
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
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
    visit_env_expandable_strings(&config, |value| vars.extend(expand::extract_env_refs(value)));
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
    let env_map = resolved_env_source(&path);
    expand_config_env_refs(&mut config, &env_map);

    let errors = validate_config(&config);
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(figment::Error::from(errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};
    use std::path::PathBuf;

    /// Helper to load a Config from a TOML string merged over defaults.
    fn config_from_toml(toml: &str) -> Config {
        Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(toml))
            .extract()
            .expect("failed to parse TOML config")
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "plug-config-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    // ---- default values ----

    #[test]
    fn default_values_are_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.http.auth_mode, DownstreamAuthMode::Auto);
        assert_eq!(cfg.http.public_base_url, None);
        assert_eq!(cfg.http.oauth_client_id, None);
        assert_eq!(
            cfg.http.oauth_client_secret.as_ref().map(|s| s.as_str()),
            None
        );
        assert_eq!(cfg.http.oauth_scopes, None);
        assert_eq!(cfg.http.bind_address, "127.0.0.1");
        assert_eq!(cfg.http.port, 3282);
        assert!(cfg.http.allowed_origins.is_empty());
        assert_eq!(cfg.http.tls_cert_path, None);
        assert_eq!(cfg.http.tls_key_path, None);
        assert_eq!(cfg.http.session_timeout_secs, 1800);
        assert_eq!(cfg.http.max_sessions, 100);
        assert_eq!(cfg.http.sse_channel_capacity, 32);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.prefix_delimiter, "__");
        assert!(cfg.enable_prefix);
        assert_eq!(cfg.startup_concurrency, 3);
        assert!(cfg.disabled_tools.is_empty());
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
            auth_mode = "auto"
            public_base_url = "https://plug.example.com"
            oauth_client_id = "client-123"
            oauth_client_secret = "secret-123"
            oauth_scopes = ["tools:read", "tools:write"]
            port = 8080
            allowed_origins = ["https://claude.ai"]
            tls_cert_path = "/tmp/plug-cert.pem"
            tls_key_path = "/tmp/plug-key.pem"
            "#,
        );
        assert_eq!(cfg.http.auth_mode, DownstreamAuthMode::Auto);
        assert_eq!(
            cfg.http.public_base_url.as_deref(),
            Some("https://plug.example.com")
        );
        assert_eq!(cfg.http.oauth_client_id.as_deref(), Some("client-123"));
        assert_eq!(
            cfg.http.oauth_client_secret.as_ref().map(|s| s.as_str()),
            Some("secret-123")
        );
        assert_eq!(
            cfg.http.oauth_scopes,
            Some(vec!["tools:read".to_string(), "tools:write".to_string()])
        );
        assert_eq!(cfg.http.port, 8080);
        assert_eq!(cfg.http.allowed_origins, vec!["https://claude.ai"]);
        assert_eq!(
            cfg.http.tls_cert_path.as_deref(),
            Some(std::path::Path::new("/tmp/plug-cert.pem"))
        );
        assert_eq!(
            cfg.http.tls_key_path.as_deref(),
            Some(std::path::Path::new("/tmp/plug-key.pem"))
        );
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.startup_concurrency, 5);
        // Non-overridden fields keep defaults
        assert_eq!(cfg.http.bind_address, "127.0.0.1");
    }

    #[test]
    fn load_config_uses_dotenv_adjacent_to_active_config_path() {
        let dir = unique_temp_dir("dotenv-adjacent");
        let config_path = dir.join("config.toml");
        std::fs::write(
            dir.join(".env"),
            "TEST_REMOTE_URL=https://env.example.com/mcp\n",
        )
        .unwrap();
        std::fs::write(
            &config_path,
            r#"
            [servers.demo]
            transport = "http"
            url = "$TEST_REMOTE_URL"
            "#,
        )
        .unwrap();

        let cfg = load_config(Some(&config_path)).expect("load config with sibling dotenv");
        assert_eq!(
            cfg.servers.get("demo").and_then(|server| server.url.as_deref()),
            Some("https://env.example.com/mcp")
        );
    }

    #[test]
    fn load_config_enforces_semantic_validation() {
        let dir = unique_temp_dir("validation");
        let config_path = dir.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
            [http]
            bind_address = "0.0.0.0"
            auth_mode = "none"
            "#,
        )
        .unwrap();

        let err = load_config(Some(&config_path)).expect_err("semantic validation should fail");
        assert!(
            err.to_string().contains("auth_mode = \"none\""),
            "unexpected error: {err}"
        );
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 0,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
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
    fn validate_bearer_on_loopback_is_valid() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Bearer;
        let errors = validate_config(&cfg);
        assert!(
            errors.is_empty(),
            "expected bearer on loopback to be valid, got {errors:?}"
        );
    }

    #[test]
    fn validate_oauth_on_loopback_is_valid_when_required_fields_are_set() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_client_id = Some("client-123".to_string());
        let errors = validate_config(&cfg);
        assert!(
            errors.is_empty(),
            "expected oauth on loopback to be valid, got {errors:?}"
        );
    }

    #[test]
    fn validate_oauth_requires_public_base_url() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("public_base_url") && e.contains("oauth")),
            "expected oauth/public_base_url validation error, got {errors:?}"
        );
    }

    #[test]
    fn validate_oauth_requires_client_id() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("oauth_client_id") && e.contains("oauth")),
            "expected oauth/oauth_client_id validation error, got {errors:?}"
        );
    }

    #[test]
    fn validate_none_on_non_loopback_is_rejected() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::None;
        cfg.http.bind_address = "0.0.0.0".to_string();
        cfg.http.tls_cert_path = Some(PathBuf::from("/tmp/cert.pem"));
        cfg.http.tls_key_path = Some(PathBuf::from("/tmp/key.pem"));
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("http.auth_mode = \"none\"") && e.contains("non-loopback")),
            "expected none/non-loopback validation error, got {errors:?}"
        );
    }

    #[test]
    fn downstream_mcp_url_prefers_public_base_url() {
        let mut cfg = Config::default();
        cfg.http.public_base_url = Some("https://plug.example.com/base/".to_string());
        assert_eq!(
            downstream_mcp_url(&cfg.http),
            "https://plug.example.com/base/mcp"
        );
    }

    #[test]
    fn downstream_mcp_url_falls_back_to_localhost_port() {
        let mut cfg = Config::default();
        cfg.http.port = 4411;
        assert_eq!(downstream_mcp_url(&cfg.http), "http://localhost:4411/mcp");
    }

    #[test]
    fn load_http_auth_mode_from_toml() {
        let cfg = config_from_toml(
            r#"
            [http]
            auth_mode = "bearer"
            "#,
        );
        assert_eq!(cfg.http.auth_mode, DownstreamAuthMode::Bearer);
    }

    #[test]
    fn validate_tls_paths_must_be_paired() {
        let mut cfg = Config::default();
        cfg.http.tls_cert_path = Some(PathBuf::from("/tmp/cert.pem"));
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("must be set together")));
    }

    #[test]
    fn validate_tls_paths_must_exist() {
        let mut cfg = Config::default();
        cfg.http.tls_cert_path = Some(PathBuf::from("/definitely/missing-cert.pem"));
        cfg.http.tls_key_path = Some(PathBuf::from("/definitely/missing-key.pem"));
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("tls_cert_path")));
        assert!(errors.iter().any(|e| e.contains("tls_key_path")));
    }

    #[test]
    fn validate_non_loopback_bind_requires_tls() {
        let mut cfg = Config::default();
        cfg.http.bind_address = "0.0.0.0".to_string();
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("non-loopback")));
    }

    #[cfg(unix)]
    #[test]
    fn validate_tls_key_must_not_be_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let temp = std::env::temp_dir().join(format!(
            "plug-key-perms-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let cert = temp.join("cert.pem");
        let key = temp.join("key.pem");
        std::fs::write(&cert, "cert").expect("write cert");
        std::fs::write(&key, "key").expect("write key");
        let mut perms = std::fs::metadata(&key).expect("key metadata").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&key, perms).expect("set key perms");

        let mut cfg = Config::default();
        cfg.http.tls_cert_path = Some(cert);
        cfg.http.tls_key_path = Some(key);

        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("must not be group/world readable"))
        );
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 0,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        // Should catch port, startup_concurrency, missing command, and zero timeout
        assert!(errors.len() >= 4, "expected >= 4 errors, got: {errors:?}");
    }

    #[test]
    fn validate_oauth_mutual_exclusion_with_auth_token() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some("https://example.com/mcp".to_string()),
                auth_token: Some(crate::types::SecretString::from("token".to_string())),
                auth: Some("oauth".to_string()),
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("mutually exclusive")));
    }

    #[test]
    fn validate_oauth_requires_http_or_sse_transport() {
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
                auth: Some("oauth".to_string()),
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("requires http or sse")));
    }

    #[test]
    fn validate_invalid_auth_value() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some("https://example.com/mcp".to_string()),
                auth_token: None,
                auth: Some("basic".to_string()),
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.iter().any(|e| e.contains("must be \"oauth\"")));
    }

    #[test]
    fn validate_server_name_path_traversal() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "../etc/passwd".to_string(),
            ServerConfig {
                command: Some("node".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("directory traversal") || e.contains("invalid path"))
        );
    }

    #[test]
    fn validate_oauth_config_on_http_is_valid() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "notion".to_string(),
            ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some("https://mcp.notion.so/mcp".to_string()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: None,
                oauth_scopes: Some(vec!["mcp:read".to_string(), "mcp:write".to_string()]),
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn sanitize_rejects_path_separators() {
        assert!(sanitize_server_name_for_path("foo/bar").is_err());
        assert!(sanitize_server_name_for_path("foo\\bar").is_err());
        assert!(sanitize_server_name_for_path("foo\0bar").is_err());
    }

    #[test]
    fn sanitize_rejects_directory_traversal() {
        assert!(sanitize_server_name_for_path("..").is_err());
        assert!(sanitize_server_name_for_path(".hidden").is_err());
        assert!(sanitize_server_name_for_path("foo..bar").is_err());
    }

    #[test]
    fn sanitize_accepts_valid_names() {
        assert!(sanitize_server_name_for_path("notion").is_ok());
        assert!(sanitize_server_name_for_path("my-server").is_ok());
        assert!(sanitize_server_name_for_path("server_123").is_ok());
    }

    #[test]
    fn sanitize_rejects_long_names() {
        let long = "a".repeat(256);
        assert!(sanitize_server_name_for_path(&long).is_err());
        let ok = "a".repeat(255);
        assert!(sanitize_server_name_for_path(&ok).is_ok());
    }
}
