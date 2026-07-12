use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::config::HttpConfig;

const AUTH_CODE_LIFETIME_SECS: u64 = 300;
const ACCESS_TOKEN_LIFETIME_SECS: u64 = 3600;
const REFRESH_TOKEN_LIFETIME_SECS: u64 = 30 * 24 * 3600;

/// Parsed downstream OAuth configuration derived from `HttpConfig`.
#[derive(Debug, Clone)]
pub struct DownstreamOauthConfig {
    pub public_base_url: String,
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret: Option<crate::types::SecretString>,
    pub oauth_scopes: Vec<String>,
    /// Exact-match allowlist of non-loopback redirect URIs. Loopback URIs are
    /// always permitted; everything else must be listed here.
    pub redirect_uri_allowlist: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DownstreamOauthManager {
    pub config: DownstreamOauthConfig,
    state: Arc<Mutex<DownstreamOauthState>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownstreamAuthChallenge {
    Redirect(String),
    Unauthorized,
}

#[derive(Debug, Clone)]
pub struct TokenResponsePayload {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    pub scope: Option<String>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DownstreamOauthError {
    #[error("invalid client")]
    InvalidClient,
    #[error("unsupported client auth method")]
    UnsupportedClientAuthMethod,
    #[error("missing client credentials")]
    MissingClientCredentials,
    #[error("invalid authorization request")]
    InvalidAuthorizationRequest,
    #[error("invalid grant")]
    InvalidGrant,
    #[error("pkce verification failed")]
    PkceVerificationFailed,
    #[error("token expired")]
    TokenExpired,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DownstreamOauthState {
    #[serde(default)]
    pending_codes: HashMap<String, PendingAuthorizationCode>,
    #[serde(default)]
    access_tokens: HashMap<String, IssuedAccessToken>,
    #[serde(default)]
    refresh_tokens: HashMap<String, IssuedRefreshToken>,
}

impl DownstreamOauthState {
    /// Eagerly drops entries that expired at or before `now` from all three
    /// maps. Called on the rare mutation paths (never on the hot
    /// `validate_access_token` read path) so issued-and-never-presented
    /// tokens and abandoned auth codes don't linger in memory or in the
    /// persisted state file forever.
    fn evict_expired(&mut self, now: u64) {
        self.pending_codes.retain(|_, c| c.expires_at >= now);
        self.access_tokens.retain(|_, t| t.expires_at >= now);
        self.refresh_tokens.retain(|_, t| t.expires_at >= now);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingAuthorizationCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    expires_at: u64,
    scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuedAccessToken {
    client_id: String,
    refresh_token: String,
    expires_at: u64,
    scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuedRefreshToken {
    client_id: String,
    expires_at: u64,
    scopes: Vec<String>,
}

impl DownstreamOauthConfig {
    pub fn from_http_config(http: &HttpConfig) -> Option<Self> {
        if http.auth_mode != crate::config::DownstreamAuthMode::Oauth {
            return None;
        }

        Some(Self {
            public_base_url: http.public_base_url.clone()?,
            oauth_client_id: http.oauth_client_id.clone(),
            oauth_client_secret: http.oauth_client_secret.clone(),
            oauth_scopes: http.oauth_scopes.clone().unwrap_or_default(),
            redirect_uri_allowlist: http.oauth_redirect_uri_allowlist.clone(),
        })
    }
}

/// Returns true when `redirect_uri`'s host is loopback (always trusted) or the
/// URI is on the configured allowlist. Prevents `/oauth/authorize` from acting
/// as an open redirector for arbitrary off-host callbacks.
fn redirect_uri_allowed(redirect_uri: &str, allowlist: &[String]) -> bool {
    if allowlist.iter().any(|allowed| allowed == redirect_uri) {
        return true;
    }
    match url::Url::parse(redirect_uri) {
        // `host_str()` returns IPv6 hosts bracketed (`[::1]`); some url
        // versions/forms surface `::1` unbracketed — accept both.
        Ok(parsed) => matches!(
            parsed.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
        ),
        Err(_) => false,
    }
}

impl DownstreamOauthManager {
    pub fn new(config: DownstreamOauthConfig) -> Self {
        let state = match load_persisted_state(&config) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "starting with default downstream oauth state; all previously issued tokens are now invalid"
                );
                DownstreamOauthState::default()
            }
        };
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn base_url(&self) -> &str {
        self.config.public_base_url.trim_end_matches('/')
    }

    pub fn authorization_endpoint(&self) -> String {
        format!("{}/oauth/authorize", self.base_url())
    }

    pub fn token_endpoint(&self) -> String {
        format!("{}/oauth/token", self.base_url())
    }

    pub async fn build_authorize_redirect(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
        code_challenge_method: &str,
        requested_scopes: Option<&str>,
    ) -> Result<String, DownstreamOauthError> {
        self.validate_client_id(client_id)?;
        if redirect_uri.is_empty() || state.is_empty() || code_challenge.is_empty() {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if code_challenge_method != "S256" {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if !redirect_uri_allowed(redirect_uri, &self.config.redirect_uri_allowlist) {
            // Reject unlisted off-host callbacks before issuing a code — do not
            // redirect to an attacker-controlled URI. Logged so an operator can
            // see when a legitimate non-loopback callback needs allowlisting.
            tracing::warn!(
                redirect_uri = %redirect_uri,
                "rejected /oauth/authorize: redirect_uri is not loopback and not on http.oauth_redirect_uri_allowlist"
            );
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }

        let auth_code = uuid::Uuid::new_v4().to_string();
        let scopes = requested_scopes
            .map(|s| {
                s.split_whitespace()
                    .filter(|part| !part.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| self.config.oauth_scopes.clone());

        let pending = PendingAuthorizationCode {
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            code_challenge: code_challenge.to_string(),
            expires_at: epoch_secs() + AUTH_CODE_LIFETIME_SECS,
            scopes,
        };
        let mut guard = self.state.lock().await;
        guard.evict_expired(epoch_secs());
        guard.pending_codes.insert(auth_code.clone(), pending);
        persist_state(&self.config, &guard);

        // Percent-encode code and state so reserved characters in `state`
        // cannot break out of the query or inject extra parameters.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &auth_code)
            .append_pair("state", state)
            .finish();
        let separator = if redirect_uri.contains('?') { '&' } else { '?' };
        Ok(format!("{redirect_uri}{separator}{query}"))
    }

    pub async fn exchange_authorization_code(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.validate_client_auth(client_id, client_secret)?;

        let mut guard = self.state.lock().await;
        guard.evict_expired(epoch_secs());
        let pending = guard
            .pending_codes
            .get(code)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidGrant)?;

        if pending.client_id != client_id || pending.redirect_uri != redirect_uri {
            return Err(DownstreamOauthError::InvalidGrant);
        }
        if pending.expires_at < epoch_secs() {
            guard.pending_codes.remove(code);
            return Err(DownstreamOauthError::TokenExpired);
        }

        let verifier = oauth2::PkceCodeVerifier::new(code_verifier.to_string());
        let computed = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        if computed.as_str() != pending.code_challenge {
            return Err(DownstreamOauthError::PkceVerificationFailed);
        }
        guard.pending_codes.remove(code);

        let access_token = uuid::Uuid::new_v4().to_string();
        let refresh_token = uuid::Uuid::new_v4().to_string();

        guard.refresh_tokens.insert(
            refresh_token.clone(),
            IssuedRefreshToken {
                client_id: client_id.to_string(),
                expires_at: epoch_secs() + REFRESH_TOKEN_LIFETIME_SECS,
                scopes: pending.scopes.clone(),
            },
        );
        guard.access_tokens.insert(
            access_token.clone(),
            IssuedAccessToken {
                client_id: client_id.to_string(),
                refresh_token: refresh_token.clone(),
                expires_at: epoch_secs() + ACCESS_TOKEN_LIFETIME_SECS,
                scopes: pending.scopes.clone(),
            },
        );
        persist_state(&self.config, &guard);

        Ok(TokenResponsePayload {
            access_token,
            refresh_token: Some(refresh_token),
            expires_in: ACCESS_TOKEN_LIFETIME_SECS,
            scope: scope_string(&pending.scopes),
        })
    }

    pub async fn exchange_refresh_token(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        refresh_token: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.validate_client_auth(client_id, client_secret)?;

        let mut guard = self.state.lock().await;
        guard.evict_expired(epoch_secs());
        let refresh = guard
            .refresh_tokens
            .get(refresh_token)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidGrant)?;

        if refresh.client_id != client_id {
            return Err(DownstreamOauthError::InvalidGrant);
        }
        if refresh.expires_at < epoch_secs() {
            guard.refresh_tokens.remove(refresh_token);
            persist_state(&self.config, &guard);
            return Err(DownstreamOauthError::TokenExpired);
        }

        let access_token = uuid::Uuid::new_v4().to_string();
        guard.access_tokens.insert(
            access_token.clone(),
            IssuedAccessToken {
                client_id: client_id.to_string(),
                refresh_token: refresh_token.to_string(),
                expires_at: epoch_secs() + ACCESS_TOKEN_LIFETIME_SECS,
                scopes: refresh.scopes.clone(),
            },
        );
        persist_state(&self.config, &guard);

        Ok(TokenResponsePayload {
            access_token,
            refresh_token: Some(refresh_token.to_string()),
            expires_in: ACCESS_TOKEN_LIFETIME_SECS,
            scope: scope_string(&refresh.scopes),
        })
    }

    pub async fn exchange_client_credentials(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        requested_scopes: Option<&str>,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.validate_client_auth(client_id, client_secret)?;
        if self.config.oauth_client_secret.is_none() {
            return Err(DownstreamOauthError::UnsupportedClientAuthMethod);
        }

        let scopes = canonical_cc_scopes(
            requested_scopes
                .map(|s| {
                    s.split_whitespace()
                        .filter(|part| !part.is_empty() && *part != "offline_access")
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| resource_scopes(&self.config.oauth_scopes)),
        );

        let mut guard = self.state.lock().await;
        guard.evict_expired(epoch_secs());

        // Reuse a live client_credentials-issued token for this client+scope
        // set instead of minting a fresh one on every call. This is the CC
        // marker: `refresh_token.is_empty()`. A 60s floor avoids handing out
        // a token that's about to die; if none qualifies, fall through to
        // minting a new one.
        let now = epoch_secs();
        if let Some((existing_token, existing_record)) =
            guard.access_tokens.iter().find(|(_, t)| {
                t.client_id == client_id
                    && t.refresh_token.is_empty()
                    && t.scopes == scopes
                    && t.expires_at >= now + 60
            })
        {
            let response = TokenResponsePayload {
                access_token: existing_token.clone(),
                refresh_token: None,
                expires_in: existing_record.expires_at - now,
                scope: scope_string(&scopes),
            };
            return Ok(response);
        }

        let access_token = uuid::Uuid::new_v4().to_string();
        guard.access_tokens.insert(
            access_token.clone(),
            IssuedAccessToken {
                client_id: client_id.to_string(),
                refresh_token: String::new(),
                expires_at: epoch_secs() + ACCESS_TOKEN_LIFETIME_SECS,
                scopes: scopes.clone(),
            },
        );
        persist_state(&self.config, &guard);

        Ok(TokenResponsePayload {
            access_token,
            refresh_token: None,
            expires_in: ACCESS_TOKEN_LIFETIME_SECS,
            scope: scope_string(&scopes),
        })
    }

    pub async fn validate_access_token(&self, token: &str) -> bool {
        let mut guard = self.state.lock().await;
        match guard.access_tokens.get(token) {
            Some(record) if record.expires_at >= epoch_secs() => {
                let _ = &record.client_id;
                let _ = &record.refresh_token;
                let _ = &record.scopes;
                true
            }
            Some(_) => {
                guard.access_tokens.remove(token);
                persist_state(&self.config, &guard);
                false
            }
            None => false,
        }
    }

    fn validate_client_id(&self, client_id: &str) -> Result<(), DownstreamOauthError> {
        match self.config.oauth_client_id.as_deref() {
            Some(expected) if expected == client_id => Ok(()),
            Some(_) => Err(DownstreamOauthError::InvalidClient),
            None => Err(DownstreamOauthError::MissingClientCredentials),
        }
    }

    fn validate_client_auth(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
    ) -> Result<(), DownstreamOauthError> {
        self.validate_client_id(client_id)?;

        match self.config.oauth_client_secret.as_ref().map(|s| s.as_str()) {
            Some(expected_secret) => match client_secret {
                Some(provided)
                    if subtle::ConstantTimeEq::ct_eq(
                        provided.as_bytes(),
                        expected_secret.as_bytes(),
                    )
                    .into() =>
                {
                    Ok(())
                }
                Some(_) => Err(DownstreamOauthError::InvalidClient),
                None => Err(DownstreamOauthError::MissingClientCredentials),
            },
            None => Ok(()),
        }
    }

    pub fn client_credentials_from_headers_or_form(
        &self,
        headers: &HeaderMap,
        form_client_id: Option<&str>,
        form_client_secret: Option<&str>,
    ) -> Result<(String, Option<String>), DownstreamOauthError> {
        if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION) {
            let auth = auth_header
                .to_str()
                .map_err(|_| DownstreamOauthError::UnsupportedClientAuthMethod)?;
            if let Some(basic) = auth.strip_prefix("Basic ") {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(basic)
                    .map_err(|_| DownstreamOauthError::UnsupportedClientAuthMethod)?;
                let decoded = String::from_utf8(decoded)
                    .map_err(|_| DownstreamOauthError::UnsupportedClientAuthMethod)?;
                let (client_id, client_secret) = decoded
                    .split_once(':')
                    .ok_or(DownstreamOauthError::UnsupportedClientAuthMethod)?;
                return Ok((client_id.to_string(), Some(client_secret.to_string())));
            }
            return Err(DownstreamOauthError::UnsupportedClientAuthMethod);
        }

        let client_id = form_client_id.ok_or(DownstreamOauthError::MissingClientCredentials)?;
        Ok((
            client_id.to_string(),
            form_client_secret.map(ToString::to_string),
        ))
    }
}

pub fn resource_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| scope.as_str() != "offline_access")
        .cloned()
        .collect()
}

/// Canonicalizes a scope set for `client_credentials` token reuse and
/// storage comparisons: sorts and dedups. RFC 6749 treats `scope` as an
/// unordered set, so "a b" and "b a" (or "a a") must compare equal for CC
/// token reuse (see `exchange_client_credentials`) to work as intended.
///
/// CC-only by design: the authorization-code flow's
/// `build_authorize_redirect` scope handling is intentionally left
/// untouched, since ordering there is only ever preserved and echoed back,
/// never compared against a stored form.
fn canonical_cc_scopes(mut scopes: Vec<String>) -> Vec<String> {
    scopes.sort();
    scopes.dedup();
    scopes
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn scope_string(scopes: &[String]) -> Option<String> {
    if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    }
}

fn state_file_path(config: &DownstreamOauthConfig) -> Result<PathBuf, String> {
    let client = config.oauth_client_id.as_deref().unwrap_or("default");
    let instance = format!("{}|{client}", config.public_base_url);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    instance.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let safe = crate::config::sanitize_server_name_for_path(client)
        .map_err(|e| format!("invalid downstream oauth client id for file path: {e}"))?;
    Ok(crate::config::config_dir()
        .join("downstream_oauth")
        .join(format!("{safe}-{hash}.json")))
}

fn load_persisted_state(config: &DownstreamOauthConfig) -> Result<DownstreamOauthState, String> {
    let path = state_file_path(config)?;
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DownstreamOauthState::default());
        }
        Err(e) => return Err(format!("failed to read downstream oauth state: {e}")),
    };

    let mut state: DownstreamOauthState = serde_json::from_str(&data)
        .map_err(|e| format!("failed to parse downstream oauth state: {e}"))?;
    // Auth codes are intentionally ephemeral. Do not reload them after restart.
    state.pending_codes.clear();
    Ok(state)
}

fn persist_state(config: &DownstreamOauthConfig, state: &DownstreamOauthState) {
    let path = match state_file_path(config) {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to persist downstream oauth state: could not compute state file path"
            );
            return;
        }
    };
    let dir = match path.parent() {
        Some(dir) => dir,
        None => {
            tracing::warn!(
                path = %path.display(),
                "failed to persist downstream oauth state: state file path has no parent directory"
            );
            return;
        }
    };
    if let Err(e) = crate::fs_perm::ensure_dir_0700(dir) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "failed to persist downstream oauth state: could not create state directory"
        );
        return;
    }

    let mut persisted = state.clone();
    persisted.pending_codes.clear();

    let json = match serde_json::to_string_pretty(&persisted) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to persist downstream oauth state: could not serialize state"
            );
            return;
        }
    };

    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // `mode(0o600)` only applies when the file is created: truncating a
        // stale temp file (left behind by a crash between write and rename)
        // keeps its old permission bits. Remove any leftover first so tokens
        // are only ever written to a freshly created owner-only file.
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    path = %tmp.display(),
                    error = %e,
                    "failed to persist downstream oauth state: could not remove stale temp file"
                );
                return;
            }
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp);
        let mut file = match file {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(
                    path = %tmp.display(),
                    error = %e,
                    "failed to persist downstream oauth state: could not open temp file"
                );
                return;
            }
        };
        if let Err(e) = file.write_all(json.as_bytes()) {
            tracing::warn!(
                path = %tmp.display(),
                error = %e,
                "failed to persist downstream oauth state: could not write temp file"
            );
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        // Backstop: after the stale-tmp removal above the file was freshly
        // created with mode 0600, so this should never change anything —
        // but if it fails, do not rename a possibly-loose temp file into
        // place as the credential file.
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                path = %tmp.display(),
                error = %e,
                "failed to persist downstream oauth state: could not set temp file permissions"
            );
            let _ = std::fs::remove_file(&tmp);
            return;
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(e) = std::fs::write(&tmp, &json) {
            tracing::warn!(
                path = %tmp.display(),
                error = %e,
                "failed to persist downstream oauth state: could not write temp file"
            );
            let _ = std::fs::remove_file(&tmp);
            return;
        }
    }

    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::warn!(
            from = %tmp.display(),
            to = %path.display(),
            error = %e,
            "failed to persist downstream oauth state: could not rename temp file into place"
        );
        let _ = std::fs::remove_file(&tmp);
        return;
    }

    #[cfg(unix)]
    {
        // Mirror the upstream credential store: enforce owner-only
        // permissions on the final file after the rename as defense in
        // depth. On failure, warn but keep the live state file — losing
        // issued tokens is worse than a permission warning here.
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "persisted downstream oauth state but could not enforce state file permissions"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(client_id: &str) -> DownstreamOauthConfig {
        DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_client_id: Some(client_id.to_string()),
            oauth_client_secret: Some("secret-123".to_string().into()),
            oauth_scopes: vec!["tools:read".to_string()],
            redirect_uri_allowlist: vec!["https://client.example.com/callback".to_string()],
        }
    }

    fn cleanup_state(client_id: &str) {
        let config = test_config(client_id);
        if let Ok(path) = state_file_path(&config) {
            let _ = std::fs::remove_file(path.with_extension("json.tmp"));
            let _ = std::fs::remove_file(path);
        }
    }

    #[tokio::test]
    async fn issued_tokens_survive_manager_recreation() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "https://client.example.com/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await
            .expect("authorize redirect");
        let code = redirect
            .split("code=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .expect("auth code");

        let issued = manager
            .exchange_authorization_code(
                &client_id,
                Some("secret-123"),
                code,
                "https://client.example.com/callback",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            )
            .await
            .expect("token exchange");

        let recreated = DownstreamOauthManager::new(test_config(&client_id));
        assert!(recreated.validate_access_token(&issued.access_token).await);
        let refresh_token = issued
            .refresh_token
            .expect("authorization code issues refresh");
        let refreshed = recreated
            .exchange_refresh_token(&client_id, Some("secret-123"), &refresh_token)
            .await
            .expect("refresh token exchange");
        assert!(!refreshed.access_token.is_empty());

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn expired_persisted_access_token_is_cleaned_up() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let config = test_config(&client_id);

        let mut state = DownstreamOauthState::default();
        state.access_tokens.insert(
            "expired-access".to_string(),
            IssuedAccessToken {
                client_id: client_id.clone(),
                refresh_token: "refresh-token".to_string(),
                expires_at: epoch_secs().saturating_sub(10),
                scopes: vec!["tools:read".to_string()],
            },
        );
        state.refresh_tokens.insert(
            "refresh-token".to_string(),
            IssuedRefreshToken {
                client_id: client_id.clone(),
                expires_at: epoch_secs() + REFRESH_TOKEN_LIFETIME_SECS,
                scopes: vec!["tools:read".to_string()],
            },
        );
        persist_state(&config, &state);

        let manager = DownstreamOauthManager::new(config.clone());
        assert!(!manager.validate_access_token("expired-access").await);

        let reloaded = load_persisted_state(&config).expect("reload persisted state");
        assert!(!reloaded.access_tokens.contains_key("expired-access"));

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn invalid_pkce_does_not_consume_authorization_code() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "https://client.example.com/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await
            .expect("authorize redirect");
        let code = redirect
            .split("code=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .expect("auth code");

        let first = manager
            .exchange_authorization_code(
                &client_id,
                Some("secret-123"),
                code,
                "https://client.example.com/callback",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .await;
        assert!(matches!(
            first,
            Err(DownstreamOauthError::PkceVerificationFailed)
        ));

        let second = manager
            .exchange_authorization_code(
                &client_id,
                Some("secret-123"),
                code,
                "https://client.example.com/callback",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            )
            .await;
        assert!(second.is_ok(), "authorization code should remain retryable");

        cleanup_state(&client_id);
    }

    #[test]
    fn persistence_is_scoped_by_public_base_url() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        let config_a = test_config(&client_id);
        let mut config_b = test_config(&client_id);
        config_b.public_base_url = "https://other.example.com".to_string();

        let path_a = state_file_path(&config_a).expect("state path a");
        let path_b = state_file_path(&config_b).expect("state path b");
        assert_ne!(path_a, path_b);
    }

    #[tokio::test]
    async fn authorize_rejects_unlisted_off_host_redirect_uri() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        // Not loopback and not on the allowlist -> rejected, no code issued.
        let result = manager
            .build_authorize_redirect(
                &client_id,
                "https://attacker.example.com/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(DownstreamOauthError::InvalidAuthorizationRequest)
            ),
            "unlisted off-host redirect_uri must be rejected, got {result:?}"
        );
        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn authorize_allows_loopback_redirect_without_allowlist() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let mut config = test_config(&client_id);
        config.redirect_uri_allowlist.clear();
        let manager = DownstreamOauthManager::new(config);

        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "http://127.0.0.1:7777/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await;
        assert!(
            redirect.is_ok(),
            "loopback redirect should be allowed without an allowlist entry, got {redirect:?}"
        );
        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn authorize_allows_ipv6_loopback_redirect() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let mut config = test_config(&client_id);
        config.redirect_uri_allowlist.clear();
        let manager = DownstreamOauthManager::new(config);

        // host_str() strips the brackets, so [::1] is recognized as loopback.
        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "http://[::1]:7777/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await;
        assert!(
            redirect.is_ok(),
            "IPv6 loopback redirect should be allowed, got {redirect:?}"
        );
        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn authorize_rejects_loopback_userinfo_confusion() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let mut config = test_config(&client_id);
        config.redirect_uri_allowlist.clear();
        let manager = DownstreamOauthManager::new(config);

        // Host is evil.com, not 127.0.0.1 — must be rejected.
        let result = manager
            .build_authorize_redirect(
                &client_id,
                "https://127.0.0.1@evil.com/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(DownstreamOauthError::InvalidAuthorizationRequest)
            ),
            "userinfo host-confusion must be rejected, got {result:?}"
        );
        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn authorize_percent_encodes_state() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        // A state value with reserved characters must not break out of the query.
        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "https://client.example.com/callback",
                "a b&c=d",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await
            .expect("authorize redirect");
        assert!(
            redirect.contains("state=a+b%26c%3Dd"),
            "state must be percent-encoded, got {redirect}"
        );
        assert!(
            !redirect.contains("state=a b&c=d"),
            "raw unencoded state must not appear, got {redirect}"
        );
        cleanup_state(&client_id);
    }

    #[test]
    fn evict_expired_drops_only_expired_entries() {
        let now = 1_000_000u64;
        let mut state = DownstreamOauthState::default();

        state.pending_codes.insert(
            "live-code".to_string(),
            PendingAuthorizationCode {
                client_id: "c".to_string(),
                redirect_uri: "https://client.example.com/callback".to_string(),
                code_challenge: "chal".to_string(),
                expires_at: now + 10,
                scopes: vec!["tools:read".to_string()],
            },
        );
        state.pending_codes.insert(
            "expired-code".to_string(),
            PendingAuthorizationCode {
                client_id: "c".to_string(),
                redirect_uri: "https://client.example.com/callback".to_string(),
                code_challenge: "chal".to_string(),
                expires_at: now - 10,
                scopes: vec!["tools:read".to_string()],
            },
        );

        state.access_tokens.insert(
            "live-access".to_string(),
            IssuedAccessToken {
                client_id: "c".to_string(),
                refresh_token: "r".to_string(),
                expires_at: now + 10,
                scopes: vec!["tools:read".to_string()],
            },
        );
        state.access_tokens.insert(
            "expired-access".to_string(),
            IssuedAccessToken {
                client_id: "c".to_string(),
                refresh_token: "r".to_string(),
                expires_at: now - 10,
                scopes: vec!["tools:read".to_string()],
            },
        );

        state.refresh_tokens.insert(
            "live-refresh".to_string(),
            IssuedRefreshToken {
                client_id: "c".to_string(),
                expires_at: now + 10,
                scopes: vec!["tools:read".to_string()],
            },
        );
        state.refresh_tokens.insert(
            "expired-refresh".to_string(),
            IssuedRefreshToken {
                client_id: "c".to_string(),
                expires_at: now - 10,
                scopes: vec!["tools:read".to_string()],
            },
        );

        state.evict_expired(now);

        assert!(state.pending_codes.contains_key("live-code"));
        assert!(!state.pending_codes.contains_key("expired-code"));
        assert!(state.access_tokens.contains_key("live-access"));
        assert!(!state.access_tokens.contains_key("expired-access"));
        assert!(state.refresh_tokens.contains_key("live-refresh"));
        assert!(!state.refresh_tokens.contains_key("expired-refresh"));
    }

    #[tokio::test]
    async fn abandoned_auth_code_swept_on_next_mutation() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let redirect = manager
            .build_authorize_redirect(
                &client_id,
                "https://client.example.com/callback",
                "abc123",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "S256",
                Some("tools:read"),
            )
            .await
            .expect("authorize redirect");
        let code = redirect
            .split("code=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .expect("auth code")
            .to_string();

        // Force the pending code to already be expired, simulating an
        // abandoned authorization that outlived AUTH_CODE_LIFETIME_SECS,
        // without waiting 300s of real time.
        {
            let mut guard = manager.state.lock().await;
            assert!(
                guard.pending_codes.contains_key(&code),
                "pending code should be present before the sweep"
            );
            if let Some(pending) = guard.pending_codes.get_mut(&code) {
                pending.expires_at = epoch_secs().saturating_sub(1);
            }
        }

        // Unrelated mutation: a client_credentials exchange should sweep the
        // now-expired code as a side effect.
        manager
            .exchange_client_credentials(&client_id, Some("secret-123"), None)
            .await
            .expect("client credentials exchange");

        {
            let guard = manager.state.lock().await;
            assert!(
                !guard.pending_codes.contains_key(&code),
                "expired auth code should be swept on next mutation"
            );
        }

        let reloaded =
            load_persisted_state(&test_config(&client_id)).expect("reload persisted state");
        assert!(
            !reloaded.pending_codes.contains_key(&code),
            "swept code must not reappear from the persisted file"
        );

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_reuses_live_token() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let first = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("first client credentials exchange");
        let second = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("second client credentials exchange");

        assert_eq!(
            first.access_token, second.access_token,
            "identical CC exchanges should reuse the same access token"
        );
        assert!(
            second.expires_in <= first.expires_in,
            "reused token's remaining lifetime should not exceed the original: first={} second={}",
            first.expires_in,
            second.expires_in
        );

        let guard = manager.state.lock().await;
        let cc_entries = guard
            .access_tokens
            .values()
            .filter(|t| t.client_id == client_id && t.refresh_token.is_empty())
            .count();
        assert_eq!(
            cc_entries, 1,
            "expected exactly one CC entry for this scope set"
        );
        drop(guard);

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_mints_new_token_for_different_scopes() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let a = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("a"))
            .await
            .expect("scope a exchange");
        let b = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("b"))
            .await
            .expect("scope b exchange");

        assert_ne!(
            a.access_token, b.access_token,
            "different scope sets must not share a reused token"
        );
        assert!(manager.validate_access_token(&a.access_token).await);
        assert!(manager.validate_access_token(&b.access_token).await);

        cleanup_state(&client_id);
    }

    // CHARACTERIZATION: scopes are issued and persisted but NOT enforced.
    // `validate_access_token` only checks expiry and returns a bare bool; it
    // does not consult the record's scopes at all, so any valid token grants
    // full access to every merged tool/resource/prompt. This test pins
    // today's behavior so that a future enforcement change (owned by plan
    // 018's conformance spike) is a deliberate, visible test edit rather
    // than an unnoticed regression.
    #[tokio::test]
    async fn any_valid_token_grants_full_access() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let issued = manager
            .exchange_client_credentials(
                &client_id,
                Some("secret-123"),
                Some("narrow:scope-that-should-not-exist"),
            )
            .await
            .expect("client credentials exchange");

        assert!(
            manager.validate_access_token(&issued.access_token).await,
            "a token with a narrow, made-up scope still validates as fully authorized"
        );

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_reused_token_survives_manager_recreation() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let first = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("first client credentials exchange");
        let second = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("second client credentials exchange");
        assert_eq!(first.access_token, second.access_token);

        let recreated = DownstreamOauthManager::new(test_config(&client_id));
        assert!(
            recreated.validate_access_token(&second.access_token).await,
            "reused CC token must survive manager recreation via persisted state"
        );

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_permuted_scopes_reuse_token() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let first = manager
            .exchange_client_credentials(
                &client_id,
                Some("secret-123"),
                Some("tools:read tools:write"),
            )
            .await
            .expect("first client credentials exchange");
        let second = manager
            .exchange_client_credentials(
                &client_id,
                Some("secret-123"),
                Some("tools:write tools:read"),
            )
            .await
            .expect("second client credentials exchange");

        assert_eq!(
            first.access_token, second.access_token,
            "reordered scope strings for the same set should reuse the same access token"
        );

        let guard = manager.state.lock().await;
        let cc_entries = guard
            .access_tokens
            .values()
            .filter(|t| t.client_id == client_id && t.refresh_token.is_empty())
            .count();
        assert_eq!(
            cc_entries, 1,
            "expected exactly one CC entry despite scope reordering"
        );
        drop(guard);

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_duplicated_scopes_reuse_token() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(test_config(&client_id));

        let first = manager
            .exchange_client_credentials(
                &client_id,
                Some("secret-123"),
                Some("tools:read tools:read"),
            )
            .await
            .expect("first client credentials exchange");
        let second = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("second client credentials exchange");

        assert_eq!(
            first.access_token, second.access_token,
            "duplicated scope tokens should collapse to the same canonical set and reuse the token"
        );

        let guard = manager.state.lock().await;
        let cc_entries = guard
            .access_tokens
            .values()
            .filter(|t| t.client_id == client_id && t.refresh_token.is_empty())
            .count();
        assert_eq!(
            cc_entries, 1,
            "expected exactly one CC entry despite duplicated scopes"
        );
        drop(guard);

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn client_credentials_default_scopes_match_explicit_reordered_scopes() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        let mut config = test_config(&client_id);
        // A two-scope default makes ordering meaningful for this comparison;
        // the shared single-scope `test_config` default would trivially match.
        config.oauth_scopes = vec!["tools:read".to_string(), "tools:write".to_string()];
        cleanup_state(&client_id);
        let manager = DownstreamOauthManager::new(config);

        let first = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), None)
            .await
            .expect("default-scope client credentials exchange");
        let second = manager
            .exchange_client_credentials(
                &client_id,
                Some("secret-123"),
                Some("tools:write tools:read"),
            )
            .await
            .expect("explicit reordered-scope client credentials exchange");

        assert_eq!(
            first.access_token, second.access_token,
            "config-default scopes and an explicit reordered request for the same set should reuse the token"
        );

        cleanup_state(&client_id);
    }

    #[tokio::test]
    async fn corrupt_state_file_falls_back_to_default_state_without_panic() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        let config = test_config(&client_id);
        cleanup_state(&client_id);

        let path = state_file_path(&config).expect("compute state file path");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create state dir");
        }
        std::fs::write(&path, b"not valid json{{{").expect("write corrupt state file");

        // Must not panic even though the on-disk state is unreadable.
        let manager = DownstreamOauthManager::new(config);
        let guard = manager.state.lock().await;
        assert!(
            guard.access_tokens.is_empty()
                && guard.refresh_tokens.is_empty()
                && guard.pending_codes.is_empty(),
            "a corrupt state file should fall back to empty default state"
        );
        drop(guard);

        cleanup_state(&client_id);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_loose_tmp_file_is_replaced_and_state_file_is_owner_only() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let config = test_config(&client_id);

        let path = state_file_path(&config).expect("compute state file path");
        let tmp = path.with_extension("json.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create state dir");
        }
        // Simulate a crash between write and rename that left a temp file
        // behind with loose permissions. A later persist must not truncate
        // and reuse it (which would keep the 0644 bits on the token file).
        std::fs::write(&tmp, b"stale junk from a previous crash").expect("write stale tmp");
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))
            .expect("loosen stale tmp permissions");

        let manager = DownstreamOauthManager::new(config.clone());
        let issued = manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("client credentials exchange");

        let meta = std::fs::metadata(&path).expect("state file exists after persist");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "state file must be owner-only even when a loose stale tmp existed"
        );
        let reloaded = load_persisted_state(&config).expect("state file parses");
        assert!(
            reloaded.access_tokens.contains_key(&issued.access_token),
            "issued token must be present in the persisted state"
        );
        assert!(!tmp.exists(), "no temp file should remain after persist");

        cleanup_state(&client_id);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_persist_writes_state_file_with_owner_only_permissions() {
        let client_id = format!("test-client-{}", uuid::Uuid::new_v4());
        cleanup_state(&client_id);
        let config = test_config(&client_id);

        let manager = DownstreamOauthManager::new(config.clone());
        manager
            .exchange_client_credentials(&client_id, Some("secret-123"), Some("tools:read"))
            .await
            .expect("client credentials exchange");

        let path = state_file_path(&config).expect("compute state file path");
        let meta = std::fs::metadata(&path).expect("state file exists after persist");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "state file must be owner-only after a normal persist"
        );

        cleanup_state(&client_id);
    }
}
