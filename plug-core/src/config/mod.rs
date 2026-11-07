pub mod env;
mod events;
pub mod secrets;
mod token_place;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::LazyToolSetting;
pub use events::{
    EVENT_SCOPE, EventsConfig, MIN_WATCH_SECS, SLACK_EVENT_NAME, SLACK_SERVER, SlackEventsConfig,
    WatchConfig, events_missing, events_possible,
};
pub use secrets::{CommandStoreConfig, FILE, KEYCHAIN, OP, SecretsConfig};
pub use token_place::TokenPlace;

/// Top-level configuration for plug.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Log level (trace, debug, info, warn, error).
    pub log_level: String,
    /// Tool name prefix delimiter.
    pub prefix_delimiter: String,
    /// Whether tool names carry their server's prefix. Off, a tool keeps
    /// its own name unless two servers share it.
    pub enable_prefix: bool,
    /// How many servers to start in parallel.
    ///
    /// Starting a server is process spawn and protocol handshake, so it spends
    /// almost all of its time waiting rather than computing. A low cap mostly
    /// buys wall-clock time on a cold start; the knob stays for machines that
    /// cannot afford the spawn burst.
    pub startup_concurrency: usize,
    /// Enable client-aware tool filtering.
    pub tool_filter_enabled: bool,
    /// Max chars for tool descriptions (None = no truncation).
    #[serde(default)]
    pub tool_description_max_chars: Option<usize>,
    /// Tool count threshold to activate search_tools meta-tool.
    pub tool_search_threshold: usize,
    /// Expose only plug meta-tools instead of the full merged tool catalog.
    /// Deprecated compatibility input; `lazy_tools` is the controlling policy model.
    #[serde(default)]
    pub meta_tool_mode: bool,
    /// Client-targeted lazy discovery policy.
    #[serde(default)]
    pub lazy_tools: LazyToolsConfig,
    /// Priority tools served first when filtering (tool names).
    #[serde(default)]
    pub priority_tools: Vec<String>,
    /// Disabled tool names or wildcard patterns (e.g. "Slack__*" or "plug__search_tools").
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// Settings for one client, keyed by how Plug identifies it: see
    /// `IpcLiveSessionInfo::client_key`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub clients: std::collections::BTreeMap<String, ClientSettings>,
    /// Events Plug makes itself, such as watching a tool for change.
    #[serde(default, skip_serializing_if = "EventsConfig::is_empty")]
    pub events: EventsConfig,
    /// Secret stores beyond the built-in ones.
    #[serde(default, skip_serializing_if = "SecretsConfig::is_empty")]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub http: HttpConfig,
    /// Allow configured upstreams to negotiate the MCP 2026-07-28 lifecycle.
    /// This remains false until modern upstream conformance has been run for
    /// the installed release. When false, every upstream uses legacy
    /// initialization regardless of its per-server `protocol` setting.
    #[serde(default)]
    pub modern_upstream_enabled: bool,
    #[serde(default)]
    pub supervision: SupervisionConfig,
    #[serde(default, serialize_with = "in_name_order")]
    pub servers: HashMap<String, ServerConfig>,
}

/// Write a map's entries in the order of their names, so saving the same
/// config twice writes the same file.
fn in_name_order<S, V>(map: &HashMap<String, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    V: Serialize,
{
    serializer.collect_map(map.iter().collect::<std::collections::BTreeMap<_, _>>())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            prefix_delimiter: "__".to_string(),
            enable_prefix: true,
            startup_concurrency: 12,
            tool_filter_enabled: true,
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            lazy_tools: LazyToolsConfig::default(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            clients: std::collections::BTreeMap::new(),
            events: EventsConfig::default(),
            secrets: SecretsConfig::default(),
            http: HttpConfig::default(),
            modern_upstream_enabled: false,
            supervision: SupervisionConfig::default(),
            servers: HashMap::new(),
        }
    }
}

/// Active upstream supervision policy. When an upstream stays
/// degraded past a threshold, the supervisor restarts it (process restart for
/// stdio, reconnect-with-reset for HTTP/SSE), bounded by exponential backoff so
/// a perpetually-failing upstream cannot cause a restart storm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SupervisionConfig {
    /// Whether the supervisor may restart degraded upstreams. A never-degraded
    /// upstream is never touched regardless.
    pub enabled: bool,
    /// Consecutive health-check failures (the `HealthState` streak) an upstream
    /// must accumulate before it is eligible for a supervised restart. An open
    /// circuit breaker also makes it eligible.
    pub degraded_restart_threshold: u32,
    /// Floor of the inter-episode backoff: the supervisor waits at least this
    /// long after a restart before restarting the same upstream again.
    pub min_restart_interval_secs: u64,
    /// Cap of the inter-episode backoff. Each consecutive restart (without a
    /// recovery to healthy in between) doubles the wait, up to this ceiling.
    pub max_restart_interval_secs: u64,
}

impl Default for SupervisionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            degraded_restart_threshold: 5,
            min_restart_interval_secs: 60,
            max_restart_interval_secs: 600,
        }
    }
}

/// What the owner has set for one client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientSettings {
    /// The name to show in place of the one Plug works out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Where the client runs, in the owner's words: "Work laptop", "Cloud".
    /// Unset for a client on this Mac, which needs no telling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// Servers this client does not get: none of their tools are listed to
    /// it or callable by it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_servers: Vec<String>,
    /// Tools this client does not get. See [`ToolBlock`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_tools: Vec<ToolBlock>,
    /// The client gets only what `allowed_servers` and `allowed_tools`
    /// name, even when they name nothing. Naming anything in either does
    /// the same, so this is written only to keep an empty list in force.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_allowed: bool,
    /// The allow list. Once this or `allowed_tools` names anything, the
    /// client gets only what the two name, so a server added later does not
    /// reach it. A block still takes away what the list lets in.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_servers: Vec<String>,
    /// Single tools on the allow list, each by its server and the name that
    /// server gives it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<AllowedTool>,
}

/// One tool a client with an allow list gets, stored the way a tool block
/// is so no rename moves it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AllowedTool {
    pub server: String,
    pub tool: String,
}

impl std::fmt::Display for AllowedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {}", self.tool, self.server)
    }
}

/// One tool kept from a client.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolBlock {
    /// A tool by its server and the name that server gives it, which no
    /// rename or change of prefix touches. `*` in `tool` matches any run of
    /// characters.
    Of { server: String, tool: String },
    /// A rule over the names Plug lists tools under, `*` matching any run of
    /// characters, as in `disabled_tools`. A plain name written here is
    /// turned into [`ToolBlock::Of`] once Plug has seen the tool.
    Named(String),
}

impl From<&str> for ToolBlock {
    fn from(name: &str) -> Self {
        Self::Named(name.to_string())
    }
}

impl std::fmt::Display for ToolBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Of { server, tool } => write!(f, "{tool} on {server}"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

impl ClientSettings {
    /// Nothing is set, so the entry need not be kept.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.place.is_none()
            && self.blocked_servers.is_empty()
            && self.blocked_tools.is_empty()
            && !self.has_allow_list()
    }

    /// Whether the client gets only what its allow list names.
    pub fn has_allow_list(&self) -> bool {
        self.only_allowed || !self.allowed_servers.is_empty() || !self.allowed_tools.is_empty()
    }
}

/// Lazy tool discovery policy configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LazyToolsConfig {
    /// Global policy. `auto` uses the client capability matrix.
    pub mode: LazyToolSetting,
    /// Per-client target overrides keyed by export target slug, e.g. `opencode`.
    #[serde(serialize_with = "in_name_order")]
    pub clients: HashMap<String, LazyToolSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Public-channel Slack MCP Events adapter. Absent means no event routes or state.
    #[serde(default)]
    pub slack_events: Option<SlackEventsConfig>,
    /// Opt in to the MCP 2026-07-28 downstream lifecycle. This remains false
    /// until modern conformance has been run for the installed release.
    #[serde(default)]
    pub modern_downstream_enabled: bool,
    /// Downstream authentication mode for remote HTTP clients.
    #[serde(default)]
    pub auth_mode: DownstreamAuthMode,
    /// Public base URL used by remote clients to reach this server.
    pub public_base_url: Option<String>,
    /// OAuth scopes available to downstream remote connectors.
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

pub const DEFAULT_SSE_CHANNEL_CAPACITY: usize = 32;

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            slack_events: None,
            modern_downstream_enabled: false,
            auth_mode: DownstreamAuthMode::default(),
            public_base_url: None,
            oauth_scopes: None,
            bind_address: "127.0.0.1".to_string(),
            port: 3282,
            allowed_origins: Vec::new(),
            tls_cert_path: None,
            tls_key_path: None,
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: DEFAULT_SSE_CHANNEL_CAPACITY,
        }
    }
}

/// Authentication mode for downstream HTTP clients. `Auto` is bearer once
/// the server is reachable off this Mac and none before: see
/// [`HttpConfig::requires_bearer`].
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

impl HttpConfig {
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert_path.is_some() && self.tls_key_path.is_some()
    }

    /// Whether clients off this Mac can reach the server: it binds a
    /// non-loopback address, or `public_base_url` names another host (the
    /// tunnel topology, `cloudflared` to `127.0.0.1:3282`).
    pub fn reachable_off_loopback(&self) -> bool {
        let public_host_is_remote = self
            .public_base_url
            .as_deref()
            .and_then(|url| url::Url::parse(url).ok())
            .is_some_and(|url| !url.host_str().is_some_and(is_loopback_host));
        !is_loopback_host(&self.bind_address) || public_host_is_remote
    }

    /// Whether a client must present the bearer token: in `bearer` mode,
    /// and in `auto` mode once the server is reachable off this Mac.
    pub fn requires_bearer(&self) -> bool {
        match self.auth_mode {
            DownstreamAuthMode::Bearer => true,
            DownstreamAuthMode::Auto => self.reachable_off_loopback(),
            DownstreamAuthMode::None | DownstreamAuthMode::Oauth => false,
        }
    }

    /// The address a client is given to reach Plug over HTTP: the public one
    /// when there is one, else this Mac's own.
    pub fn mcp_url(&self) -> String {
        let base = self.public_base_url.clone().unwrap_or_else(|| {
            let scheme = if self.tls_enabled() { "https" } else { "http" };
            let host = match self.bind_address.as_str() {
                "0.0.0.0" | "::" | "[::]" => "localhost",
                bind if is_loopback_host(bind) => "localhost",
                bind => bind,
            };
            format!("{scheme}://{host}:{}", self.port)
        });
        format!("{}/mcp", base.trim_end_matches('/'))
    }

    /// Whether `url` reaches Plug itself: the address clients are given, or
    /// this Mac's listener under any loopback name or the address it binds.
    pub fn is_own_mcp_url(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        let path = |url: &url::Url| url.path().trim_end_matches('/').to_string();
        let is_given = url::Url::parse(&self.mcp_url()).is_ok_and(|given| {
            given.scheme() == url.scheme()
                && given.host_str() == url.host_str()
                && given.port_or_known_default() == url.port_or_known_default()
                && path(&given) == path(&url)
        });
        let is_listener = url.port_or_known_default() == Some(self.port)
            && path(&url) == "/mcp"
            && url
                .host_str()
                .is_some_and(|host| is_loopback_host(host) || host == self.bind_address);
        is_given || is_listener
    }
}

/// Configuration for a single upstream MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Command to execute (for stdio transport).
    pub command: Option<String>,
    /// Arguments for the command.
    pub args: Vec<String>,
    /// Environment variables to set for the child process.
    #[serde(serialize_with = "in_name_order")]
    pub env: HashMap<String, String>,
    /// Whether this server is enabled.
    pub enabled: bool,
    /// Transport type.
    pub transport: TransportType,
    /// MCP lifecycle negotiation policy for this upstream. Omitted is always
    /// `legacy`; `auto` and `modern` only take effect while the global modern
    /// upstream gate is enabled.
    #[serde(rename = "protocol", alias = "protocol_mode")]
    pub protocol_mode: UpstreamProtocolMode,
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
    pub oauth_scopes: Option<Vec<String>>,
    /// Startup timeout in seconds.
    pub timeout_secs: u64,
    /// Tool call timeout in seconds. Set higher for slow tools.
    pub call_timeout_secs: u64,
    /// Max concurrent requests to this server.
    pub max_concurrent: usize,
    /// Health check interval in seconds.
    pub health_check_interval_secs: u64,
    /// Enable circuit breaker for this server.
    pub circuit_breaker_enabled: bool,
    /// Manual tool renames (original_name -> new_name).
    #[serde(serialize_with = "in_name_order")]
    pub tool_renames: HashMap<String, String>,
    /// Tool group classification for sub-service decomposition.
    /// Maps group prefix (e.g. "Gmail") to match rules.
    /// When set, tools matching a rule get `GroupPrefix__tool_name` instead of `ServerName__tool_name`.
    pub tool_groups: Vec<ToolGroupRule>,
    /// Optional sandbox policy for stdio upstream processes.
    pub sandbox: Option<StdioSandboxConfig>,
    /// OpenAPI document for the openapi transport: a URL or a file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Operations of the OpenAPI document to expose, by tool name or
    /// `operationId`; `*` is a wildcard. Empty means all of them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    /// Where an openapi server sends `auth_token`: `bearer`,
    /// `header:<name>`, or `query:<name>`. Omitted, the OpenAPI document
    /// decides, and a bearer token is the fallback.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_in: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::default(),
            protocol_mode: UpstreamProtocolMode::default(),
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
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
            spec: None,
            operations: Vec::new(),
            token_in: None,
        }
    }
}

/// Stand-in the daemon sends in place of a stored secret. Operator clients hand
/// it back untouched when they save, and [`ServerConfig::restore_redacted_secrets`]
/// swaps the real value in again, so editing a server preserves credentials it
/// never received. The value is deliberately legible in a text field: an editor
/// that renders it shows the user that something is being kept, not lost.
pub const REDACTED_SECRET: &str =
    "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

impl ServerConfig {
    /// Whether Plug signs in to this server with OAuth. `oauth_client_id`
    /// alone does not make it so: the server starts without OAuth.
    pub fn is_oauth(&self) -> bool {
        self.auth.as_deref() == Some("oauth")
    }

    /// How Plug authenticates to this server: `oauth`, `bearer`, or `none`.
    pub fn auth_label(&self) -> &'static str {
        if self.is_oauth() {
            "oauth"
        } else if self.auth_token.is_some() {
            "bearer"
        } else {
            "none"
        }
    }

    /// Copy with credential material replaced by [`REDACTED_SECRET`].
    ///
    /// Environment values are redacted wholesale rather than by key-name
    /// heuristic, because a name-matching rule leaks every variable it fails to
    /// recognize; the keys stay in the clear since that is what an editor needs
    /// to render the list. `oauth_client_id` and `oauth_scopes` also stay: a
    /// client ID is a public identifier and scopes are names, while the OAuth
    /// secret lives in the credential store and never appears in this struct.
    #[must_use]
    pub fn redacted(&self) -> Self {
        let mut redacted = self.clone();
        redacted.auth_token = self
            .auth_token
            .as_ref()
            .map(|_| crate::types::SecretString::from(REDACTED_SECRET.to_string()));
        for value in redacted.env.values_mut() {
            REDACTED_SECRET.clone_into(value);
        }
        redacted
    }

    /// Swap [`REDACTED_SECRET`] placeholders back for the values `stored` holds.
    ///
    /// A placeholder with nothing stored behind it was never a real value, so it
    /// is dropped rather than written to disk.
    pub fn restore_redacted_secrets(&mut self, stored: Option<&Self>) {
        if self
            .auth_token
            .as_ref()
            .is_some_and(|token| token.as_str() == REDACTED_SECRET)
        {
            self.auth_token = stored.and_then(|stored| stored.auth_token.clone());
        }
        self.env.retain(|key, value| {
            if value != REDACTED_SECRET {
                return true;
            }
            match stored.and_then(|stored| stored.env.get(key)) {
                Some(original) => {
                    value.clone_from(original);
                    true
                }
                None => false,
            }
        });
    }
}

/// Opt-in sandbox policy for stdio upstream child processes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StdioSandboxConfig {
    pub enabled: bool,
    pub allow_network: bool,
    pub allow_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub profile_path: Option<PathBuf>,
}

/// Whether a host, as a bind address or a URL gives it, is this Mac only:
/// `localhost`, or a loopback IP with or without IPv6 brackets.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Transport type for upstream servers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    #[default]
    Stdio,
    Http,
    Sse,
    /// An HTTP API described by an OpenAPI document.
    OpenApi,
}

/// MCP lifecycle negotiation policy for one upstream server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamProtocolMode {
    /// Use only the legacy `initialize` lifecycle.
    #[default]
    Legacy,
    /// Probe `server/discover` and fall back only on JSON-RPC method-not-found.
    Auto,
    /// Require the modern `server/discover` lifecycle.
    Modern,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    errors.extend(config.secrets.validate());
    errors.extend(config.event_errors());

    if config.supervision.enabled {
        if config.supervision.degraded_restart_threshold == 0 {
            errors.push(
                "supervision.degraded_restart_threshold must be >= 1 when supervision is enabled"
                    .to_string(),
            );
        }
        if config.supervision.min_restart_interval_secs == 0 {
            errors.push(
                "supervision.min_restart_interval_secs must be >= 1 when supervision is enabled \
                 (0 disables the backoff entirely)"
                    .to_string(),
            );
        }
        if config.supervision.max_restart_interval_secs
            < config.supervision.min_restart_interval_secs
        {
            errors.push(
                "supervision.max_restart_interval_secs must be >= min_restart_interval_secs"
                    .to_string(),
            );
        }
    }

    if config.http.port == 0 {
        errors.push("http.port must be in range 1-65535".to_string());
    }
    if matches!(config.http.auth_mode, DownstreamAuthMode::Oauth)
        && config.http.public_base_url.is_none()
    {
        errors.push("http.public_base_url is required when http.auth_mode = \"oauth\"".to_string());
    }
    if matches!(config.http.auth_mode, DownstreamAuthMode::Oauth) {
        if let Some(public_base_url) = config.http.public_base_url.as_deref() {
            match url::Url::parse(public_base_url) {
                Ok(url)
                    if url.scheme() == "https"
                        && matches!(url.host(), Some(url::Host::Domain(_)))
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.path() == "/"
                        && url.query().is_none()
                        && url.fragment().is_none() => {}
                _ => errors.push(
                    "http.public_base_url must be an HTTPS origin with a domain hostname and without a path, credentials, query, or fragment when http.auth_mode = \"oauth\""
                        .to_string(),
                ),
            }
        }

        // An absent list inherits the default downstream grant, which always
        // contains tools:read; only an explicit list can drop it.
        if config
            .http
            .oauth_scopes
            .as_ref()
            .is_some_and(|scopes| !scopes.iter().any(|scope| scope == "tools:read"))
        {
            errors.push(
                "http.oauth_scopes must include \"tools:read\" when http.auth_mode = \"oauth\""
                    .to_string(),
            );
        }
    }
    if config.http.reachable_off_loopback()
        && matches!(config.http.auth_mode, DownstreamAuthMode::None)
    {
        errors.push(
            "http.auth_mode = \"none\" is not allowed when the server is reachable off-loopback (non-loopback bind or non-loopback public_base_url)"
                .to_string(),
        );
    }

    if !is_loopback_host(&config.http.bind_address) && config.http.tls_cert_path.is_none() {
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

        if let Err(e) = sanitize_server_name_for_path(name) {
            errors.push(format!("server '{name}': {e}"));
        }

        if server.is_oauth() {
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
        if let Some(ref auth) = server.auth
            && auth != "oauth"
        {
            errors.push(format!(
                "server '{name}': auth must be \"oauth\" if set (got \"{auth}\")"
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

        if server
            .sandbox
            .as_ref()
            .is_some_and(|sandbox| sandbox.enabled)
            && !matches!(server.transport, TransportType::Stdio)
        {
            errors.push(format!(
                "server '{name}': sandbox is only supported for stdio transport"
            ));
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
            TransportType::OpenApi => {
                if server.spec.is_none() {
                    errors.push(format!(
                        "server '{name}': openapi transport requires 'spec' to be set"
                    ));
                }
                if server.is_oauth() {
                    errors.push(format!(
                        "server '{name}': openapi transport takes auth_token, not auth = \"oauth\""
                    ));
                }
                if let Some(token_in) = &server.token_in
                    && let Err(error) = TokenPlace::parse(token_in)
                {
                    errors.push(format!("server '{name}': {error}"));
                }
            }
        }
    }

    errors
}

/// Visit every value `$NAME` expands in, mutably, with the server it belongs
/// to and the field it is in. Expansion and `plug doctor` both walk this one
/// list. `token_in` and `operations` are names, not values, so they are not
/// on it.
pub(crate) fn for_each_expandable(
    config: &mut Config,
    mut visit: impl FnMut(Option<&str>, &str, &mut String),
) {
    fn path(value: &mut PathBuf, mut visit: impl FnMut(&mut String)) {
        let mut text = value.to_string_lossy().into_owned();
        visit(&mut text);
        *value = PathBuf::from(text);
    }
    fn keys<V>(
        map: &mut HashMap<String, V>,
        field: &str,
        server: Option<&str>,
        visit: &mut impl FnMut(Option<&str>, &str, &mut String),
    ) {
        *map = map
            .drain()
            .map(|(mut key, value)| {
                visit(server, field, &mut key);
                (key, value)
            })
            .collect();
    }

    let mut top = |field: &str, value: &mut String| visit(None, field, value);
    top("log_level", &mut config.log_level);
    top("prefix_delimiter", &mut config.prefix_delimiter);
    config
        .priority_tools
        .iter_mut()
        .for_each(|v| top("priority_tools", v));
    config
        .disabled_tools
        .iter_mut()
        .for_each(|v| top("disabled_tools", v));
    let http = &mut config.http;
    top("http.bind_address", &mut http.bind_address);
    http.allowed_origins
        .iter_mut()
        .for_each(|v| top("http.allowed_origins", v));
    if let Some(cert) = &mut http.tls_cert_path {
        path(cert, |v| top("http.tls_cert_path", v));
    }
    if let Some(key) = &mut http.tls_key_path {
        path(key, |v| top("http.tls_key_path", v));
    }
    if let Some(url) = &mut http.public_base_url {
        top("http.public_base_url", url);
    }
    for scope in http.oauth_scopes.iter_mut().flatten() {
        top("http.oauth_scopes", scope);
    }
    keys(
        &mut config.lazy_tools.clients,
        "lazy_tools.clients.key",
        None,
        &mut visit,
    );

    for (name, server) in &mut config.servers {
        let name = Some(name.as_str());
        let mut at = |field: &str, value: &mut String| visit(name, field, value);
        for (field, value) in [
            ("command", &mut server.command),
            ("url", &mut server.url),
            ("auth", &mut server.auth),
            ("oauth_client_id", &mut server.oauth_client_id),
            ("spec", &mut server.spec),
        ] {
            if let Some(value) = value {
                at(field, value);
            }
        }
        server.args.iter_mut().for_each(|v| at("args", v));
        for (key, value) in &mut server.env {
            at(&format!("env.{key}"), value);
        }
        if let Some(token) = &mut server.auth_token {
            let mut text = token.as_str().to_string();
            at("auth_token", &mut text);
            *token = text.into();
        }
        for scope in server.oauth_scopes.iter_mut().flatten() {
            at("oauth_scopes", scope);
        }
        server
            .tool_renames
            .values_mut()
            .for_each(|v| at("tool_renames.value", v));
        for group in &mut server.tool_groups {
            at("tool_groups.prefix", &mut group.prefix);
            group
                .contains
                .iter_mut()
                .for_each(|v| at("tool_groups.contains", v));
            group
                .strip
                .iter_mut()
                .for_each(|v| at("tool_groups.strip", v));
        }
        if let Some(sandbox) = &mut server.sandbox {
            for (field, paths) in [
                ("sandbox.allow_read", &mut sandbox.allow_read),
                ("sandbox.allow_write", &mut sandbox.allow_write),
            ] {
                paths.iter_mut().for_each(|p| path(p, |v| at(field, v)));
            }
            if let Some(profile) = &mut sandbox.profile_path {
                path(profile, |v| at("sandbox.profile_path", v));
            }
        }
        keys(
            &mut server.tool_renames,
            "tool_renames.key",
            name,
            &mut visit,
        );
    }
}

impl Config {
    /// Replace each `$NAME` with its value in `vars`.
    pub fn expand_env_refs(&mut self, vars: &HashMap<String, String>) {
        for_each_expandable(self, |_, _, value| *value = env::expand(value, vars));
    }
}

/// What is wrong with a config as it is written to `config_path`: the
/// errors it would have once loaded. A `$NAME` in a path or address is
/// judged by its value, so a config that loads can always be saved.
pub fn validate_as_written(config: &Config, config_path: &Path) -> Vec<String> {
    let mut expanded = config.clone();
    expanded.expand_env_refs(&env::env_source(config_path));
    validate_config(&expanded)
}

/// The config as the file has it, `$NAME` unexpanded: the one to edit and
/// save, so a reference is never written back as its value.
#[allow(clippy::result_large_err)]
pub fn load_editable_config(path: &Path) -> Result<Config, figment::Error> {
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};

    if !path.exists() {
        return Ok(Config::default());
    }
    Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(path))
        .extract()
}

/// The config as Plug runs it: the file under `PLUG_` environment overrides,
/// with `$NAME` expanded, and valid.
#[allow(clippy::result_large_err)]
pub fn load_config(config_path: Option<&PathBuf>) -> Result<Config, figment::Error> {
    use figment::Figment;
    use figment::providers::{Env, Format, Serialized, Toml};

    let path = config_path
        .cloned()
        .unwrap_or_else(crate::paths::default_config_path);

    let mut config: Config = Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(&path))
        .merge(Env::prefixed("PLUG_").split("__"))
        .extract()?;
    config.expand_env_refs(&env::env_source(&path));

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

    #[test]
    fn a_saved_config_lists_servers_in_name_order() {
        let mut config = Config::default();
        for name in ["zulu", "alpha", "mike", "bravo", "yankee", "charlie"] {
            let server: ServerConfig =
                serde_json::from_value(serde_json::json!({"command": "echo"})).unwrap();
            config.servers.insert(name.to_string(), server);
        }
        let saved = toml::to_string_pretty(&config).unwrap();
        let order: Vec<usize> = ["alpha", "bravo", "charlie", "mike", "yankee", "zulu"]
            .iter()
            .map(|name| saved.find(&format!("[servers.{name}]")).unwrap())
            .collect();
        assert!(order.is_sorted(), "{saved}");
    }
    use figment::providers::{Format, Serialized, Toml};
    use std::path::PathBuf;

    fn server_with_secrets() -> ServerConfig {
        toml::from_str(
            r#"
transport = "http"
url = "https://example.test/mcp"
auth_token = "bearer-abc"
oauth_client_id = "public-client-id"

[env]
API_KEY = "sk-live-123"
LOG_LEVEL = "debug"
"#,
        )
        .unwrap()
    }

    #[test]
    fn redaction_covers_every_env_value_and_the_bearer_token() {
        let redacted = server_with_secrets().redacted();
        assert_eq!(
            redacted.auth_token.as_ref().map(|token| token.as_str()),
            Some(REDACTED_SECRET)
        );
        // Every value, not the ones a name heuristic would have guessed at.
        assert_eq!(redacted.env["API_KEY"], REDACTED_SECRET);
        assert_eq!(redacted.env["LOG_LEVEL"], REDACTED_SECRET);
        // Identifiers an editor needs are not credential material.
        assert_eq!(redacted.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(
            redacted.oauth_client_id.as_deref(),
            Some("public-client-id")
        );
        assert_eq!(
            redacted
                .env
                .keys()
                .collect::<std::collections::BTreeSet<_>>(),
            ["API_KEY".to_string(), "LOG_LEVEL".to_string()]
                .iter()
                .collect()
        );
    }

    #[test]
    fn a_server_without_secrets_gains_no_placeholder() {
        let mut plain = server_with_secrets();
        plain.auth_token = None;
        plain.env.clear();
        let redacted = plain.redacted();
        assert!(redacted.auth_token.is_none());
        assert!(redacted.env.is_empty());
    }

    #[test]
    fn restoring_an_edited_redaction_keeps_the_stored_secrets() {
        let stored = server_with_secrets();
        let mut edited = stored.redacted();
        edited.url = Some("https://example.test/v2".to_string());
        edited.restore_redacted_secrets(Some(&stored));

        assert_eq!(
            edited.auth_token.as_ref().map(|token| token.as_str()),
            Some("bearer-abc")
        );
        assert_eq!(edited.env["API_KEY"], "sk-live-123");
        assert_eq!(edited.env["LOG_LEVEL"], "debug");
        assert_eq!(edited.url.as_deref(), Some("https://example.test/v2"));
    }

    #[test]
    fn a_typed_value_wins_over_the_stored_secret() {
        let stored = server_with_secrets();
        let mut edited = stored.redacted();
        edited.auth_token = Some(crate::types::SecretString::from("bearer-new".to_string()));
        edited
            .env
            .insert("API_KEY".to_string(), "sk-live-456".to_string());
        edited.restore_redacted_secrets(Some(&stored));

        assert_eq!(
            edited.auth_token.as_ref().map(|token| token.as_str()),
            Some("bearer-new")
        );
        assert_eq!(edited.env["API_KEY"], "sk-live-456");
    }

    #[test]
    fn a_placeholder_with_nothing_stored_behind_it_is_dropped() {
        let mut edited = server_with_secrets().redacted();
        edited
            .env
            .insert("INVENTED".to_string(), REDACTED_SECRET.to_string());
        edited.restore_redacted_secrets(None);

        assert!(edited.auth_token.is_none());
        assert!(edited.env.is_empty());
    }

    #[test]
    fn supervision_config_validation_rejects_bad_thresholds() {
        let mut config = Config::default();
        config.supervision.degraded_restart_threshold = 0;
        config.supervision.max_restart_interval_secs = 10;
        config.supervision.min_restart_interval_secs = 60;
        let errors = validate_config(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("degraded_restart_threshold"))
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("max_restart_interval_secs"))
        );
    }

    #[test]
    fn supervision_config_validation_rejects_zero_min_interval() {
        // A zero floor would disable the backoff entirely (0 * 2^n = 0) and let a
        // degraded upstream restart on every health tick.
        let mut config = Config::default();
        config.supervision.min_restart_interval_secs = 0;
        let errors = validate_config(&config);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("min_restart_interval_secs")),
            "zero min interval must be rejected: {errors:?}"
        );
    }

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
            cfg.servers
                .get("demo")
                .and_then(|server| server.url.as_deref()),
            Some("https://env.example.com/mcp")
        );
    }

    #[test]
    fn a_config_with_tls_paths_under_a_name_can_be_saved_as_written() {
        let dir = unique_temp_dir("tls-by-name");
        let config_path = dir.join("config.toml");
        std::fs::write(dir.join("cert.pem"), "cert").unwrap();
        std::fs::write(dir.join("key.pem"), "key").unwrap();
        std::fs::set_permissions(
            dir.join("key.pem"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(
            dir.join(".env"),
            format!("PLUG_TEST_TLS_DIR={}\n", dir.display()),
        )
        .unwrap();
        std::fs::write(
            &config_path,
            r#"
            [http]
            tls_cert_path = "$PLUG_TEST_TLS_DIR/cert.pem"
            tls_key_path = "$PLUG_TEST_TLS_DIR/key.pem"
            "#,
        )
        .unwrap();

        let written = load_editable_config(&config_path).unwrap();
        assert!(
            !validate_config(&written).is_empty(),
            "the literal path does not exist"
        );
        assert_eq!(
            validate_as_written(&written, &config_path),
            Vec::<String>::new()
        );
        assert!(load_config(Some(&config_path)).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A server's `enrichment` setting is gone; a file that still sets it
    /// loads.
    #[test]
    fn a_server_setting_the_retired_enrichment_still_loads() {
        let config = config_from_toml(
            r#"
            [servers.github]
            command = "github-mcp"
            enrichment = true
            "#,
        );
        assert!(config.servers.contains_key("github"));
    }

    /// The idle self-shutdown knob is gone; a file that still sets it loads.
    #[test]
    fn a_file_setting_the_retired_grace_period_still_loads() {
        let dir = unique_temp_dir("retired-grace-period");
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "daemon_grace_period_secs = 30\n").unwrap();
        assert!(load_config(Some(&config_path)).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn spec_and_sandbox_paths_expand() {
        let mut cfg = config_from_toml(
            r#"
            [servers.api]
            transport = "openapi"
            spec = "$SPEC_URL"

            [servers.local]
            command = "tool"
            sandbox = { enabled = true, allow_read = ["$DATA/in"], profile_path = "$DATA/p.sb" }
            "#,
        );
        let vars = HashMap::from([
            (
                "SPEC_URL".to_string(),
                "https://api.test/openapi.json".to_string(),
            ),
            ("DATA".to_string(), "/data".to_string()),
        ]);
        cfg.expand_env_refs(&vars);
        assert_eq!(
            cfg.servers["api"].spec.as_deref(),
            Some("https://api.test/openapi.json")
        );
        let sandbox = cfg.servers["local"].sandbox.as_ref().unwrap();
        assert_eq!(sandbox.allow_read, vec![PathBuf::from("/data/in")]);
        assert_eq!(sandbox.profile_path, Some(PathBuf::from("/data/p.sb")));
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
        assert_eq!(srv.protocol_mode, UpstreamProtocolMode::Legacy);
        assert!(!cfg.modern_upstream_enabled);
    }

    #[test]
    fn modern_upstream_protocol_requires_explicit_global_and_server_opt_in() {
        let cfg = config_from_toml(
            r#"
            modern_upstream_enabled = true

            [servers.modern]
            command = "modern-server"
            protocol = "modern"

            [servers.auto]
            command = "auto-server"
            protocol_mode = "auto"
            "#,
        );

        assert!(cfg.modern_upstream_enabled);
        assert_eq!(
            cfg.servers["modern"].protocol_mode,
            UpstreamProtocolMode::Modern
        );
        assert_eq!(
            cfg.servers["auto"].protocol_mode,
            UpstreamProtocolMode::Auto
        );
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
    #[allow(clippy::result_large_err)] // figment::Jail returns figment::Error.
    fn a_plug_variable_beats_the_file() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                "tool_description_max_chars = 7\nstartup_concurrency = 3\n",
            )?;
            jail.set_env("PLUG_TOOL_DESCRIPTION_MAX_CHARS", "9");
            let config = load_config(Some(&jail.directory().join("config.toml")))?;
            assert_eq!(config.tool_description_max_chars, Some(9));
            assert_eq!(config.startup_concurrency, 3);
            Ok(())
        });
    }

    // ---- validation ----

    #[test]
    fn validate_valid_config() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "ok".to_string(),
            ServerConfig {
                command: Some("node".to_string()),
                ..Default::default()
            },
        );
        let errors = validate_config(&cfg);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn validate_sandbox_only_applies_to_stdio_servers() {
        let cfg = config_from_toml(
            r#"
            [servers.remote]
            transport = "http"
            url = "https://example.com/mcp"

            [servers.remote.sandbox]
            enabled = true
            allow_network = false
            "#,
        );

        let errors = validate_config(&cfg);

        assert!(
            errors
                .iter()
                .any(|error| error.contains("sandbox") && error.contains("stdio transport")),
            "expected sandbox/stdout validation error, got {errors:?}"
        );
    }

    #[test]
    fn validate_stdio_without_command() {
        let mut cfg = Config::default();
        cfg.servers
            .insert("bad".to_string(), ServerConfig::default());
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
                transport: TransportType::Http,
                ..Default::default()
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
                timeout_secs: 0,
                ..Default::default()
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
    fn validate_oauth_is_valid_with_https_issuer_and_tools_scope() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        let errors = validate_config(&cfg);
        assert!(
            errors.is_empty(),
            "expected multi-client oauth config to be valid, got {errors:?}"
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
    fn validate_oauth_requires_tools_read_scope() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_scopes = Some(vec!["resources:read".to_string()]);
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("oauth_scopes") && e.contains("tools:read")),
            "expected oauth tools:read validation error, got {errors:?}"
        );
    }

    #[test]
    fn validate_oauth_accepts_absent_scope_list_as_default_grant() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_scopes = None;
        let errors = validate_config(&cfg);
        assert!(
            errors.is_empty(),
            "expected absent oauth_scopes to inherit the default grant, got {errors:?}"
        );
    }

    #[test]
    fn validate_oauth_requires_https_public_base_url() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.public_base_url = Some("http://127.0.0.1:3282".to_string());
        cfg.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        let errors = validate_config(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("HTTPS origin")),
            "expected oauth HTTPS issuer validation error, got {errors:?}"
        );
    }

    #[test]
    fn load_config_rejects_oauth_ip_literal_public_base_urls_before_startup() {
        for (label, public_base_url) in [
            ("ipv4", "https://127.0.0.1:3282"),
            ("ipv6", "https://[::1]:3282"),
        ] {
            let temp = unique_temp_dir(label);
            let path = temp.join("config.toml");
            std::fs::write(
                &path,
                format!(
                    r#"
                    [http]
                    auth_mode = "oauth"
                    public_base_url = "{public_base_url}"
                    oauth_scopes = ["tools:read"]
                    "#
                ),
            )
            .expect("write config fixture");

            let error = load_config(Some(&path)).expect_err("IP issuer must fail config loading");
            let message = error.to_string();
            assert!(
                message.contains("http.public_base_url") && message.contains("domain hostname"),
                "invalid setting must be named for {label}: {message}"
            );

            std::fs::remove_dir_all(temp).expect("remove config fixture");
        }
    }

    #[test]
    fn removed_singular_downstream_oauth_keys_are_rejected() {
        let result = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Toml::string(
                r#"
                [http]
                oauth_client_id = "legacy-client"
                oauth_client_secret = "legacy-secret"
                "#,
            ))
            .extract::<Config>();
        assert!(
            result.is_err(),
            "removed singular OAuth keys must not be ignored"
        );
    }

    #[test]
    fn validate_none_on_non_loopback_is_rejected() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::None;
        cfg.http.bind_address = "0.0.0.0".to_string();
        cfg.http.tls_cert_path = Some(PathBuf::from("Cargo.toml"));
        cfg.http.tls_key_path = Some(PathBuf::from("Cargo.toml"));
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("http.auth_mode = \"none\"") && e.contains("non-loopback")),
            "expected none/non-loopback validation error, got {errors:?}"
        );
    }

    #[test]
    fn auto_mode_wants_a_bearer_once_the_server_is_reachable_off_this_mac() {
        let http = |bind: &str, public: Option<&str>| HttpConfig {
            bind_address: bind.to_string(),
            public_base_url: public.map(str::to_string),
            ..HttpConfig::default()
        };
        for (bind, public, off) in [
            ("127.0.0.1", None, false),
            ("::1", Some("http://[::1]:3282"), false),
            ("127.0.0.2", Some("http://localhost:3282"), false),
            ("127.0.0.1", Some("https://plug.example.com"), true),
            ("0.0.0.0", None, true),
        ] {
            let http = http(bind, public);
            assert_eq!(http.reachable_off_loopback(), off, "{bind} {public:?}");
            assert_eq!(http.requires_bearer(), off, "{bind} {public:?}");
        }
        let bearer = HttpConfig {
            auth_mode: DownstreamAuthMode::Bearer,
            ..HttpConfig::default()
        };
        assert!(bearer.requires_bearer());
    }

    #[test]
    fn validate_none_on_tunneled_loopback_bind_is_rejected() {
        // The real tunnel topology: cloudflared -> 127.0.0.1:3282, but the
        // server is reachable off-host via a non-loopback public_base_url.
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::None;
        cfg.http.bind_address = "127.0.0.1".to_string();
        cfg.http.public_base_url = Some("https://plug.example.trycloudflare.com".to_string());
        let errors = validate_config(&cfg);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("http.auth_mode = \"none\"") && e.contains("off-loopback")),
            "expected none/tunneled-loopback validation error, got {errors:?}"
        );
    }

    #[test]
    fn validate_none_on_loopback_without_public_base_url_is_valid() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::None;
        let errors = validate_config(&cfg);
        assert!(
            !errors
                .iter()
                .any(|e| e.contains("http.auth_mode = \"none\"")),
            "expected no auth_mode=none validation error for pure loopback, got {errors:?}"
        );
    }

    #[test]
    fn validate_public_oauth_on_non_loopback_is_allowed_with_tls() {
        let key_path =
            std::env::temp_dir().join(format!("plug-config-key-{}", uuid::Uuid::new_v4()));
        std::fs::write(&key_path, "test key").expect("write test key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .expect("secure test key");
        }
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.bind_address = "0.0.0.0".to_string();
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        cfg.http.tls_cert_path = Some(key_path.clone());
        cfg.http.tls_key_path = Some(key_path.clone());
        let errors = validate_config(&cfg);
        let _ = std::fs::remove_file(key_path);
        assert!(
            errors.is_empty(),
            "public registered clients with PKCE should be valid, got {errors:?}"
        );
    }

    #[test]
    fn validate_public_oauth_on_tunneled_loopback_bind_is_allowed() {
        let mut cfg = Config::default();
        cfg.http.auth_mode = DownstreamAuthMode::Oauth;
        cfg.http.bind_address = "127.0.0.1".to_string();
        cfg.http.public_base_url = Some("https://plug.example.com".to_string());
        cfg.http.oauth_scopes = Some(vec!["tools:read".to_string()]);
        let errors = validate_config(&cfg);
        assert!(
            errors.is_empty(),
            "tunneled public-client oauth should be allowed, got {errors:?}"
        );
    }

    #[test]
    fn mcp_url_prefers_public_base_url() {
        let mut cfg = Config::default();
        cfg.http.public_base_url = Some("https://plug.example.com/base/".to_string());
        assert_eq!(cfg.http.mcp_url(), "https://plug.example.com/base/mcp");
    }

    #[test]
    fn mcp_url_falls_back_to_this_macs_listener() {
        let mut cfg = Config::default();
        cfg.http.port = 4411;
        assert_eq!(cfg.http.mcp_url(), "http://localhost:4411/mcp");
        cfg.http.bind_address = "0.0.0.0".to_string();
        cfg.http.tls_cert_path = Some(PathBuf::from("cert.pem"));
        cfg.http.tls_key_path = Some(PathBuf::from("key.pem"));
        assert_eq!(cfg.http.mcp_url(), "https://localhost:4411/mcp");
        cfg.http.bind_address = "192.168.1.5".to_string();
        assert_eq!(cfg.http.mcp_url(), "https://192.168.1.5:4411/mcp");
    }

    #[test]
    fn plug_knows_its_own_addresses() {
        let mut cfg = Config::default();
        cfg.http.public_base_url = Some("https://plug.example.com/base/".to_string());
        for own in [
            "https://plug.example.com/base/mcp",
            "http://localhost:3282/mcp",
            "http://127.0.0.1:3282/mcp/",
            "http://[::1]:3282/mcp",
        ] {
            assert!(cfg.http.is_own_mcp_url(own), "{own}");
        }
        for other in [
            "https://plug.example.com/mcp",
            "http://localhost:3283/mcp",
            "http://localhost:3282/other",
            "http://192.168.1.5:3282/mcp",
            "not a url",
        ] {
            assert!(!cfg.http.is_own_mcp_url(other), "{other}");
        }
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
                ..Default::default()
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
                timeout_secs: 0,
                ..Default::default()
            },
        );
        let mut errors = validate_config(&cfg);
        errors.sort();
        assert_eq!(
            errors,
            [
                "http.port must be in range 1-65535",
                "server 'bad_stdio': stdio transport requires 'command' to be set",
                "server 'bad_stdio': timeout must be > 0",
                "startup_concurrency must be > 0",
            ]
        );
    }

    #[test]
    fn validate_oauth_mutual_exclusion_with_auth_token() {
        let mut cfg = Config::default();
        cfg.servers.insert(
            "bad".to_string(),
            ServerConfig {
                transport: TransportType::Http,
                url: Some("https://example.com/mcp".to_string()),
                auth_token: Some(crate::types::SecretString::from("token".to_string())),
                auth: Some("oauth".to_string()),
                ..Default::default()
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
                auth: Some("oauth".to_string()),
                ..Default::default()
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
                transport: TransportType::Http,
                url: Some("https://example.com/mcp".to_string()),
                auth: Some("basic".to_string()),
                ..Default::default()
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
                ..Default::default()
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
                transport: TransportType::Http,
                url: Some("https://mcp.notion.so/mcp".to_string()),
                auth: Some("oauth".to_string()),
                oauth_scopes: Some(vec!["mcp:read".to_string(), "mcp:write".to_string()]),
                ..Default::default()
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
