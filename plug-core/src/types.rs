use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use rmcp::model::Icon;
use rmcp::model::MetaObject;

/// The only peer-controlled MCP `_meta` data Plug will retain or forward.
///
/// This is deliberately a one-way wire envelope, not a general map exposed to
/// policy code.  Authentication, ownership, routing, credentials, and
/// continuation state therefore cannot accidentally start consulting an
/// extension field in a later refactor.
#[derive(Clone, PartialEq)]
pub struct ExtensionEnvelope {
    meta: MetaObject,
    encoded_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionEnvelopeError {
    InvalidKey,
    ReservedNamespace,
    ControlShapedKey,
    SecretShapedValue,
    TooDeep,
    TooManyValues,
    TooLarge,
}

impl fmt::Display for ExtensionEnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::InvalidKey => "extension metadata contains an invalid key",
            Self::ReservedNamespace => "extension metadata uses Plug's reserved namespace",
            Self::ControlShapedKey => {
                "extension metadata uses an authentication or routing control key"
            }
            Self::SecretShapedValue => "extension metadata contains a secret-shaped value",
            Self::TooDeep => "extension metadata exceeds the nesting limit",
            Self::TooManyValues => "extension metadata exceeds the value-count limit",
            Self::TooLarge => "extension metadata exceeds the byte limit",
        };
        f.write_str(reason)
    }
}

impl std::error::Error for ExtensionEnvelopeError {}

impl fmt::Debug for ExtensionEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtensionEnvelope")
            .field("field_count", &self.meta.len())
            .field("encoded_bytes", &self.encoded_bytes)
            .field("values", &"[REDACTED]")
            .finish()
    }
}

impl ExtensionEnvelope {
    pub const MAX_ENCODED_BYTES: usize = 16 * 1024;
    pub const MAX_DEPTH: usize = 8;
    pub const MAX_VALUES: usize = 128;

    /// Admit extension fields from a peer-owned `_meta` map.
    ///
    /// MCP core fields are intentionally omitted: RMCP and Plug's typed
    /// protocol adapters own them. Unknown fields must be namespaced.
    pub fn from_peer_meta(meta: Option<&MetaObject>) -> Result<Self, ExtensionEnvelopeError> {
        Self::from_peer_meta_with_reserved_policy(meta, false)
    }

    /// Catalog/result ingress strips peer attempts to claim Plug's namespace,
    /// while retaining other independently valid fields. Request ingress uses
    /// [`Self::from_peer_meta`] and rejects the same collision before effects.
    pub fn from_peer_catalog_meta(
        meta: Option<&MetaObject>,
    ) -> Result<Self, ExtensionEnvelopeError> {
        Self::from_peer_meta_with_reserved_policy(meta, true)
    }

    fn from_peer_meta_with_reserved_policy(
        meta: Option<&MetaObject>,
        strip_reserved: bool,
    ) -> Result<Self, ExtensionEnvelopeError> {
        let mut admitted = serde_json::Map::new();
        let Some(meta) = meta else {
            return Ok(Self {
                meta: MetaObject::new(),
                encoded_bytes: 2,
            });
        };

        let mut value_count = 0;
        for (key, value) in meta.iter() {
            if is_typed_mcp_meta_key(key) {
                if matches!(key.as_str(), "traceparent" | "tracestate" | "baggage") {
                    validate_trace_value(key, value)?;
                    admitted.insert(key.clone(), value.clone());
                }
                continue;
            }
            if strip_reserved && is_reserved_extension_key(key) {
                continue;
            }
            validate_extension_key(key)?;
            if matches!(
                key.as_str(),
                "io.modelcontextprotocol/ui" | "io.modelcontextprotocol/apps"
            ) && !value.is_object()
            {
                return Err(ExtensionEnvelopeError::InvalidKey);
            }
            validate_value(value, 1, &mut value_count)?;
            admitted.insert(key.clone(), value.clone());
        }

        let encoded_bytes = serde_json::to_vec(&admitted)
            .map_err(|_| ExtensionEnvelopeError::TooLarge)?
            .len();
        if encoded_bytes > Self::MAX_ENCODED_BYTES {
            return Err(ExtensionEnvelopeError::TooLarge);
        }
        Ok(Self {
            meta: MetaObject::from(admitted),
            encoded_bytes,
        })
    }

    pub fn into_meta(self) -> Option<MetaObject> {
        (!self.meta.is_empty()).then_some(self.meta)
    }

    pub fn to_meta(&self) -> Option<MetaObject> {
        (!self.meta.is_empty()).then(|| self.meta.clone())
    }

    /// Exact serialized size used by aggregate retention budgets after the
    /// envelope has applied key, value, depth, and secret-shape policy.
    pub(crate) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
}

fn is_typed_mcp_meta_key(key: &str) -> bool {
    matches!(
        key,
        "progressToken"
            | "traceparent"
            | "tracestate"
            | "baggage"
            | "io.modelcontextprotocol/protocolVersion"
            | "io.modelcontextprotocol/clientInfo"
            | "io.modelcontextprotocol/clientCapabilities"
            | "io.modelcontextprotocol/logLevel"
            | "plug.dev/legacy-task"
    )
}

fn validate_extension_key(key: &str) -> Result<(), ExtensionEnvelopeError> {
    if is_reserved_extension_key(key) {
        return Err(ExtensionEnvelopeError::ReservedNamespace);
    }
    let Some((namespace, name)) = key.split_once('/') else {
        return Err(ExtensionEnvelopeError::InvalidKey);
    };
    if namespace.is_empty()
        || name.is_empty()
        || name.contains('/')
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(ExtensionEnvelopeError::InvalidKey);
    }
    if control_shaped_extension_name(name) {
        return Err(ExtensionEnvelopeError::ControlShapedKey);
    }
    Ok(())
}

fn control_shaped_extension_name(name: &str) -> bool {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in name.chars() {
        if matches!(character, '.' | '_' | '-') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if character.is_ascii_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(character.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }

    if words.iter().any(|word| {
        matches!(
            word.as_str(),
            "auth"
                | "authentication"
                | "authorization"
                | "authorize"
                | "oauth"
                | "token"
                | "password"
                | "secret"
                | "cookie"
                | "credential"
                | "credentials"
                | "principal"
                | "scope"
                | "scopes"
                | "route"
                | "routing"
                | "requeststate"
                | "continuation"
        )
    }) {
        return true;
    }

    let compact = words.concat();
    matches!(
        compact.as_str(),
        "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "apikey"
            | "privatekey"
            | "clientcredential"
            | "clientcredentials"
            | "requeststate"
    )
}

fn is_reserved_extension_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.starts_with("plug/")
        || lower.starts_with("plug.")
        || lower.starts_with("io.plug/")
        || lower.starts_with("com.plug/")
}

fn validate_trace_value(
    key: &str,
    value: &serde_json::Value,
) -> Result<(), ExtensionEnvelopeError> {
    let Some(value) = value.as_str() else {
        return Err(ExtensionEnvelopeError::InvalidKey);
    };
    let limit = if key == "baggage" { 4096 } else { 512 };
    if value.len() > limit || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ExtensionEnvelopeError::TooLarge);
    }
    if key == "traceparent" && !valid_traceparent(value) {
        return Err(ExtensionEnvelopeError::InvalidKey);
    }
    Ok(())
}

/// Validate the W3C trace-parent shape Plug admits from either MCP `_meta` or
/// mirrored HTTP headers.
pub(crate) fn valid_traceparent(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0] == "00"
        && parts[1].len() == 32
        && parts[1].bytes().all(|byte| byte.is_ascii_hexdigit())
        && parts[1].bytes().any(|byte| byte != b'0')
        && parts[2].len() == 16
        && parts[2].bytes().all(|byte| byte.is_ascii_hexdigit())
        && parts[2].bytes().any(|byte| byte != b'0')
        && parts[3].len() == 2
        && parts[3].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_value(
    value: &serde_json::Value,
    depth: usize,
    count: &mut usize,
) -> Result<(), ExtensionEnvelopeError> {
    *count += 1;
    if *count > ExtensionEnvelope::MAX_VALUES {
        return Err(ExtensionEnvelopeError::TooManyValues);
    }
    if depth > ExtensionEnvelope::MAX_DEPTH {
        return Err(ExtensionEnvelopeError::TooDeep);
    }
    match value {
        serde_json::Value::String(value) if looks_secret_shaped(value) => {
            Err(ExtensionEnvelopeError::SecretShapedValue)
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_value(value, depth + 1, count)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                validate_value(value, depth + 1, count)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn looks_secret_shaped(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("bearer ")
        || lower.starts_with("basic ")
        || lower.starts_with("sk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("ya29.")
        || value.contains("-----BEGIN PRIVATE KEY-----")
        || (value.split('.').count() == 3
            && value.len() >= 48
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
}

/// Stable, authorization-grade identity for a downstream caller.
///
/// The enum tag is part of the identity boundary: values from different trust
/// domains can never compare equal even when their display strings match.
/// Self-reported MCP client metadata and session ids are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PrincipalId {
    DownstreamOauth {
        issuer: String,
        client_id: String,
        resource: String,
    },
    ConfiguredCredential {
        config_id: String,
        generation: u64,
    },
    StdioProcess {
        instance_id: Uuid,
    },
    DaemonIpc {
        registry_id: Uuid,
    },
}

impl PrincipalId {
    pub fn downstream_oauth(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
        resource: impl Into<String>,
    ) -> Self {
        Self::DownstreamOauth {
            issuer: issuer.into(),
            client_id: client_id.into(),
            resource: resource.into(),
        }
    }

    pub fn configured_credential(config_id: impl Into<String>, generation: u64) -> Self {
        Self::ConfiguredCredential {
            config_id: config_id.into(),
            generation,
        }
    }

    pub fn stdio_process(instance_id: Uuid) -> Self {
        Self::StdioProcess { instance_id }
    }

    pub fn daemon_ipc(registry_id: Uuid) -> Self {
        Self::DaemonIpc { registry_id }
    }

    pub fn daemon_ipc_registry(registry_client_id: &str) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("plug:ipc-registry:{registry_client_id}"));
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Self::daemon_ipc(Uuid::from_bytes(bytes))
    }

    /// Stable opaque key suitable for ownership indexes and logs. The
    /// canonical principal fields themselves are never exposed.
    pub fn owner_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let encoded = serde_json::to_vec(self).expect("PrincipalId serialization cannot fail");
        let digest = Sha256::digest(encoded);
        format!("principal:{digest:x}")
    }
}

/// A string that redacts its value in `Debug`/`Display` output to prevent
/// secret leakage.
///
/// Redaction covers `Debug` and `Display` only. `Serialize` is
/// `#[serde(transparent)]` and intentionally emits the plaintext value — this
/// is required for config persistence (`config.toml` round-tripping through
/// load → edit → save depends on it). This is a deliberate asymmetry, not a
/// bug: **never** serialize a `Config` (or any struct containing
/// `SecretString`) into logs, IPC diagnostics, or status output, since that
/// path bypasses the `Debug`/`Display` redaction entirely and will leak the
/// secret in plaintext.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    use super::{ExtensionEnvelope, ExtensionEnvelopeError, PrincipalId, SecretString};
    use rmcp::model::MetaObject;
    use uuid::Uuid;

    #[test]
    fn secret_string_debug_is_redacted() {
        let secret = SecretString::from("super-secret".to_string());
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
    }

    #[test]
    fn principal_tags_prevent_cross_domain_collisions() {
        let id = Uuid::from_u128(7);
        assert_ne!(PrincipalId::stdio_process(id), PrincipalId::daemon_ipc(id));
    }

    #[test]
    fn credential_rotation_rules_are_explicit_in_identity() {
        assert_eq!(
            PrincipalId::downstream_oauth("issuer", "client", "resource"),
            PrincipalId::downstream_oauth("issuer", "client", "resource")
        );
        assert_ne!(
            PrincipalId::configured_credential("key", 1),
            PrincipalId::configured_credential("key", 2)
        );
    }

    #[test]
    fn secret_string_display_is_redacted() {
        let secret = SecretString::from("super-secret".to_string());
        assert_eq!(format!("{secret}"), "[REDACTED]");
    }

    fn meta(value: serde_json::Value) -> MetaObject {
        MetaObject::from(value.as_object().expect("object").clone())
    }

    #[test]
    fn extension_envelope_preserves_admitted_types_and_trace_context() {
        let source = meta(serde_json::json!({
            "acme.example/ui": {"enabled": true, "count": 2, "items": [null, "x"]},
            "traceparent": "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01",
            "progressToken": "typed-core-field-is-not-forwarded"
        }));
        let admitted = ExtensionEnvelope::from_peer_meta(Some(&source))
            .unwrap()
            .into_meta()
            .unwrap();
        assert_eq!(
            admitted.get("acme.example/ui"),
            source.get("acme.example/ui")
        );
        assert_eq!(admitted.get("traceparent"), source.get("traceparent"));
        assert!(!admitted.contains_key("progressToken"));
    }

    #[test]
    fn extension_envelope_rejects_reserved_and_control_namespaces() {
        for (value, expected) in [
            (
                serde_json::json!({"plug.dev/legacy-task-support": true}),
                ExtensionEnvelopeError::ReservedNamespace,
            ),
            (
                serde_json::json!({"evil.example/authorization": "not-even-used"}),
                ExtensionEnvelopeError::ControlShapedKey,
            ),
            (
                serde_json::json!({"not-namespaced": true}),
                ExtensionEnvelopeError::InvalidKey,
            ),
        ] {
            assert_eq!(
                ExtensionEnvelope::from_peer_meta(Some(&meta(value))).unwrap_err(),
                expected
            );
        }
    }

    #[test]
    fn extension_envelope_rejects_compound_control_key_spellings() {
        for key in [
            "evil.example/api_key",
            "evil.example/api-key",
            "evil.example/apiKey",
            "evil.example/auth_token",
            "evil.example/auth-token",
            "evil.example/authToken",
            "evil.example/private_key",
            "evil.example/private-key",
            "evil.example/privateKey",
            "evil.example/clientCredentials",
        ] {
            let source = meta(serde_json::json!({(key): "opaque"}));
            assert_eq!(
                ExtensionEnvelope::from_peer_meta(Some(&source)).unwrap_err(),
                ExtensionEnvelopeError::ControlShapedKey,
                "compound control key escaped: {key}"
            );
        }
    }

    #[test]
    fn catalog_envelope_strips_peer_plug_keys_but_keeps_safe_extensions() {
        let source = meta(serde_json::json!({
            "plug.dev/legacy-task-support": true,
            "example.test/value": {"ok": true}
        }));
        let admitted = ExtensionEnvelope::from_peer_catalog_meta(Some(&source))
            .unwrap()
            .into_meta()
            .unwrap();
        assert!(!admitted.contains_key("plug.dev/legacy-task-support"));
        assert_eq!(
            admitted.get("example.test/value"),
            Some(&serde_json::json!({"ok": true}))
        );
    }

    #[test]
    fn extension_envelope_rejects_secret_depth_count_and_byte_bombs() {
        let secret = meta(serde_json::json!({"acme.example/value": "Bearer top-secret"}));
        assert_eq!(
            ExtensionEnvelope::from_peer_meta(Some(&secret)).unwrap_err(),
            ExtensionEnvelopeError::SecretShapedValue
        );

        let mut nested = serde_json::json!(true);
        for _ in 0..=ExtensionEnvelope::MAX_DEPTH {
            nested = serde_json::json!([nested]);
        }
        let nested = meta(serde_json::json!({"acme.example/value": nested}));
        assert_eq!(
            ExtensionEnvelope::from_peer_meta(Some(&nested)).unwrap_err(),
            ExtensionEnvelopeError::TooDeep
        );

        let many = meta(serde_json::json!({
            "acme.example/value": vec![true; ExtensionEnvelope::MAX_VALUES + 1]
        }));
        assert_eq!(
            ExtensionEnvelope::from_peer_meta(Some(&many)).unwrap_err(),
            ExtensionEnvelopeError::TooManyValues
        );

        let huge = meta(serde_json::json!({
            "acme.example/value": "x".repeat(ExtensionEnvelope::MAX_ENCODED_BYTES)
        }));
        assert_eq!(
            ExtensionEnvelope::from_peer_meta(Some(&huge)).unwrap_err(),
            ExtensionEnvelopeError::TooLarge
        );
    }

    #[test]
    fn extension_envelope_debug_never_contains_values() {
        let source = meta(serde_json::json!({"acme.example/value": "operator-private"}));
        let envelope = ExtensionEnvelope::from_peer_meta(Some(&source)).unwrap();
        let rendered = format!("{envelope:?}");
        assert!(!rendered.contains("operator-private"));
        assert!(rendered.contains("[REDACTED]"));
    }

    /// Pins the deliberate asymmetry documented on `SecretString`: `Serialize`
    /// emits plaintext (required for config persistence) while
    /// `Debug`/`Display` redact. If this test ever fails because `Serialize`
    /// stops emitting plaintext, config round-tripping is likely broken;
    /// don't "fix" it without checking `plug/src/commands/config.rs`.
    #[test]
    fn secret_string_serialize_is_plaintext_but_debug_is_redacted() {
        let secret = SecretString::from("value".to_string());
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"value\"");
        assert_eq!(format!("{secret:?}"), "[REDACTED]");
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
            selected_protocol_era: None,
            selected_protocol_version: None,
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
/// Where [`ServerHealth`] tracks the connection's health-check state (a failure
/// counter), `Availability` describes what the merged catalog currently serves for
/// this upstream. The two are orthogonal and can legitimately disagree — a routable
/// server (`health = Healthy`) whose listing timed out this cycle reports
/// `availability = Degraded`. Note both enums spell one state `Degraded`; they mean
/// different things (connection vs. catalog).
///
/// - `Healthy`: the last catalog refresh listed this upstream's resources/prompts live.
/// - `Degraded`: the upstream is still routable but its last refresh failed to list
///   (timeout/error). Last-known-good catalog entries are carried forward when they
///   exist (and resource subscriptions are preserved, not pruned); if there is no
///   last-known-good yet, the upstream contributes nothing this cycle but is still
///   reported degraded rather than healthy.
/// - `Absent`: the upstream is not in the routed set — removed from config, failed, or
///   awaiting auth.
///
/// This is independent of [`UpstreamMetricsSnapshot::degraded_since_epoch_secs`], which
/// times *tool-call* degradation, not catalog-listing degradation.
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
    /// How many times this upstream has been restarted since start by recovery
    /// or supervision (item 2b). Always present; `0` for a never-restarted
    /// upstream.
    #[serde(default)]
    pub restart_count: u64,
    /// Unix epoch seconds of the most recent supervised restart; serialized as
    /// `null` when the upstream has never been restarted. Always present so the
    /// JSON schema stays stable for agent consumers.
    #[serde(default)]
    pub last_restart_epoch_secs: Option<u64>,
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
    /// Protocol selected by the current live upstream connection. Both values
    /// are absent when no connection exists, so configured intent is never
    /// mistaken for runtime truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_protocol_era: Option<crate::protocol::ProtocolEra>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_protocol_version: Option<String>,
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
    /// Protocol selected by the live upstream connection. Additive so older
    /// daemon/operator JSON remains readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_protocol_version: Option<String>,
}
