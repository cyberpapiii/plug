use std::collections::HashMap;
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
    pub refresh_token: String,
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
        })
    }
}

impl DownstreamOauthManager {
    pub fn new(config: DownstreamOauthConfig) -> Self {
        let state = load_persisted_state(&config).unwrap_or_default();
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
        guard.pending_codes.insert(auth_code.clone(), pending);
        persist_state(&self.config, &guard);

        let separator = if redirect_uri.contains('?') { '&' } else { '?' };
        Ok(format!(
            "{redirect_uri}{separator}code={auth_code}&state={state}"
        ))
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
        let pending = guard
            .pending_codes
            .remove(code)
            .ok_or(DownstreamOauthError::InvalidGrant)?;

        if pending.client_id != client_id || pending.redirect_uri != redirect_uri {
            return Err(DownstreamOauthError::InvalidGrant);
        }
        if pending.expires_at < epoch_secs() {
            return Err(DownstreamOauthError::TokenExpired);
        }

        let verifier = oauth2::PkceCodeVerifier::new(code_verifier.to_string());
        let computed = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        if computed.as_str() != pending.code_challenge {
            return Err(DownstreamOauthError::PkceVerificationFailed);
        }

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
            refresh_token,
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
            refresh_token: refresh_token.to_string(),
            expires_in: ACCESS_TOKEN_LIFETIME_SECS,
            scope: scope_string(&refresh.scopes),
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
    let safe = crate::config::sanitize_server_name_for_path(client)
        .map_err(|e| format!("invalid downstream oauth client id for file path: {e}"))?;
    Ok(crate::config::config_dir()
        .join("downstream_oauth")
        .join(format!("{safe}.json")))
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
        Err(_) => return,
    };
    let dir = match path.parent() {
        Some(dir) => dir,
        None => return,
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }

    let mut persisted = state.clone();
    persisted.pending_codes.clear();

    let json = match serde_json::to_string_pretty(&persisted) {
        Ok(json) => json,
        Err(_) => return,
    };

    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp);
        let mut file = match file {
            Ok(file) => file,
            Err(_) => return,
        };
        if file.write_all(json.as_bytes()).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        if std::fs::write(&tmp, &json).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
    }

    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
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
        }
    }

    fn cleanup_state(client_id: &str) {
        let config = test_config(client_id);
        if let Ok(path) = state_file_path(&config) {
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
        let refreshed = recreated
            .exchange_refresh_token(&client_id, Some("secret-123"), &issued.refresh_token)
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
}
