use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use rmcp::model::Icon;

/// A string that redacts its value in Debug output to prevent secret leakage.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Unique identifier for a client session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::SecretString;

    #[test]
    fn secret_string_debug_is_redacted() {
        let secret = SecretString::from("super-secret".to_string());
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
    }

    #[test]
    fn secret_string_display_is_redacted() {
        let secret = SecretString::from("super-secret".to_string());
        assert_eq!(format!("{secret}"), "[REDACTED]");
    }

    #[test]
    fn auth_required_is_sticky_on_success() {
        use super::{HealthState, ServerHealth};
        let mut state = HealthState {
            health: ServerHealth::AuthRequired,
            consecutive_failures: 0,
        };
        let changed = state.record_success();
        assert!(!changed);
        assert_eq!(state.health, ServerHealth::AuthRequired);
    }

    #[test]
    fn auth_required_is_sticky_on_failure() {
        use super::{HealthState, ServerHealth};
        let mut state = HealthState {
            health: ServerHealth::AuthRequired,
            consecutive_failures: 0,
        };
        let changed = state.record_failure();
        assert!(!changed);
        assert_eq!(state.health, ServerHealth::AuthRequired);
    }

    #[test]
    fn auth_required_is_not_routable() {
        use super::ServerHealth;
        assert!(!ServerHealth::AuthRequired.is_routable());
    }

    #[test]
    fn availability_serializes_lowercase() {
        use super::Availability;
        assert_eq!(
            serde_json::to_value(Availability::Healthy).unwrap(),
            serde_json::json!("healthy")
        );
        assert_eq!(
            serde_json::to_value(Availability::Degraded).unwrap(),
            serde_json::json!("degraded")
        );
        assert_eq!(
            serde_json::to_value(Availability::Absent).unwrap(),
            serde_json::json!("absent")
        );
    }

    #[test]
    fn availability_default_is_healthy() {
        use super::Availability;
        assert_eq!(Availability::default(), Availability::Healthy);
    }

    #[test]
    fn server_status_missing_availability_deserializes_as_healthy() {
        use super::{Availability, ServerHealth, ServerStatus};
        // An older daemon's JSON without the `availability` key must still
        // deserialize (schema stability), defaulting to healthy.
        let json = serde_json::json!({
            "server_id": "legacy",
            "health": "Healthy",
            "tool_count": 3,
            "auth_status": "none"
        });
        let status: ServerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status.availability, Availability::Healthy);
        assert_eq!(status.health, ServerHealth::Healthy);
    }

    #[test]
    fn server_status_roundtrips_degraded_availability() {
        use super::{Availability, ServerHealth, ServerStatus};
        let status = ServerStatus {
            server_id: "imessage".to_string(),
            health: ServerHealth::Healthy,
            tool_count: 1,
            auth_status: "none".to_string(),
            upstream: None,
            metrics: None,
            availability: Availability::Degraded,
            last_seen: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["availability"], serde_json::json!("degraded"));
        let back: ServerStatus = serde_json::from_value(value).unwrap();
        assert_eq!(back.availability, Availability::Degraded);
    }
}

/// Known AI client types that connect to plug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientType {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Windsurf,
    VSCodeCopilot,
    GeminiCli,
    CodexCli,
    OpenCode,
    Zed,
    Unknown,
}

impl ClientType {
    /// Stable config/export target slug for known clients.
    pub fn target_slug(&self) -> Option<&'static str> {
        match self {
            ClientType::ClaudeCode => Some("claude-code"),
            ClientType::ClaudeDesktop => Some("claude-desktop"),
            ClientType::Cursor => Some("cursor"),
            ClientType::Windsurf => Some("windsurf"),
            ClientType::VSCodeCopilot => Some("vscode"),
            ClientType::GeminiCli => Some("gemini-cli"),
            ClientType::CodexCli => Some("codex-cli"),
            ClientType::OpenCode => Some("opencode"),
            ClientType::Zed => Some("zed"),
            ClientType::Unknown => None,
        }
    }

    /// Returns the maximum number of tools this client supports, if known.
    pub fn tool_limit(&self) -> Option<usize> {
        match self {
            ClientType::Windsurf => Some(100),
            ClientType::VSCodeCopilot => Some(128),
            _ => None,
        }
    }
}

/// Operator-facing lazy discovery setting stored in config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LazyToolSetting {
    /// Let plug choose from the client capability matrix.
    #[default]
    Auto,
    /// Expose the full normal routed tool catalog, subject to existing client limits.
    Standard,
    /// Let the downstream client use its own native lazy/deferred tool mechanism.
    Native,
    /// Use plug's bridge tools to search, load, evict, then direct-call loaded tools.
    Bridge,
}

impl LazyToolSetting {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Standard => "standard",
            Self::Native => "native",
            Self::Bridge => "bridge",
        }
    }
}

/// Concrete lazy discovery mode after defaults and overrides are resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LazyToolMode {
    Standard,
    Native,
    Bridge,
}

impl LazyToolMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Native => "native",
            Self::Bridge => "bridge",
        }
    }
}

/// Why a lazy discovery mode was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LazyToolModeOrigin {
    ClientOverride,
    GlobalOverride,
    LegacyMetaToolMode,
    AutoDefault,
}

impl LazyToolModeOrigin {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClientOverride => "client_override",
            Self::GlobalOverride => "global_override",
            Self::LegacyMetaToolMode => "legacy_meta_tool_mode",
            Self::AutoDefault => "auto_default",
        }
    }
}

/// Resolved lazy discovery policy for a client target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLazyToolPolicy {
    pub mode: LazyToolMode,
    pub origin: LazyToolModeOrigin,
    pub reason: String,
}

impl ResolvedLazyToolPolicy {
    pub fn new(mode: LazyToolMode, origin: LazyToolModeOrigin, reason: impl Into<String>) -> Self {
        Self {
            mode,
            origin,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for ClientType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ClientType::ClaudeCode => "Claude Code",
            ClientType::ClaudeDesktop => "Claude Desktop",
            ClientType::Cursor => "Cursor",
            ClientType::Windsurf => "Windsurf",
            ClientType::VSCodeCopilot => "VS Code Copilot",
            ClientType::GeminiCli => "Gemini CLI",
            ClientType::CodexCli => "Codex CLI",
            ClientType::OpenCode => "OpenCode",
            ClientType::Zed => "Zed",
            ClientType::Unknown => "Unknown",
        };
        write!(f, "{name}")
    }
}

/// Health state of an upstream server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerHealth {
    /// Server is responding normally.
    Healthy,
    /// Server is responding but with degraded performance (timeouts, partial failures).
    Degraded,
    /// Server is not responding.
    Failed,
    /// OAuth credentials missing or refresh failed. Server awaits re-auth.
    AuthRequired,
}

impl ServerHealth {
    /// Returns true for health states that should participate in tool/resource/prompt routing.
    pub fn is_routable(&self) -> bool {
        matches!(self, ServerHealth::Healthy | ServerHealth::Degraded)
    }
}

/// Catalog availability of an upstream, distinct from connection health.
///
/// Where [`ServerHealth`] tracks the connection's health-check state, `Availability`
/// describes what the merged catalog currently serves for this upstream:
///
/// - `Healthy`: the last catalog refresh listed this upstream's resources/prompts live.
/// - `Degraded`: the last refresh failed to list (timeout/error) but the upstream is
///   still routable, so its last-known-good catalog entries are being carried forward
///   (and its resource subscriptions are preserved, not pruned).
/// - `Absent`: the upstream contributes nothing to the merged catalog — removed from
///   config, or failing with no last-known-good to carry forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    #[default]
    Healthy,
    Degraded,
    Absent,
}

/// Tracked health state with consecutive failure counting for state machine transitions.
///
/// State machine:
/// - Healthy → 3 consecutive failures → Degraded
/// - Degraded → 3 more failures → Failed
/// - Failed → 1 success → Degraded
/// - Degraded → 1 success → Healthy
#[derive(Debug, Clone)]
pub struct HealthState {
    pub health: ServerHealth,
    pub consecutive_failures: u32,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            health: ServerHealth::Healthy,
            consecutive_failures: 0,
        }
    }

    /// Record a successful health check. Returns true if health state changed.
    pub fn record_success(&mut self) -> bool {
        self.consecutive_failures = 0;
        let old = self.health;
        self.health = match old {
            ServerHealth::Healthy => ServerHealth::Healthy,
            ServerHealth::Degraded => ServerHealth::Healthy,
            ServerHealth::Failed => ServerHealth::Degraded,
            ServerHealth::AuthRequired => ServerHealth::AuthRequired, // sticky
        };
        old != self.health
    }

    /// Record a failed health check. Returns true if health state changed.
    pub fn record_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        let old = self.health;
        self.health = match old {
            ServerHealth::Healthy => {
                if self.consecutive_failures >= 3 {
                    ServerHealth::Degraded
                } else {
                    ServerHealth::Healthy
                }
            }
            ServerHealth::Degraded => {
                if self.consecutive_failures >= 6 {
                    ServerHealth::Failed
                } else {
                    ServerHealth::Degraded
                }
            }
            ServerHealth::Failed => ServerHealth::Failed,
            ServerHealth::AuthRequired => ServerHealth::AuthRequired, // sticky
        };
        old != self.health
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-upstream operability metrics, surfaced to operators via
/// `plug status --output json`. Read-side only — nothing acts on these.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UpstreamMetricsSnapshot {
    /// Total tool calls routed to this upstream since start.
    pub call_count: u64,
    /// Of those, how many failed (error or timeout).
    pub error_count: u64,
    /// Latency of the most recent call, in milliseconds.
    pub last_latency_ms: u64,
    /// Unix epoch seconds since which this upstream has been failing; serialized
    /// as `null` when the last call succeeded (i.e. currently healthy) — the key
    /// is always present so the JSON schema is stable for agent consumers.
    #[serde(default)]
    pub degraded_since_epoch_secs: Option<u64>,
    /// Circuit-breaker state: `"closed"`, `"open"`, or `"half-open"`.
    pub circuit_state: String,
}

/// Status information for an upstream server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub server_id: String,
    pub health: ServerHealth,
    pub tool_count: usize,
    /// Auth mechanism in use: `"bearer"`, `"oauth"`, `"auth-required"`, or `"none"`.
    #[serde(default = "default_auth_status")]
    pub auth_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamServerMetadata>,
    /// Per-upstream operability metrics (calls, errors, latency, circuit,
    /// degraded-since). Always present for a configured live server (zero-valued
    /// before its first call); `null` only for a server no longer in the routed
    /// set. The key is always present so the agent-facing schema is stable.
    #[serde(default)]
    pub metrics: Option<UpstreamMetricsSnapshot>,
    /// Catalog availability — whether the merged catalog entries for this upstream
    /// are live (`healthy`), carried-forward last-known-good after a failed listing
    /// (`degraded`), or absent. Additive with a default so the agent-facing schema
    /// stays stable: an older daemon's JSON without the key deserializes as `healthy`.
    #[serde(default)]
    pub availability: Availability,
    #[serde(skip)]
    pub last_seen: Option<std::time::Instant>,
}

fn default_auth_status() -> String {
    "none".to_string()
}

/// Sanitized upstream implementation metadata captured from MCP initialize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UpstreamServerMetadata {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
}
