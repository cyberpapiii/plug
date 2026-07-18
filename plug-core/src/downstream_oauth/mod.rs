//! Standards-based downstream OAuth for remote MCP clients.
//!
//! Dynamic registrations, grants, and tokens are issuer-scoped and persisted
//! in one owner-only file. There is deliberately no compatibility path for the
//! former single configured client: upgrading is a clean security boundary.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt};

use crate::config::HttpConfig;

const STATE_VERSION: u8 = 2;
const AUTH_REQUEST_LIFETIME_SECS: u64 = 300;
const AUTH_CODE_LIFETIME_SECS: u64 = 300;
const ACCESS_TOKEN_LIFETIME_SECS: u64 = 3600;
const REFRESH_TOKEN_LIFETIME_SECS: u64 = 30 * 24 * 3600;
const REGISTRATION_LIFETIME_SECS: u64 = 90 * 24 * 3600;
const UNACTIVATED_REGISTRATION_LIFETIME_SECS: u64 = 3600;
const MAX_REGISTRATIONS: usize = 100;
const REGISTRATION_RATE_WINDOW_SECS: u64 = 3600;
const MAX_REGISTRATIONS_PER_WINDOW: usize = 10;
const MAX_PENDING_CONSENTS: usize = 200;
const MAX_PENDING_CONSENTS_PER_CLIENT: usize = 5;
const MAX_ACCESS_TOKENS_PER_CLIENT: usize = 10;
const MAX_REGISTRATION_RATE_KEYS: usize = 10_000;
const MAX_METADATA_DOCUMENT_BYTES: usize = 64 * 1024;
const CURSOR_NATIVE_REDIRECT: &str = "cursor://anysphere.cursor-mcp/oauth/callback";

#[derive(Debug, Clone)]
pub struct DownstreamOauthConfig {
    pub public_base_url: String,
    pub oauth_scopes: Vec<String>,
    pub local_port: u16,
}

impl DownstreamOauthConfig {
    pub fn from_http_config(http: &HttpConfig) -> Option<Self> {
        if http.auth_mode != crate::config::DownstreamAuthMode::Oauth {
            return None;
        }
        Some(Self {
            public_base_url: http.public_base_url.clone()?,
            oauth_scopes: http.oauth_scopes.clone().unwrap_or_default(),
            local_port: http.port,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DownstreamOauthManager {
    pub config: DownstreamOauthConfig,
    state: Arc<Mutex<DownstreamOauthState>>,
    registration_rate: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    state_path: Arc<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientRegistrationRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    pub grant_types: Option<Vec<String>>,
    #[serde(default)]
    pub response_types: Option<Vec<String>>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientRegistrationResponse {
    pub client_id: String,
    pub client_id_issued_at: u64,
    pub redirect_uris: Vec<String>,
    pub client_name: String,
    pub token_endpoint_auth_method: String,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredClientSummary {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub source: ClientSource,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientSource {
    DynamicRegistration,
    MetadataDocument,
}

#[derive(Debug, Clone)]
pub struct AuthorizationRequest<'a> {
    pub response_type: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub code_challenge_method: &'a str,
    pub scope: Option<&'a str>,
    pub resource: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsentRequest {
    pub consent_id: String,
    pub client_id: String,
    pub client_name: String,
    pub redirect_host: String,
    pub scopes: Vec<String>,
    pub resource: String,
}

#[derive(Debug, Clone)]
pub struct AuthorizationRedirect {
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenResponsePayload {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTokenClaims {
    pub client_id: String,
    pub scopes: Vec<String>,
    pub resource: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessTokenValidation {
    Valid(AccessTokenClaims),
    Invalid,
    InsufficientScope,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DownstreamOauthError {
    #[error("invalid client")]
    InvalidClient,
    #[error("invalid client metadata")]
    InvalidClientMetadata,
    #[error("invalid redirect URI")]
    InvalidRedirectUri,
    #[error("invalid authorization request")]
    InvalidAuthorizationRequest,
    #[error("access denied")]
    AccessDenied,
    #[error("invalid grant")]
    InvalidGrant,
    #[error("PKCE verification failed")]
    PkceVerificationFailed,
    #[error("unsupported grant type")]
    UnsupportedGrantType,
    #[error("unsupported client authentication method")]
    UnsupportedClientAuthMethod,
    #[error("registration rate limit exceeded")]
    RateLimited,
    #[error("registration quota exceeded")]
    RegistrationQuotaExceeded,
    #[error("requested scope is not allowed")]
    InvalidScope,
    #[error("invalid resource")]
    InvalidResource,
    #[error("OAuth state persistence failed: {0}")]
    Persistence(String),
    #[error("client metadata document fetch failed")]
    MetadataFetch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegisteredClient {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    source: ClientSource,
    created_at: u64,
    last_used_at: Option<u64>,
    expires_at: u64,
}

impl From<&RegisteredClient> for RegisteredClientSummary {
    fn from(value: &RegisteredClient) -> Self {
        Self {
            client_id: value.client_id.clone(),
            client_name: value.client_name.clone(),
            redirect_uris: value.redirect_uris.clone(),
            source: value.source.clone(),
            created_at: value.created_at,
            last_used_at: value.last_used_at,
            expires_at: value.expires_at,
        }
    }
}

#[derive(Debug, Clone)]
struct PendingConsent {
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    scopes: Vec<String>,
    resource: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingAuthorizationCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    scopes: Vec<String>,
    resource: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuedAccessToken {
    client_id: String,
    scopes: Vec<String>,
    resource: String,
    issued_at: u64,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssuedRefreshToken {
    client_id: String,
    scopes: Vec<String>,
    resource: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownstreamOauthState {
    version: u8,
    #[serde(default)]
    clients: HashMap<String, RegisteredClient>,
    #[serde(skip)]
    pending_consents: HashMap<String, PendingConsent>,
    #[serde(skip)]
    pending_codes: HashMap<String, PendingAuthorizationCode>,
    #[serde(default)]
    access_tokens: HashMap<String, IssuedAccessToken>,
    #[serde(default)]
    refresh_tokens: HashMap<String, IssuedRefreshToken>,
    #[serde(default)]
    revoked_client_ids: HashSet<String>,
}

impl Default for DownstreamOauthState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            clients: HashMap::new(),
            pending_consents: HashMap::new(),
            pending_codes: HashMap::new(),
            access_tokens: HashMap::new(),
            refresh_tokens: HashMap::new(),
            revoked_client_ids: HashSet::new(),
        }
    }
}

impl DownstreamOauthState {
    fn evict_expired(&mut self, now: u64) {
        let expired: HashSet<String> = self
            .clients
            .iter()
            .filter(|(_, client)| client.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        self.clients.retain(|id, _| !expired.contains(id));
        self.pending_consents
            .retain(|_, item| item.expires_at > now);
        self.pending_codes.retain(|_, item| item.expires_at > now);
        self.access_tokens
            .retain(|_, item| item.expires_at > now && !expired.contains(&item.client_id));
        self.refresh_tokens
            .retain(|_, item| item.expires_at > now && !expired.contains(&item.client_id));
    }

    fn remove_client_material(&mut self, client_id: &str) {
        self.clients.remove(client_id);
        self.pending_consents
            .retain(|_, item| item.client_id != client_id);
        self.pending_codes
            .retain(|_, item| item.client_id != client_id);
        self.access_tokens
            .retain(|_, item| item.client_id != client_id);
        self.refresh_tokens
            .retain(|_, item| item.client_id != client_id);
    }
}

#[derive(Debug, Deserialize)]
struct ClientMetadataDocument {
    client_id: String,
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}

impl DownstreamOauthManager {
    pub fn new(config: DownstreamOauthConfig) -> Self {
        Self::try_new(config).expect("downstream OAuth state must be readable")
    }

    pub fn try_new(config: DownstreamOauthConfig) -> Result<Self, DownstreamOauthError> {
        let path = state_file_path(&config);
        Self::new_with_state_path(config, path)
    }

    #[doc(hidden)]
    pub fn new_with_state_path(
        config: DownstreamOauthConfig,
        state_path: PathBuf,
    ) -> Result<Self, DownstreamOauthError> {
        let state = load_persisted_state(&state_path)?;
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(state)),
            registration_rate: Arc::new(Mutex::new(HashMap::new())),
            state_path: Arc::new(state_path),
        })
    }

    pub fn base_url(&self) -> &str {
        self.config.public_base_url.trim_end_matches('/')
    }

    pub fn resource(&self) -> String {
        format!("{}/mcp", self.base_url())
    }

    pub fn authorization_endpoint(&self) -> String {
        format!("{}/oauth/authorize", self.base_url())
    }

    pub fn token_endpoint(&self) -> String {
        format!("{}/oauth/token", self.base_url())
    }

    pub fn registration_endpoint(&self) -> String {
        format!("{}/oauth/register", self.base_url())
    }

    pub fn local_consent_endpoint(&self) -> String {
        format!(
            "http://127.0.0.1:{}/_plug/oauth/authorize",
            self.config.local_port
        )
    }

    pub fn local_approval_request_allowed(&self, headers: &axum::http::HeaderMap) -> bool {
        if headers.contains_key("cf-connecting-ip")
            || headers.contains_key("x-forwarded-for")
            || headers.contains_key("forwarded")
        {
            return false;
        }
        let Some(host) = headers
            .get(axum::http::header::HOST)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let expected_port = self.config.local_port.to_string();
        host == format!("127.0.0.1:{expected_port}")
            || host == format!("localhost:{expected_port}")
            || host == format!("[::1]:{expected_port}")
    }

    pub async fn register_client(
        &self,
        mut request: ClientRegistrationRequest,
        rate_key: &str,
    ) -> Result<ClientRegistrationResponse, DownstreamOauthError> {
        request.redirect_uris.retain(|uri| valid_redirect_uri(uri));
        request.redirect_uris.sort();
        request.redirect_uris.dedup();
        validate_registration_request(&request)?;
        self.check_registration_rate(rate_key).await?;

        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        next.evict_expired(now);
        if next.clients.len() >= MAX_REGISTRATIONS {
            return Err(DownstreamOauthError::RegistrationQuotaExceeded);
        }

        let client_id = format!("plug_{}", opaque_value());
        let client_name = safe_client_name(request.client_name.as_deref());
        let client = RegisteredClient {
            client_id: client_id.clone(),
            client_name: client_name.clone(),
            redirect_uris: request.redirect_uris.clone(),
            source: ClientSource::DynamicRegistration,
            created_at: now,
            last_used_at: None,
            expires_at: now + UNACTIVATED_REGISTRATION_LIFETIME_SECS,
        };
        next.clients.insert(client_id.clone(), client);
        persist_state(&self.state_path, &next)?;
        *guard = next;

        Ok(ClientRegistrationResponse {
            client_id,
            client_id_issued_at: now,
            redirect_uris: request.redirect_uris,
            client_name,
            token_endpoint_auth_method: "none".to_string(),
            grant_types: vec![
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ],
            response_types: vec!["code".to_string()],
        })
    }

    pub async fn list_clients(&self) -> Vec<RegisteredClientSummary> {
        let mut clients = self
            .state
            .lock()
            .await
            .clients
            .values()
            .map(RegisteredClientSummary::from)
            .collect::<Vec<_>>();
        clients.sort_by(|a, b| {
            a.client_name
                .cmp(&b.client_name)
                .then(a.client_id.cmp(&b.client_id))
        });
        clients
    }

    pub async fn revoke_client(&self, client_id: &str) -> Result<bool, DownstreamOauthError> {
        let mut guard = self.state.lock().await;
        let existed = guard.clients.contains_key(client_id);
        if !existed {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.remove_client_material(client_id);
        next.revoked_client_ids.insert(client_id.to_string());
        persist_state(&self.state_path, &next)?;
        *guard = next;
        Ok(existed)
    }

    pub async fn begin_authorization(
        &self,
        request: AuthorizationRequest<'_>,
    ) -> Result<ConsentRequest, DownstreamOauthError> {
        if request.response_type != "code"
            || request.state.is_empty()
            || request.code_challenge.is_empty()
            || request.code_challenge_method != "S256"
        {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        self.ensure_client(request.client_id).await?;
        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        guard.evict_expired(now);
        let client = guard
            .clients
            .get(request.client_id)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidClient)?;
        if !client
            .redirect_uris
            .iter()
            .any(|uri| uri == request.redirect_uri)
        {
            return Err(DownstreamOauthError::InvalidRedirectUri);
        }
        if request.resource != self.resource() {
            return Err(DownstreamOauthError::InvalidResource);
        }
        let scopes = self.validate_scopes(request.scope)?;
        if guard.pending_consents.len() >= MAX_PENDING_CONSENTS
            || guard
                .pending_consents
                .values()
                .filter(|pending| pending.client_id == request.client_id)
                .count()
                >= MAX_PENDING_CONSENTS_PER_CLIENT
        {
            return Err(DownstreamOauthError::RateLimited);
        }
        let redirect_host = url::Url::parse(request.redirect_uri)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))
            .ok_or(DownstreamOauthError::InvalidRedirectUri)?;
        let consent_id = opaque_value();
        guard.pending_consents.insert(
            consent_id.clone(),
            PendingConsent {
                client_id: request.client_id.to_string(),
                redirect_uri: request.redirect_uri.to_string(),
                state: request.state.to_string(),
                code_challenge: request.code_challenge.to_string(),
                scopes: scopes.clone(),
                resource: request.resource.to_string(),
                expires_at: now + AUTH_REQUEST_LIFETIME_SECS,
            },
        );
        Ok(ConsentRequest {
            consent_id,
            client_id: request.client_id.to_string(),
            client_name: client.client_name,
            redirect_host,
            scopes,
            resource: request.resource.to_string(),
        })
    }

    pub async fn decide_consent(
        &self,
        consent_id: &str,
        approved: bool,
    ) -> Result<AuthorizationRedirect, DownstreamOauthError> {
        let mut guard = self.state.lock().await;
        let consent = guard
            .pending_consents
            .remove(consent_id)
            .ok_or(DownstreamOauthError::InvalidAuthorizationRequest)?;
        if consent.expires_at <= epoch_secs() {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if !approved {
            return Ok(AuthorizationRedirect {
                location: redirect_with_params(
                    &consent.redirect_uri,
                    &[("error", "access_denied"), ("state", &consent.state)],
                ),
            });
        }

        let mut next = guard.clone();
        let code = opaque_value();
        next.pending_codes.insert(
            code.clone(),
            PendingAuthorizationCode {
                client_id: consent.client_id.clone(),
                redirect_uri: consent.redirect_uri.clone(),
                code_challenge: consent.code_challenge,
                scopes: consent.scopes,
                resource: consent.resource,
                expires_at: epoch_secs() + AUTH_CODE_LIFETIME_SECS,
            },
        );
        if let Some(client) = next.clients.get_mut(&consent.client_id) {
            client.last_used_at = Some(epoch_secs());
            client.expires_at = epoch_secs() + REGISTRATION_LIFETIME_SECS;
        }
        // Codes are intentionally memory-only, but persisting the client use
        // timestamp must succeed before the code is handed out.
        persist_state(&self.state_path, &next)?;
        *guard = next;
        Ok(AuthorizationRedirect {
            location: redirect_with_params(
                &consent.redirect_uri,
                &[("code", &code), ("state", &consent.state)],
            ),
        })
    }

    pub async fn exchange_authorization_code(
        &self,
        client_id: &str,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
        resource: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.validate_public_client(client_id).await?;
        if resource != self.resource() {
            return Err(DownstreamOauthError::InvalidResource);
        }
        let mut guard = self.state.lock().await;
        let pending = guard
            .pending_codes
            .get(code)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidGrant)?;
        if pending.client_id != client_id
            || pending.redirect_uri != redirect_uri
            || pending.resource != resource
            || pending.expires_at <= epoch_secs()
        {
            return Err(DownstreamOauthError::InvalidGrant);
        }
        let verifier = oauth2::PkceCodeVerifier::new(code_verifier.to_string());
        let challenge = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        if challenge.as_str() != pending.code_challenge {
            return Err(DownstreamOauthError::PkceVerificationFailed);
        }

        let mut next = guard.clone();
        next.pending_codes.remove(code);
        let token = issue_token_pair(
            &self.state_path,
            &mut next,
            client_id,
            &pending.scopes,
            resource,
        )?;
        *guard = next;
        Ok(token)
    }

    pub async fn exchange_refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        resource: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.validate_public_client(client_id).await?;
        if resource != self.resource() {
            return Err(DownstreamOauthError::InvalidResource);
        }
        let mut guard = self.state.lock().await;
        let refresh = guard
            .refresh_tokens
            .get(refresh_token)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidGrant)?;
        if refresh.client_id != client_id
            || refresh.resource != resource
            || refresh.expires_at <= epoch_secs()
        {
            return Err(DownstreamOauthError::InvalidGrant);
        }
        let mut next = guard.clone();
        next.refresh_tokens.remove(refresh_token);
        let token = issue_token_pair(
            &self.state_path,
            &mut next,
            client_id,
            &refresh.scopes,
            resource,
        )?;
        *guard = next;
        Ok(token)
    }

    pub async fn validate_access_token_for(
        &self,
        token: &str,
        required_scopes: &[String],
        resource: &str,
    ) -> AccessTokenValidation {
        let guard = self.state.lock().await;
        let Some(record) = guard.access_tokens.get(token) else {
            return AccessTokenValidation::Invalid;
        };
        if record.expires_at <= epoch_secs()
            || record.resource != resource
            || !guard.clients.contains_key(&record.client_id)
        {
            return AccessTokenValidation::Invalid;
        }
        if required_scopes
            .iter()
            .any(|scope| !record.scopes.contains(scope))
        {
            return AccessTokenValidation::InsufficientScope;
        }
        AccessTokenValidation::Valid(AccessTokenClaims {
            client_id: record.client_id.clone(),
            scopes: record.scopes.clone(),
            resource: record.resource.clone(),
        })
    }

    pub async fn client_redirect_allowed(&self, client_id: &str, redirect_uri: &str) -> bool {
        self.state
            .lock()
            .await
            .clients
            .get(client_id)
            .is_some_and(|client| {
                client.expires_at > epoch_secs()
                    && client
                        .redirect_uris
                        .iter()
                        .any(|registered| registered == redirect_uri)
            })
    }

    async fn validate_public_client(&self, client_id: &str) -> Result<(), DownstreamOauthError> {
        let guard = self.state.lock().await;
        match guard.clients.get(client_id) {
            Some(client) if client.expires_at > epoch_secs() => Ok(()),
            _ => Err(DownstreamOauthError::InvalidClient),
        }
    }

    fn validate_scopes(
        &self,
        requested: Option<&str>,
    ) -> Result<Vec<String>, DownstreamOauthError> {
        let scopes = requested
            .map(|value| value.split_whitespace().map(ToString::to_string).collect())
            .unwrap_or_else(|| self.config.oauth_scopes.clone());
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|scope| !self.config.oauth_scopes.contains(scope))
        {
            return Err(DownstreamOauthError::InvalidScope);
        }
        let mut canonical = scopes;
        canonical.sort();
        canonical.dedup();
        Ok(canonical)
    }

    async fn check_registration_rate(&self, key: &str) -> Result<(), DownstreamOauthError> {
        let now = epoch_secs();
        let mut rate = self.registration_rate.lock().await;
        rate.retain(|_, events| {
            while events
                .front()
                .is_some_and(|seen| *seen + REGISTRATION_RATE_WINDOW_SECS <= now)
            {
                events.pop_front();
            }
            !events.is_empty()
        });
        if !rate.contains_key(key) && rate.len() >= MAX_REGISTRATION_RATE_KEYS {
            return Err(DownstreamOauthError::RateLimited);
        }
        let events = rate.entry(key.to_string()).or_default();
        if events.len() >= MAX_REGISTRATIONS_PER_WINDOW {
            return Err(DownstreamOauthError::RateLimited);
        }
        events.push_back(now);
        Ok(())
    }

    async fn ensure_client(&self, client_id: &str) -> Result<(), DownstreamOauthError> {
        let existing_source = {
            let guard = self.state.lock().await;
            if guard.revoked_client_ids.contains(client_id) {
                return Err(DownstreamOauthError::InvalidClient);
            }
            guard
                .clients
                .get(client_id)
                .filter(|client| client.expires_at > epoch_secs())
                .map(|client| client.source.clone())
        };
        if matches!(existing_source, Some(ClientSource::DynamicRegistration)) {
            return Ok(());
        }

        let document = match fetch_client_metadata_document(client_id).await {
            Ok(document) => document,
            Err(_) if matches!(existing_source, Some(ClientSource::MetadataDocument)) => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        validate_metadata_document(client_id, &document)?;
        let now = epoch_secs();
        let client = RegisteredClient {
            client_id: client_id.to_string(),
            client_name: safe_client_name(document.client_name.as_deref()),
            redirect_uris: document.redirect_uris,
            source: ClientSource::MetadataDocument,
            created_at: now,
            last_used_at: None,
            expires_at: now + REGISTRATION_LIFETIME_SECS,
        };
        let mut guard = self.state.lock().await;
        if guard.revoked_client_ids.contains(client_id) {
            return Err(DownstreamOauthError::InvalidClient);
        }
        let mut next = guard.clone();
        next.evict_expired(now);
        if !next.clients.contains_key(client_id) && next.clients.len() >= MAX_REGISTRATIONS {
            return Err(DownstreamOauthError::RegistrationQuotaExceeded);
        }
        next.clients.insert(client_id.to_string(), client);
        persist_state(&self.state_path, &next)?;
        *guard = next;
        Ok(())
    }
}

fn validate_registration_request(
    request: &ClientRegistrationRequest,
) -> Result<(), DownstreamOauthError> {
    if request.redirect_uris.is_empty()
        || request.redirect_uris.len() > 10
        || request
            .redirect_uris
            .iter()
            .any(|uri| !valid_redirect_uri(uri))
        || request
            .token_endpoint_auth_method
            .as_deref()
            .unwrap_or("none")
            != "none"
        || request.grant_types.as_ref().is_some_and(|items| {
            items
                .iter()
                .any(|item| item != "authorization_code" && item != "refresh_token")
        })
        || request
            .response_types
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item != "code"))
    {
        return Err(DownstreamOauthError::InvalidClientMetadata);
    }
    Ok(())
}

fn safe_client_name(value: Option<&str>) -> String {
    let sanitized = value
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect::<String>();
    if sanitized.is_empty() {
        "MCP client".to_string()
    } else {
        sanitized
    }
}

fn valid_redirect_uri(value: &str) -> bool {
    // Cursor Remote Control is a native app and uses this reverse-domain,
    // application-claimed callback. Keep the exception exact; arbitrary custom
    // schemes remain invalid because they can be claimed by another app.
    if value == CURSOR_NATIVE_REDIRECT {
        return true;
    }
    let Ok(uri) = url::Url::parse(value) else {
        return false;
    };
    if !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
        || uri.host_str().is_none()
    {
        return false;
    }
    match uri.scheme() {
        "https" => true,
        "http" => uri.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn validate_metadata_document(
    expected_client_id: &str,
    document: &ClientMetadataDocument,
) -> Result<(), DownstreamOauthError> {
    let request = ClientRegistrationRequest {
        redirect_uris: document.redirect_uris.clone(),
        client_name: document.client_name.clone(),
        token_endpoint_auth_method: document.token_endpoint_auth_method.clone(),
        grant_types: document.grant_types.clone(),
        response_types: document.response_types.clone(),
        scope: None,
    };
    if document.client_id != expected_client_id {
        return Err(DownstreamOauthError::InvalidClientMetadata);
    }
    validate_registration_request(&request)
}

async fn fetch_client_metadata_document(
    client_id: &str,
) -> Result<ClientMetadataDocument, DownstreamOauthError> {
    let url = url::Url::parse(client_id).map_err(|_| DownstreamOauthError::InvalidClient)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(DownstreamOauthError::InvalidClient);
    }
    let host = url.host_str().ok_or(DownstreamOauthError::InvalidClient)?;
    let port = url.port_or_known_default().unwrap_or(443);
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| DownstreamOauthError::MetadataFetch)?
        .collect::<Vec<SocketAddr>>();
    if resolved.is_empty()
        || resolved
            .iter()
            .any(|address| forbidden_metadata_ip(address.ip()))
    {
        return Err(DownstreamOauthError::MetadataFetch);
    }
    let pinned = resolved[0];
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .resolve(host, pinned)
        .build()
        .map_err(|_| DownstreamOauthError::MetadataFetch)?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| DownstreamOauthError::MetadataFetch)?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > MAX_METADATA_DOCUMENT_BYTES as u64)
    {
        return Err(DownstreamOauthError::MetadataFetch);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| DownstreamOauthError::MetadataFetch)?;
    if bytes.len() > MAX_METADATA_DOCUMENT_BYTES {
        return Err(DownstreamOauthError::MetadataFetch);
    }
    serde_json::from_slice(&bytes).map_err(|_| DownstreamOauthError::InvalidClientMetadata)
}

fn forbidden_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
                || octets[0] >= 240
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    }
}

fn issue_token_pair(
    state_path: &std::path::Path,
    state: &mut DownstreamOauthState,
    client_id: &str,
    scopes: &[String],
    resource: &str,
) -> Result<TokenResponsePayload, DownstreamOauthError> {
    let access_token = opaque_value();
    let refresh_token = opaque_value();
    let now = epoch_secs();
    let mut existing = state
        .access_tokens
        .iter()
        .filter(|(_, token)| token.client_id == client_id)
        .map(|(value, token)| (value.clone(), token.issued_at))
        .collect::<Vec<_>>();
    existing.sort_by_key(|(_, issued_at)| *issued_at);
    let remove_count = existing
        .len()
        .saturating_sub(MAX_ACCESS_TOKENS_PER_CLIENT - 1);
    for (value, _) in existing.into_iter().take(remove_count) {
        state.access_tokens.remove(&value);
    }
    state.access_tokens.insert(
        access_token.clone(),
        IssuedAccessToken {
            client_id: client_id.to_string(),
            scopes: scopes.to_vec(),
            resource: resource.to_string(),
            issued_at: now,
            expires_at: now + ACCESS_TOKEN_LIFETIME_SECS,
        },
    );
    state.refresh_tokens.insert(
        refresh_token.clone(),
        IssuedRefreshToken {
            client_id: client_id.to_string(),
            scopes: scopes.to_vec(),
            resource: resource.to_string(),
            expires_at: now + REFRESH_TOKEN_LIFETIME_SECS,
        },
    );
    persist_state(state_path, state)?;
    Ok(TokenResponsePayload {
        access_token,
        refresh_token: Some(refresh_token),
        expires_in: ACCESS_TOKEN_LIFETIME_SECS,
        scope: (!scopes.is_empty()).then(|| scopes.join(" ")),
    })
}

fn redirect_with_params(base: &str, pairs: &[(&str, &str)]) -> String {
    let query = pairs
        .iter()
        .fold(
            url::form_urlencoded::Serializer::new(String::new()),
            |mut serializer, (key, value)| {
                serializer.append_pair(key, value);
                serializer
            },
        )
        .finish();
    format!(
        "{base}{}{query}",
        if base.contains('?') { '&' } else { '?' }
    )
}

fn opaque_value() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn resource_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| scope.as_str() != "offline_access")
        .cloned()
        .collect()
}

fn state_file_path(config: &DownstreamOauthConfig) -> PathBuf {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(config.public_base_url.trim_end_matches('/').as_bytes());
    crate::config::config_dir()
        .join("downstream_oauth")
        .join(format!(
            "issuer-v{STATE_VERSION}-{}.json",
            hex::encode(&digest[..8])
        ))
}

fn load_persisted_state(
    path: &std::path::Path,
) -> Result<DownstreamOauthState, DownstreamOauthError> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DownstreamOauthState::default());
        }
        Err(error) => return Err(DownstreamOauthError::Persistence(error.to_string())),
    };
    let mut state: DownstreamOauthState = serde_json::from_str(&data)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    if state.version != STATE_VERSION {
        return Err(DownstreamOauthError::Persistence(
            "unsupported downstream OAuth state version".to_string(),
        ));
    }
    state.pending_consents.clear();
    state.pending_codes.clear();
    state.evict_expired(epoch_secs());
    Ok(state)
}

fn persist_state(
    path: &std::path::Path,
    state: &DownstreamOauthState,
) -> Result<(), DownstreamOauthError> {
    let dir = path
        .parent()
        .ok_or_else(|| DownstreamOauthError::Persistence("invalid state path".to_string()))?;
    crate::fs_perm::ensure_dir_0700(dir)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    let json = serde_json::to_vec_pretty(state)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    {
        use std::io::Write as _;
        let mut file = options
            .open(&tmp)
            .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
        file.write_all(&json)
            .and_then(|_| file.sync_all())
            .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    }
    std::fs::rename(&tmp, path)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DownstreamOauthConfig {
        DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
            local_port: 3282,
        }
    }

    fn test_manager() -> (DownstreamOauthManager, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "plug-downstream-oauth-{}.json",
            uuid::Uuid::new_v4()
        ));
        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("test manager");
        (manager, path)
    }

    async fn register(
        manager: &DownstreamOauthManager,
        name: &str,
        redirect: &str,
    ) -> ClientRegistrationResponse {
        manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![redirect.to_string()],
                    client_name: Some(name.to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec![
                        "authorization_code".to_string(),
                        "refresh_token".to_string(),
                    ]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "test",
            )
            .await
            .expect("register client")
    }

    async fn issue_tokens(
        manager: &DownstreamOauthManager,
        client: &ClientRegistrationResponse,
    ) -> TokenResponsePayload {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-123",
                code_challenge: challenge,
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");
        let redirect = manager
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("approve consent");
        let parsed = url::Url::parse(&redirect.location).expect("redirect URL");
        let code = parsed
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("authorization code");
        manager
            .exchange_authorization_code(
                &client.client_id,
                &code,
                &client.redirect_uris[0],
                verifier,
                "https://plug.example.com/mcp",
            )
            .await
            .expect("exchange code")
    }

    #[test]
    fn redirect_validation_accepts_web_loopback_and_exact_cursor_native_callback() {
        assert!(valid_redirect_uri("https://client.example/callback"));
        assert!(valid_redirect_uri("http://127.0.0.1:8787/callback"));
        assert!(valid_redirect_uri("http://localhost:8787/callback"));
        assert!(valid_redirect_uri(CURSOR_NATIVE_REDIRECT));
        assert!(!valid_redirect_uri("http://client.example/callback"));
        assert!(!valid_redirect_uri("cursor://callback"));
        assert!(!valid_redirect_uri(
            "cursor://anysphere.cursor-mcp/other-callback"
        ));
        assert!(!valid_redirect_uri(
            "https://user:pass@client.example/callback"
        ));
        assert!(!valid_redirect_uri(
            "https://client.example/callback#fragment"
        ));
    }

    #[test]
    fn opaque_values_have_256_bits_of_random_input() {
        let first = opaque_value();
        let second = opaque_value();
        assert_ne!(first, second);
        assert_eq!(first.len(), 43);
    }

    #[tokio::test]
    async fn cursor_style_registration_and_rotating_grants_are_client_isolated() {
        let (manager, _) = test_manager();
        let cursor = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let claude = register(
            &manager,
            "Claude",
            "https://claude.ai/api/mcp/auth_callback",
        )
        .await;
        assert_ne!(cursor.client_id, claude.client_id);
        assert!(cursor.client_id.starts_with("plug_"));
        assert_eq!(cursor.token_endpoint_auth_method, "none");

        let cursor_tokens = issue_tokens(&manager, &cursor).await;
        let access = cursor_tokens.access_token.clone();
        assert!(matches!(
            manager
                .validate_access_token_for(
                    &access,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp"
                )
                .await,
            AccessTokenValidation::Valid(_)
        ));
        assert_eq!(
            manager
                .exchange_refresh_token(
                    &claude.client_id,
                    cursor_tokens.refresh_token.as_deref().expect("refresh"),
                    "https://plug.example.com/mcp"
                )
                .await,
            Err(DownstreamOauthError::InvalidGrant)
        );

        let rotated = manager
            .exchange_refresh_token(
                &cursor.client_id,
                cursor_tokens.refresh_token.as_deref().expect("refresh"),
                "https://plug.example.com/mcp",
            )
            .await
            .expect("rotate refresh token");
        assert_ne!(rotated.refresh_token, cursor_tokens.refresh_token);
        assert_eq!(
            manager
                .exchange_refresh_token(
                    &cursor.client_id,
                    cursor_tokens.refresh_token.as_deref().expect("refresh"),
                    "https://plug.example.com/mcp"
                )
                .await,
            Err(DownstreamOauthError::InvalidGrant)
        );
        assert!(matches!(
            manager
                .validate_access_token_for(
                    &access,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp"
                )
                .await,
            AccessTokenValidation::Valid(_)
        ));
    }

    #[tokio::test]
    async fn exact_redirect_pkce_scope_and_resource_are_enforced() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        for (redirect, method, scope, resource, expected) in [
            (
                "http://localhost:8788/callback",
                "S256",
                "tools:read",
                "https://plug.example.com/mcp",
                DownstreamOauthError::InvalidRedirectUri,
            ),
            (
                "http://localhost:8787/callback",
                "plain",
                "tools:read",
                "https://plug.example.com/mcp",
                DownstreamOauthError::InvalidAuthorizationRequest,
            ),
            (
                "http://localhost:8787/callback",
                "S256",
                "tools:write",
                "https://plug.example.com/mcp",
                DownstreamOauthError::InvalidScope,
            ),
            (
                "http://localhost:8787/callback",
                "S256",
                "tools:read",
                "https://other.example/mcp",
                DownstreamOauthError::InvalidResource,
            ),
        ] {
            let result = manager
                .begin_authorization(AuthorizationRequest {
                    response_type: "code",
                    client_id: &client.client_id,
                    redirect_uri: redirect,
                    state: "state",
                    code_challenge: "challenge",
                    code_challenge_method: method,
                    scope: Some(scope),
                    resource,
                })
                .await;
            assert_eq!(result.expect_err("request must fail"), expected);
        }
    }

    #[tokio::test]
    async fn registrations_tokens_persist_and_revocation_survives_restart() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let tokens = issue_tokens(&manager, &client).await;
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("restart manager");
        assert_eq!(restarted.list_clients().await.len(), 1);
        assert!(matches!(
            restarted
                .validate_access_token_for(
                    &tokens.access_token,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp"
                )
                .await,
            AccessTokenValidation::Valid(_)
        ));
        assert!(
            restarted
                .revoke_client(&client.client_id)
                .await
                .expect("revoke")
        );
        let after_revoke = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("restart after revoke");
        assert!(after_revoke.list_clients().await.is_empty());
        assert!(matches!(
            after_revoke
                .validate_access_token_for(
                    &tokens.access_token,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp"
                )
                .await,
            AccessTokenValidation::Invalid
        ));
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn registration_security_rate_limit_and_expiry_cleanup_work() {
        let (manager, _) = test_manager();
        for unsafe_redirect in [
            "http://evil.example/callback",
            "cursor://callback",
            "https://user:pass@client.example/callback",
            "https://client.example/callback#fragment",
        ] {
            let result = manager
                .register_client(
                    ClientRegistrationRequest {
                        redirect_uris: vec![unsafe_redirect.to_string()],
                        client_name: Some("Unsafe".to_string()),
                        token_endpoint_auth_method: Some("none".to_string()),
                        grant_types: None,
                        response_types: None,
                        scope: None,
                    },
                    unsafe_redirect,
                )
                .await;
            assert_eq!(result, Err(DownstreamOauthError::InvalidClientMetadata));
        }

        let mixed = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![
                        "cursor://callback".to_string(),
                        "http://localhost:8787/callback".to_string(),
                    ],
                    client_name: Some("Cursor".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: None,
                    response_types: None,
                    scope: None,
                },
                "mixed",
            )
            .await
            .expect("mixed registration keeps only safe callbacks");
        assert_eq!(
            mixed.redirect_uris,
            vec!["http://localhost:8787/callback".to_string()]
        );

        for index in 0..MAX_REGISTRATIONS_PER_WINDOW {
            register(
                &manager,
                &format!("client-{index}"),
                "http://localhost:8787/callback",
            )
            .await;
        }
        let limited = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec!["http://localhost:8787/callback".to_string()],
                    client_name: Some("limited".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: None,
                    response_types: None,
                    scope: None,
                },
                "test",
            )
            .await;
        assert_eq!(limited, Err(DownstreamOauthError::RateLimited));

        let expired_id = manager.list_clients().await[0].client_id.clone();
        {
            let mut state = manager.state.lock().await;
            state
                .clients
                .get_mut(&expired_id)
                .expect("client")
                .expires_at = 0;
        }
        manager.registration_rate.lock().await.clear();
        register(
            &manager,
            "cleanup-trigger",
            "http://localhost:8787/callback",
        )
        .await;
        assert!(
            !manager
                .list_clients()
                .await
                .iter()
                .any(|client| client.client_id == expired_id)
        );
    }

    #[tokio::test]
    async fn registration_quota_is_enforced() {
        let (manager, _) = test_manager();
        {
            let mut state = manager.state.lock().await;
            for index in 0..MAX_REGISTRATIONS {
                let id = format!("existing-{index}");
                state.clients.insert(
                    id.clone(),
                    RegisteredClient {
                        client_id: id,
                        client_name: "Existing".to_string(),
                        redirect_uris: vec!["http://localhost:8787/callback".to_string()],
                        source: ClientSource::DynamicRegistration,
                        created_at: epoch_secs(),
                        last_used_at: None,
                        expires_at: epoch_secs() + REGISTRATION_LIFETIME_SECS,
                    },
                );
            }
        }
        let result = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec!["http://localhost:8787/callback".to_string()],
                    client_name: Some("Over quota".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: None,
                    response_types: None,
                    scope: None,
                },
                "quota",
            )
            .await;
        assert_eq!(result, Err(DownstreamOauthError::RegistrationQuotaExceeded));
    }
}
