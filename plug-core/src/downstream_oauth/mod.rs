//! Standards-based downstream OAuth for remote MCP clients.
//!
//! Dynamic registrations, grants, and tokens are issuer-scoped and persisted
//! in one owner-only file. There is deliberately no compatibility path for the
//! former single configured client: upgrading is a clean security boundary.

pub mod owner;

pub use owner::{
    OwnerApprovalChallenge, OwnerAuthenticationCeremony, OwnerBootstrap, OwnerCredential,
    OwnerCredentialSummary, OwnerRegistrationCeremony, OwnerRegistrationChallenge, OwnerSecurity,
    PublicKeyCredential, RegisterPublicKeyCredential,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt};

use crate::config::HttpConfig;

const STATE_VERSION: u8 = 3;
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
const PARENT_DIR_SYNC_ATTEMPTS: usize = 3;
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
    pub owner_security: Arc<OwnerSecurity>,
    state: Arc<Mutex<DownstreamOauthState>>,
    principal_lifecycles: Arc<dashmap::DashMap<String, Arc<PrincipalLifecycleState>>>,
    registration_rate: Arc<Mutex<HashMap<String, VecDeque<u64>>>>,
    state_path: Arc<PathBuf>,
    durability_degraded: Arc<AtomicBool>,
    _state_lock: Arc<std::fs::File>,
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
    pub client_source: ClientSource,
    pub redirect_uri: String,
    pub redirect_host: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub csrf_token: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug)]
struct PrincipalLifecycleState {
    generation: AtomicU64,
    active: AtomicBool,
}

impl PrincipalLifecycleState {
    fn active() -> Self {
        Self {
            generation: AtomicU64::new(1),
            active: AtomicBool::new(true),
        }
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

/// Validation-time lease for a downstream OAuth principal.
///
/// The access token proves who the caller was when middleware validated it;
/// this lease additionally proves that the same client generation is still
/// active when durable work is admitted later in the request.
#[derive(Clone)]
pub struct PrincipalLifecycleLease {
    state: Arc<PrincipalLifecycleState>,
    generation: u64,
}

impl PrincipalLifecycleLease {
    pub fn is_active(&self) -> bool {
        self.state.active.load(Ordering::SeqCst)
            && self.state.generation.load(Ordering::SeqCst) == self.generation
    }

    #[cfg(test)]
    pub(crate) fn active_for_tests() -> Self {
        Self {
            state: Arc::new(PrincipalLifecycleState::active()),
            generation: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn deactivate_for_tests(&self) {
        self.state.deactivate();
    }
}

impl std::fmt::Debug for PrincipalLifecycleLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrincipalLifecycleLease")
            .field("generation", &self.generation)
            .field("active", &self.is_active())
            .finish()
    }
}

impl PartialEq for PrincipalLifecycleLease {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for PrincipalLifecycleLease {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTokenClaims {
    pub client_id: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub principal_lifecycle: PrincipalLifecycleLease,
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
    #[error("downstream OAuth owner is not enrolled")]
    OwnerNotEnrolled,
    #[error("invalid owner enrollment bootstrap")]
    InvalidOwnerBootstrap,
    #[error("owner passkey challenge expired")]
    OwnerChallengeExpired,
    #[error("invalid owner passkey assertion")]
    InvalidOwnerAssertion,
    #[error("owner credential limit exceeded")]
    OwnerCredentialLimit,
    #[error("owner credential not found")]
    OwnerCredentialNotFound,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingConsent {
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    scopes: Vec<String>,
    resource: String,
    #[serde(default)]
    csrf_token: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletedConsent {
    client_id: String,
    redirect: AuthorizationRedirect,
    expires_at: u64,
    #[serde(
        default,
        alias = "approval_ceremony_id",
        deserialize_with = "deserialize_approval_ceremony_ids"
    )]
    approval_ceremony_ids: Vec<String>,
}

fn deserialize_approval_ceremony_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(id)) => vec![id],
        Some(OneOrMany::Many(ids)) => ids,
        None => Vec::new(),
    })
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
    #[serde(default)]
    pending_consents: HashMap<String, PendingConsent>,
    #[serde(default)]
    completed_consents: HashMap<String, CompletedConsent>,
    #[serde(default)]
    pending_codes: HashMap<String, PendingAuthorizationCode>,
    #[serde(default)]
    access_tokens: HashMap<String, IssuedAccessToken>,
    #[serde(default)]
    refresh_tokens: HashMap<String, IssuedRefreshToken>,
    #[serde(default)]
    revoked_client_ids: HashSet<String>,
    #[serde(default)]
    owner_credentials: HashMap<String, OwnerCredential>,
    #[serde(default)]
    owner_bootstraps: HashMap<String, OwnerBootstrap>,
    #[serde(default)]
    owner_registration_ceremonies: HashMap<String, OwnerRegistrationCeremony>,
    #[serde(default)]
    owner_authentication_ceremonies: HashMap<String, OwnerAuthenticationCeremony>,
}

impl Default for DownstreamOauthState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            clients: HashMap::new(),
            pending_consents: HashMap::new(),
            completed_consents: HashMap::new(),
            pending_codes: HashMap::new(),
            access_tokens: HashMap::new(),
            refresh_tokens: HashMap::new(),
            revoked_client_ids: HashSet::new(),
            owner_credentials: HashMap::new(),
            owner_bootstraps: HashMap::new(),
            owner_registration_ceremonies: HashMap::new(),
            owner_authentication_ceremonies: HashMap::new(),
        }
    }
}

impl DownstreamOauthState {
    fn owner_ceremony_ids_for_consent(&self, consent_id: &str) -> Vec<String> {
        let mut ids = self
            .owner_authentication_ceremonies
            .values()
            .filter(|ceremony| ceremony.consent_id == consent_id)
            .map(|ceremony| ceremony.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    fn evict_expired_owner_records(&mut self, now: u64) {
        self.owner_bootstraps
            .retain(|_, item| item.expires_at > now);
        self.owner_registration_ceremonies
            .retain(|_, item| item.expires_at > now);
        self.owner_authentication_ceremonies
            .retain(|_, item| item.expires_at > now);
    }

    fn evict_expired(&mut self, now: u64) -> HashSet<String> {
        let expired: HashSet<String> = self
            .clients
            .iter()
            .filter(|(_, client)| client.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        self.clients.retain(|id, _| !expired.contains(id));
        self.pending_consents
            .retain(|_, item| item.expires_at > now);
        self.completed_consents
            .retain(|_, item| item.expires_at > now);
        self.pending_codes.retain(|_, item| item.expires_at > now);
        self.access_tokens
            .retain(|_, item| item.expires_at > now && !expired.contains(&item.client_id));
        self.refresh_tokens
            .retain(|_, item| item.expires_at > now && !expired.contains(&item.client_id));
        self.owner_bootstraps
            .retain(|_, item| item.expires_at > now);
        self.owner_registration_ceremonies
            .retain(|_, item| item.expires_at > now);
        self.owner_authentication_ceremonies
            .retain(|_, item| item.expires_at > now);
        expired
    }

    fn remove_client_material(&mut self, client_id: &str) {
        self.clients.remove(client_id);
        self.pending_consents
            .retain(|_, item| item.client_id != client_id);
        self.completed_consents
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
        let state_dir = crate::config::config_dir().join("downstream_oauth");
        Self::try_new_with_state_dir(config, &state_dir)
    }

    fn try_new_with_state_dir(
        config: DownstreamOauthConfig,
        state_dir: &std::path::Path,
    ) -> Result<Self, DownstreamOauthError> {
        let path = state_file_path_in_dir(&config, state_dir, STATE_VERSION);
        let legacy_path = state_file_path_in_dir(&config, state_dir, 2);
        let state_lock = acquire_state_lock(&path)?;
        reconcile_legacy_state(&config, state_dir, &path, &legacy_path)?;
        Self::new_with_state_path_and_lock(config, path, state_lock)
    }

    #[doc(hidden)]
    pub fn new_with_state_path(
        config: DownstreamOauthConfig,
        state_path: PathBuf,
    ) -> Result<Self, DownstreamOauthError> {
        let state_lock = acquire_state_lock(&state_path)?;
        Self::new_with_state_path_and_lock(config, state_path, state_lock)
    }

    fn new_with_state_path_and_lock(
        config: DownstreamOauthConfig,
        state_path: PathBuf,
        state_lock: std::fs::File,
    ) -> Result<Self, DownstreamOauthError> {
        let state_dir = state_path
            .parent()
            .ok_or_else(|| DownstreamOauthError::Persistence("invalid state path".to_string()))?;
        sync_parent_dir_with_retry(state_dir).map_err(|error| {
            DownstreamOauthError::Persistence(format!(
                "downstream OAuth startup directory sync failed for {}: {error}",
                state_dir.display()
            ))
        })?;
        let owner_security = Arc::new(OwnerSecurity::new(&config.public_base_url)?);
        let state = load_persisted_state(&state_path)?;
        let principal_lifecycles = state
            .clients
            .keys()
            .map(|client_id| {
                (
                    client_id.clone(),
                    Arc::new(PrincipalLifecycleState::active()),
                )
            })
            .collect();
        Ok(Self {
            config,
            owner_security,
            state: Arc::new(Mutex::new(state)),
            principal_lifecycles: Arc::new(principal_lifecycles),
            registration_rate: Arc::new(Mutex::new(HashMap::new())),
            state_path: Arc::new(state_path),
            durability_degraded: Arc::new(AtomicBool::new(false)),
            _state_lock: Arc::new(state_lock),
        })
    }

    pub fn durability_degraded(&self) -> bool {
        self.durability_degraded.load(Ordering::SeqCst)
    }

    fn ensure_durable(&self) -> Result<(), DownstreamOauthError> {
        if self.durability_degraded() {
            Err(DownstreamOauthError::Persistence(
                "downstream OAuth durability is uncertain; restart and reconcile persisted state"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    }

    fn commit_state(
        &self,
        live: &mut DownstreamOauthState,
        next: DownstreamOauthState,
    ) -> Result<(), DownstreamOauthError> {
        self.ensure_durable()?;
        let outcome = persist_state(&self.state_path, &next)?;
        *live = next;
        match outcome {
            PersistOutcome::Durable => Ok(()),
            PersistOutcome::CommittedDurabilityUncertain(error) => {
                self.durability_degraded.store(true, Ordering::SeqCst);
                tracing::error!(
                    state_path = %self.state_path.display(),
                    %error,
                    "downstream OAuth entered fail-closed durability-degraded state"
                );
                Err(DownstreamOauthError::Persistence(format!(
                    "state rename committed but parent directory durability remains uncertain after {PARENT_DIR_SYNC_ATTEMPTS} attempts: {error}"
                )))
            }
        }
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

    #[cfg(test)]
    async fn persisted_state_version_for_tests(&self) -> u8 {
        self.state.lock().await.version
    }

    #[cfg(test)]
    async fn pending_consent_exists_for_tests(&self, consent_id: &str) -> bool {
        self.state
            .lock()
            .await
            .pending_consents
            .contains_key(consent_id)
    }

    pub async fn create_owner_bootstrap(&self) -> Result<String, DownstreamOauthError> {
        self.ensure_durable()?;
        let secret = opaque_value();
        let secret_hash = owner::owner_binding_hash(&[b"owner-bootstrap", secret.as_bytes()]);
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        next.evict_expired_owner_records(epoch_secs());
        next.owner_bootstraps.clear();
        next.owner_bootstraps.insert(
            secret_hash.clone(),
            OwnerBootstrap {
                secret_hash,
                expires_at: epoch_secs() + owner::OWNER_CEREMONY_LIFETIME_SECS,
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(secret)
    }

    pub async fn start_owner_registration(
        &self,
        bootstrap: &str,
    ) -> Result<OwnerRegistrationChallenge, DownstreamOauthError> {
        self.ensure_durable()?;
        let now = epoch_secs();
        let bootstrap_hash = owner::owner_binding_hash(&[b"owner-bootstrap", bootstrap.as_bytes()]);
        let issuer_hash = owner::owner_binding_hash(&[self.base_url().as_bytes()]);
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        next.evict_expired_owner_records(now);
        let valid_bootstrap = next.owner_bootstraps.values().any(|record| {
            record.expires_at > now
                && crate::auth::verify_auth_token(&bootstrap_hash, &record.secret_hash)
        });
        if !valid_bootstrap {
            return Err(DownstreamOauthError::InvalidOwnerBootstrap);
        }
        if next.owner_credentials.len() >= owner::MAX_OWNER_CREDENTIALS {
            return Err(DownstreamOauthError::OwnerCredentialLimit);
        }
        if next
            .owner_registration_ceremonies
            .len()
            .saturating_add(next.owner_authentication_ceremonies.len())
            >= owner::MAX_OWNER_CHALLENGES
        {
            return Err(DownstreamOauthError::RateLimited);
        }
        let excluded = next
            .owner_credentials
            .values()
            .map(|credential| credential.passkey.id.clone())
            .collect::<Vec<_>>();
        let user_id = owner::owner_user_id(self.base_url());
        let (public_key, registration_state) = self.owner_security.webauthn.start_registration(
            &user_id,
            "plug-owner",
            "Plug owner",
            &excluded,
        );
        let ceremony_id = opaque_value();
        next.owner_bootstraps.clear();
        next.owner_registration_ceremonies.insert(
            ceremony_id.clone(),
            OwnerRegistrationCeremony {
                id: ceremony_id.clone(),
                bootstrap_hash,
                issuer_hash,
                state: registration_state,
                expires_at: now + owner::OWNER_CEREMONY_LIFETIME_SECS,
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(OwnerRegistrationChallenge {
            ceremony_id,
            public_key,
        })
    }

    pub async fn finish_owner_registration(
        &self,
        ceremony_id: &str,
        response: RegisterPublicKeyCredential,
    ) -> Result<OwnerCredentialSummary, DownstreamOauthError> {
        self.ensure_durable()?;
        let now = epoch_secs();
        let issuer_hash = owner::owner_binding_hash(&[self.base_url().as_bytes()]);
        let mut guard = self.state.lock().await;
        let ceremony = guard
            .owner_registration_ceremonies
            .get(ceremony_id)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidOwnerAssertion)?;
        let mut next = guard.clone();
        next.owner_registration_ceremonies.remove(ceremony_id);
        if ceremony.expires_at <= now {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::OwnerChallengeExpired);
        }
        if !crate::auth::verify_auth_token(&issuer_hash, &ceremony.issuer_hash) {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidOwnerAssertion);
        }
        let passkey = match self
            .owner_security
            .webauthn
            .finish_registration(&ceremony.state, &response)
        {
            Ok(passkey) => passkey,
            Err(_) => {
                self.commit_state(&mut guard, next)?;
                return Err(DownstreamOauthError::InvalidOwnerAssertion);
            }
        };
        let credential_id = passkey.id.to_b64url();
        if next.owner_credentials.contains_key(&credential_id) {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidOwnerAssertion);
        }
        if next.owner_credentials.len() >= owner::MAX_OWNER_CREDENTIALS {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::OwnerCredentialLimit);
        }
        let credential = OwnerCredential {
            id: credential_id.clone(),
            label: format!("Passkey {}", next.owner_credentials.len() + 1),
            passkey,
            created_at: now,
            last_used_at: None,
        };
        let summary = OwnerCredentialSummary::from(&credential);
        next.owner_credentials.insert(credential_id, credential);
        self.commit_state(&mut guard, next)?;
        Ok(summary)
    }

    pub async fn start_owner_approval(
        &self,
        consent_id: &str,
    ) -> Result<OwnerApprovalChallenge, DownstreamOauthError> {
        self.ensure_durable()?;
        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        next.evict_expired_owner_records(now);
        if next.owner_credentials.is_empty() {
            return Err(DownstreamOauthError::OwnerNotEnrolled);
        }
        let consent = next
            .pending_consents
            .get(consent_id)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidAuthorizationRequest)?;
        if consent.expires_at <= now {
            next.pending_consents.remove(consent_id);
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if next
            .owner_registration_ceremonies
            .len()
            .saturating_add(next.owner_authentication_ceremonies.len())
            >= owner::MAX_OWNER_CHALLENGES
        {
            return Err(DownstreamOauthError::RateLimited);
        }
        let credentials = next
            .owner_credentials
            .values()
            .map(|credential| credential.passkey.clone())
            .collect::<Vec<_>>();
        let credential_ids = next.owner_credentials.keys().cloned().collect::<Vec<_>>();
        let user_id = owner::owner_user_id(self.base_url());
        let (public_key, authentication_state) = self
            .owner_security
            .webauthn
            .start_authentication_with_creds_for_user(&user_id, &credentials);
        let consent_bytes = serde_json::to_vec(&consent)
            .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
        let issuer_hash = owner::owner_binding_hash(&[self.base_url().as_bytes()]);
        let consent_binding = owner::owner_binding_hash(&[
            self.base_url().as_bytes(),
            b"approve",
            consent_id.as_bytes(),
            &consent_bytes,
        ]);
        let ceremony_id = opaque_value();
        next.owner_authentication_ceremonies.insert(
            ceremony_id.clone(),
            OwnerAuthenticationCeremony {
                id: ceremony_id.clone(),
                consent_id: consent_id.to_string(),
                issuer_hash,
                consent_binding,
                decision: "approve".to_string(),
                credential_ids,
                state: authentication_state,
                expires_at: now + owner::OWNER_CEREMONY_LIFETIME_SECS,
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(OwnerApprovalChallenge {
            ceremony_id,
            public_key,
        })
    }

    pub async fn finish_owner_approval(
        &self,
        ceremony_id: &str,
        response: PublicKeyCredential,
    ) -> Result<AuthorizationRedirect, DownstreamOauthError> {
        self.ensure_durable()?;
        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        let Some(ceremony) = guard
            .owner_authentication_ceremonies
            .get(ceremony_id)
            .cloned()
        else {
            return guard
                .completed_consents
                .values()
                .find(|completed| {
                    completed.expires_at > now
                        && completed
                            .approval_ceremony_ids
                            .iter()
                            .any(|id| id == ceremony_id)
                })
                .map(|completed| completed.redirect.clone())
                .ok_or(DownstreamOauthError::InvalidOwnerAssertion);
        };
        let mut next = guard.clone();
        let approval_ceremony_ids = next.owner_ceremony_ids_for_consent(&ceremony.consent_id);
        next.completed_consents
            .retain(|_, completed| completed.expires_at > now);
        next.owner_authentication_ceremonies.remove(ceremony_id);
        if ceremony.expires_at <= now {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::OwnerChallengeExpired);
        }
        let Some(consent) = next.pending_consents.get(&ceremony.consent_id).cloned() else {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidOwnerAssertion);
        };
        if consent.expires_at <= now {
            next.pending_consents.remove(&ceremony.consent_id);
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        let consent_bytes = serde_json::to_vec(&consent)
            .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
        let expected_issuer_hash = owner::owner_binding_hash(&[self.base_url().as_bytes()]);
        let expected_consent_binding = owner::owner_binding_hash(&[
            self.base_url().as_bytes(),
            b"approve",
            ceremony.consent_id.as_bytes(),
            &consent_bytes,
        ]);
        if ceremony.decision != "approve"
            || !crate::auth::verify_auth_token(&expected_issuer_hash, &ceremony.issuer_hash)
            || !crate::auth::verify_auth_token(&expected_consent_binding, &ceremony.consent_binding)
        {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidOwnerAssertion);
        }
        let asserted_id = match passkey_auth::CredentialId::from_b64url(&response.id) {
            Ok(id) => id,
            Err(_) => {
                self.commit_state(&mut guard, next)?;
                return Err(DownstreamOauthError::InvalidOwnerAssertion);
            }
        };
        let asserted_id_string = asserted_id.to_b64url();
        if !ceremony
            .credential_ids
            .iter()
            .any(|id| id == &asserted_id_string)
        {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidOwnerAssertion);
        }
        let Some(stored) = next.owner_credentials.get(&asserted_id_string).cloned() else {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::OwnerCredentialNotFound);
        };
        let success = match self.owner_security.webauthn.finish_authentication(
            &ceremony.state,
            &response,
            &stored.passkey,
        ) {
            Ok(success) if success.user_verified => success,
            Ok(_) | Err(_) => {
                self.commit_state(&mut guard, next)?;
                return Err(DownstreamOauthError::InvalidOwnerAssertion);
            }
        };
        let Some(owner_credential) = next.owner_credentials.get_mut(&asserted_id_string) else {
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::OwnerCredentialNotFound);
        };
        owner_credential.passkey.counter = success.new_counter;
        owner_credential.last_used_at = Some(now);
        next.pending_consents.remove(&ceremony.consent_id);
        next.owner_authentication_ceremonies
            .retain(|_, pending| pending.consent_id != ceremony.consent_id);
        let code = opaque_value();
        next.pending_codes.insert(
            code.clone(),
            PendingAuthorizationCode {
                client_id: consent.client_id.clone(),
                redirect_uri: consent.redirect_uri.clone(),
                code_challenge: consent.code_challenge,
                scopes: consent.scopes,
                resource: consent.resource,
                expires_at: now + AUTH_CODE_LIFETIME_SECS,
            },
        );
        if let Some(client) = next.clients.get_mut(&consent.client_id) {
            client.last_used_at = Some(now);
            client.expires_at = now + REGISTRATION_LIFETIME_SECS;
        }
        let redirect = AuthorizationRedirect {
            location: redirect_with_params(
                &consent.redirect_uri,
                &[("code", &code), ("state", &consent.state)],
            ),
        };
        next.completed_consents.insert(
            ceremony.consent_id,
            CompletedConsent {
                client_id: consent.client_id,
                redirect: redirect.clone(),
                expires_at: now + AUTH_CODE_LIFETIME_SECS,
                approval_ceremony_ids,
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(redirect)
    }

    pub async fn deny_consent(
        &self,
        consent_id: &str,
        csrf_token: &str,
    ) -> Result<AuthorizationRedirect, DownstreamOauthError> {
        self.ensure_durable()?;
        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        if let Some(completed) = guard
            .completed_consents
            .get(consent_id)
            .filter(|completed| completed.expires_at > now)
        {
            return Ok(completed.redirect.clone());
        }
        let consent = guard
            .pending_consents
            .get(consent_id)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidAuthorizationRequest)?;
        if consent.expires_at <= now {
            let mut next = guard.clone();
            next.pending_consents.remove(consent_id);
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if !crate::auth::verify_auth_token(csrf_token, &consent.csrf_token) {
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        let redirect = AuthorizationRedirect {
            location: redirect_with_params(
                &consent.redirect_uri,
                &[("error", "access_denied"), ("state", &consent.state)],
            ),
        };
        let mut next = guard.clone();
        let approval_ceremony_ids = next.owner_ceremony_ids_for_consent(consent_id);
        next.completed_consents
            .retain(|_, completed| completed.expires_at > now);
        next.pending_consents.remove(consent_id);
        next.owner_authentication_ceremonies
            .retain(|_, pending| pending.consent_id != consent_id);
        next.completed_consents.insert(
            consent_id.to_string(),
            CompletedConsent {
                client_id: consent.client_id,
                redirect: redirect.clone(),
                expires_at: now + AUTH_CODE_LIFETIME_SECS,
                approval_ceremony_ids,
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(redirect)
    }

    pub async fn list_owner_credentials(&self) -> Vec<OwnerCredentialSummary> {
        let mut credentials = self
            .state
            .lock()
            .await
            .owner_credentials
            .values()
            .map(OwnerCredentialSummary::from)
            .collect::<Vec<_>>();
        credentials.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.credential_id.cmp(&right.credential_id))
        });
        credentials
    }

    pub async fn remove_owner_credential(
        &self,
        credential_id: &str,
    ) -> Result<bool, DownstreamOauthError> {
        self.ensure_durable()?;
        let mut guard = self.state.lock().await;
        if !guard.owner_credentials.contains_key(credential_id) {
            return Ok(false);
        }
        let mut next = guard.clone();
        next.owner_credentials.remove(credential_id);
        next.owner_authentication_ceremonies
            .retain(|_, ceremony| !ceremony.credential_ids.iter().any(|id| id == credential_id));
        self.commit_state(&mut guard, next)?;
        Ok(true)
    }

    pub async fn owner_enrolled(&self) -> bool {
        !self.state.lock().await.owner_credentials.is_empty()
    }

    pub fn local_approval_request_allowed(&self, headers: &axum::http::HeaderMap) -> bool {
        if self.durability_degraded() {
            return false;
        }
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
        self.ensure_durable()?;
        request.redirect_uris.retain(|uri| valid_redirect_uri(uri));
        request.redirect_uris.sort();
        request.redirect_uris.dedup();
        validate_registration_request(&request)?;
        self.check_registration_rate(rate_key).await?;

        let now = epoch_secs();
        let mut guard = self.state.lock().await;
        let mut next = guard.clone();
        let expired = next.evict_expired(now);
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
        self.deactivate_principal_lifecycles(&expired);
        self.commit_state(&mut guard, next)?;
        self.principal_lifecycles.insert(
            client_id.clone(),
            Arc::new(PrincipalLifecycleState::active()),
        );
        self.remove_principal_lifecycles(&expired);

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
        self.ensure_durable()?;
        let mut guard = self.state.lock().await;
        let existed = guard.clients.contains_key(client_id);
        if !existed {
            return Ok(false);
        }
        // Invalidate validation-time leases before removing credentials or
        // starting task cleanup. If persistence fails, this process remains
        // fail-closed; the operator did not receive a successful revocation.
        if let Some(lifecycle) = self.principal_lifecycles.get(client_id) {
            lifecycle.deactivate();
        }
        let mut next = guard.clone();
        next.remove_client_material(client_id);
        next.revoked_client_ids.insert(client_id.to_string());
        self.commit_state(&mut guard, next)?;
        self.principal_lifecycles.remove(client_id);
        Ok(existed)
    }

    pub async fn begin_authorization(
        &self,
        request: AuthorizationRequest<'_>,
    ) -> Result<ConsentRequest, DownstreamOauthError> {
        self.ensure_durable()?;
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
        let mut next = guard.clone();
        let expired = next.evict_expired(now);
        let client = next
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
        if next
            .pending_consents
            .len()
            .saturating_add(next.completed_consents.len())
            >= MAX_PENDING_CONSENTS
            || next
                .pending_consents
                .values()
                .map(|pending| &pending.client_id)
                .chain(
                    next.completed_consents
                        .values()
                        .map(|completed| &completed.client_id),
                )
                .filter(|client_id| client_id.as_str() == request.client_id)
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
        let csrf_token = opaque_value();
        let expires_at = now + AUTH_REQUEST_LIFETIME_SECS;
        next.pending_consents.insert(
            consent_id.clone(),
            PendingConsent {
                client_id: request.client_id.to_string(),
                redirect_uri: request.redirect_uri.to_string(),
                state: request.state.to_string(),
                code_challenge: request.code_challenge.to_string(),
                scopes: scopes.clone(),
                resource: request.resource.to_string(),
                csrf_token: csrf_token.clone(),
                expires_at,
            },
        );
        self.deactivate_principal_lifecycles(&expired);
        self.commit_state(&mut guard, next)?;
        self.remove_principal_lifecycles(&expired);
        Ok(ConsentRequest {
            consent_id,
            client_id: request.client_id.to_string(),
            client_name: client.client_name,
            client_source: client.source,
            redirect_uri: request.redirect_uri.to_string(),
            redirect_host,
            scopes,
            resource: request.resource.to_string(),
            csrf_token,
            expires_at,
        })
    }

    pub async fn decide_consent(
        &self,
        consent_id: &str,
        approved: bool,
    ) -> Result<AuthorizationRedirect, DownstreamOauthError> {
        self.ensure_durable()?;
        let mut guard = self.state.lock().await;
        let now = epoch_secs();
        guard
            .completed_consents
            .retain(|_, completed| completed.expires_at > now);
        if let Some(completed) = guard.completed_consents.get(consent_id) {
            return Ok(completed.redirect.clone());
        }
        let consent = guard
            .pending_consents
            .get(consent_id)
            .cloned()
            .ok_or(DownstreamOauthError::InvalidAuthorizationRequest)?;
        if consent.expires_at <= now {
            let mut next = guard.clone();
            next.pending_consents.remove(consent_id);
            self.commit_state(&mut guard, next)?;
            return Err(DownstreamOauthError::InvalidAuthorizationRequest);
        }
        if !approved {
            let redirect = AuthorizationRedirect {
                location: redirect_with_params(
                    &consent.redirect_uri,
                    &[("error", "access_denied"), ("state", &consent.state)],
                ),
            };
            let mut next = guard.clone();
            next.pending_consents.remove(consent_id);
            next.completed_consents.insert(
                consent_id.to_string(),
                CompletedConsent {
                    client_id: consent.client_id,
                    redirect: redirect.clone(),
                    expires_at: now + AUTH_CODE_LIFETIME_SECS,
                    approval_ceremony_ids: Vec::new(),
                },
            );
            self.commit_state(&mut guard, next)?;
            return Ok(redirect);
        }

        let mut next = guard.clone();
        next.pending_consents.remove(consent_id);
        let code = opaque_value();
        next.pending_codes.insert(
            code.clone(),
            PendingAuthorizationCode {
                client_id: consent.client_id.clone(),
                redirect_uri: consent.redirect_uri.clone(),
                code_challenge: consent.code_challenge,
                scopes: consent.scopes,
                resource: consent.resource,
                expires_at: now + AUTH_CODE_LIFETIME_SECS,
            },
        );
        if let Some(client) = next.clients.get_mut(&consent.client_id) {
            client.last_used_at = Some(now);
            client.expires_at = now + REGISTRATION_LIFETIME_SECS;
        }
        let redirect = AuthorizationRedirect {
            location: redirect_with_params(
                &consent.redirect_uri,
                &[("code", &code), ("state", &consent.state)],
            ),
        };
        next.completed_consents.insert(
            consent_id.to_string(),
            CompletedConsent {
                client_id: consent.client_id,
                redirect: redirect.clone(),
                expires_at: now + AUTH_CODE_LIFETIME_SECS,
                approval_ceremony_ids: Vec::new(),
            },
        );
        self.commit_state(&mut guard, next)?;
        Ok(redirect)
    }

    pub async fn exchange_authorization_code(
        &self,
        client_id: &str,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
        resource: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.ensure_durable()?;
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
        let token = issue_token_pair(&mut next, client_id, &pending.scopes, resource);
        self.commit_state(&mut guard, next)?;
        Ok(token)
    }

    pub async fn exchange_refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        resource: &str,
    ) -> Result<TokenResponsePayload, DownstreamOauthError> {
        self.ensure_durable()?;
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
        let token = issue_token_pair(&mut next, client_id, &refresh.scopes, resource);
        self.commit_state(&mut guard, next)?;
        Ok(token)
    }

    pub async fn validate_access_token_for(
        &self,
        token: &str,
        required_scopes: &[String],
        resource: &str,
    ) -> AccessTokenValidation {
        if self.durability_degraded() {
            return AccessTokenValidation::Invalid;
        }
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
            principal_lifecycle: {
                let Some(lifecycle) = self.principal_lifecycles.get(&record.client_id) else {
                    return AccessTokenValidation::Invalid;
                };
                let generation = lifecycle.generation.load(Ordering::SeqCst);
                let lease = PrincipalLifecycleLease {
                    state: Arc::clone(lifecycle.value()),
                    generation,
                };
                if !lease.is_active() {
                    return AccessTokenValidation::Invalid;
                }
                lease
            },
        })
    }

    pub async fn client_redirect_allowed(&self, client_id: &str, redirect_uri: &str) -> bool {
        if self.durability_degraded() {
            return false;
        }
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
        let expired = next.evict_expired(now);
        if !next.clients.contains_key(client_id) && next.clients.len() >= MAX_REGISTRATIONS {
            return Err(DownstreamOauthError::RegistrationQuotaExceeded);
        }
        next.clients.insert(client_id.to_string(), client);
        self.deactivate_principal_lifecycles(&expired);
        self.commit_state(&mut guard, next)?;
        self.remove_principal_lifecycles(&expired);
        self.principal_lifecycles
            .entry(client_id.to_string())
            .or_insert_with(|| Arc::new(PrincipalLifecycleState::active()));
        Ok(())
    }

    fn deactivate_principal_lifecycles(&self, client_ids: &HashSet<String>) {
        for client_id in client_ids {
            if let Some(lifecycle) = self.principal_lifecycles.get(client_id) {
                lifecycle.deactivate();
            }
        }
    }

    fn remove_principal_lifecycles(&self, client_ids: &HashSet<String>) {
        for client_id in client_ids {
            self.principal_lifecycles.remove(client_id);
        }
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
    // A metadata document describes the client's capabilities across every
    // authorization server it can use. Require the flow Plug will select, but
    // do not reject unrelated extension capabilities. Dynamic registration is
    // authorization-server-specific and intentionally remains stricter.
    if document.client_id != expected_client_id
        || document.redirect_uris.is_empty()
        || document.redirect_uris.len() > 10
        || document
            .redirect_uris
            .iter()
            .any(|uri| !valid_redirect_uri(uri))
        || document
            .token_endpoint_auth_method
            .as_deref()
            .unwrap_or("none")
            != "none"
        || document
            .grant_types
            .as_ref()
            .is_some_and(|items| !items.iter().any(|item| item == "authorization_code"))
        || document
            .response_types
            .as_ref()
            .is_some_and(|items| !items.iter().any(|item| item == "code"))
    {
        return Err(DownstreamOauthError::InvalidClientMetadata);
    }
    Ok(())
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
    state: &mut DownstreamOauthState,
    client_id: &str,
    scopes: &[String],
    resource: &str,
) -> TokenResponsePayload {
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
    TokenResponsePayload {
        access_token,
        refresh_token: Some(refresh_token),
        expires_in: ACCESS_TOKEN_LIFETIME_SECS,
        scope: (!scopes.is_empty()).then(|| scopes.join(" ")),
    }
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

fn state_file_path_in_dir(
    config: &DownstreamOauthConfig,
    state_dir: &std::path::Path,
    version: u8,
) -> PathBuf {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(config.public_base_url.trim_end_matches('/').as_bytes());
    state_dir.join(format!(
        "issuer-v{version}-{}.json",
        hex::encode(&digest[..8])
    ))
}

fn lineage_file_path_in_dir(
    config: &DownstreamOauthConfig,
    state_dir: &std::path::Path,
) -> PathBuf {
    state_file_path_in_dir(config, state_dir, STATE_VERSION).with_extension("lineage.json")
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LineagePhase {
    Pending,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyV2Lineage {
    version: u8,
    phase: LineagePhase,
    full_digest: String,
    global_digest: String,
    client_material_digests: HashMap<String, String>,
    revoked_client_ids: Vec<String>,
}

struct LegacyV2Snapshot {
    lineage: LegacyV2Lineage,
}

fn reconcile_legacy_state(
    config: &DownstreamOauthConfig,
    state_dir: &std::path::Path,
    current_path: &std::path::Path,
    legacy_path: &std::path::Path,
) -> Result<(), DownstreamOauthError> {
    let lineage_path = lineage_file_path_in_dir(config, state_dir);
    let lineage = load_lineage(&lineage_path)?;
    match lineage {
        None if !legacy_path.exists() => Ok(()),
        None if current_path.exists() => Err(DownstreamOauthError::Persistence(format!(
            "version 2 and version {STATE_VERSION} state exist without durable lineage; reconcile {} and {} before starting",
            legacy_path.display(),
            current_path.display()
        ))),
        None => {
            let snapshot = legacy_v2_snapshot(legacy_path)?;
            let mut pending = snapshot.lineage.clone();
            pending.phase = LineagePhase::Pending;
            require_durable(persist_lineage(&lineage_path, &pending)?, &lineage_path)?;
            let migrated = load_persisted_state(legacy_path)?;
            require_durable(persist_state(current_path, &migrated)?, current_path)?;
            let mut complete = snapshot.lineage;
            complete.phase = LineagePhase::Complete;
            require_durable(persist_lineage(&lineage_path, &complete)?, &lineage_path)
        }
        Some(lineage) if lineage.phase == LineagePhase::Pending => {
            if !legacy_path.exists() {
                return Err(DownstreamOauthError::Persistence(format!(
                    "pending version 2 migration is missing its source file: {}",
                    legacy_path.display()
                )));
            }
            let snapshot = legacy_v2_snapshot(legacy_path)?;
            if snapshot.lineage.full_digest != lineage.full_digest {
                return Err(DownstreamOauthError::Persistence(
                    "version 2 state changed during pending migration; explicit reconciliation is required"
                        .to_string(),
                ));
            }
            if !current_path.exists() {
                let migrated = load_persisted_state(legacy_path)?;
                require_durable(persist_state(current_path, &migrated)?, current_path)?;
            }
            let mut complete = lineage;
            complete.phase = LineagePhase::Complete;
            require_durable(persist_lineage(&lineage_path, &complete)?, &lineage_path)
        }
        Some(mut lineage) => {
            if !current_path.exists() {
                return Err(DownstreamOauthError::Persistence(format!(
                    "version {STATE_VERSION} state is missing while completed migration lineage exists; restore or reconcile {} instead of reimporting stale {}",
                    current_path.display(),
                    legacy_path.display()
                )));
            }
            if !legacy_path.exists() {
                return Ok(());
            }
            let snapshot = legacy_v2_snapshot(legacy_path)?;
            if snapshot.lineage.full_digest == lineage.full_digest {
                return Ok(());
            }
            let baseline_revoked = lineage
                .revoked_client_ids
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let current_revoked = snapshot
                .lineage
                .revoked_client_ids
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            let added_revocations = current_revoked
                .difference(&baseline_revoked)
                .cloned()
                .collect::<HashSet<_>>();
            let mut baseline_material = lineage.client_material_digests.clone();
            let mut current_material = snapshot.lineage.client_material_digests.clone();
            for client_id in &added_revocations {
                baseline_material.remove(client_id);
                current_material.remove(client_id);
            }
            if snapshot.lineage.global_digest != lineage.global_digest
                || current_material != baseline_material
                || !current_revoked.is_superset(&baseline_revoked)
            {
                return Err(DownstreamOauthError::Persistence(
                    "ambiguous version 2 changes include more than added revocations; explicit non-destructive reconciliation is required"
                        .to_string(),
                ));
            }

            let mut current = load_persisted_state(current_path)?;
            for client_id in &added_revocations {
                current.remove_client_material(client_id);
                current.revoked_client_ids.insert(client_id.clone());
            }
            require_durable(persist_state(current_path, &current)?, current_path)?;
            lineage.full_digest = snapshot.lineage.full_digest;
            lineage.global_digest = snapshot.lineage.global_digest;
            lineage.client_material_digests = snapshot.lineage.client_material_digests;
            lineage.revoked_client_ids = snapshot.lineage.revoked_client_ids;
            require_durable(persist_lineage(&lineage_path, &lineage)?, &lineage_path)
        }
    }
}

fn load_lineage(path: &std::path::Path) -> Result<Option<LegacyV2Lineage>, DownstreamOauthError> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DownstreamOauthError::Persistence(error.to_string())),
    };
    let lineage: LegacyV2Lineage = serde_json::from_slice(&data)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    if lineage.version != 2 {
        return Err(DownstreamOauthError::Persistence(
            "unsupported downstream OAuth migration lineage version".to_string(),
        ));
    }
    Ok(Some(lineage))
}

fn persist_lineage(
    path: &std::path::Path,
    lineage: &LegacyV2Lineage,
) -> Result<PersistOutcome, DownstreamOauthError> {
    let json = serde_json::to_vec_pretty(lineage)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    persist_bytes(path, &json)
}

fn legacy_v2_snapshot(path: &std::path::Path) -> Result<LegacyV2Snapshot, DownstreamOauthError> {
    let data = std::fs::read(path)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&data)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(2) {
        return Err(DownstreamOauthError::Persistence(
            "legacy downstream OAuth state must have version 2".to_string(),
        ));
    }
    let mut revoked_client_ids = value
        .get("revoked_client_ids")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    revoked_client_ids.sort();
    revoked_client_ids.dedup();
    let full_digest = canonical_json_digest(&value)?;
    let (global_digest, client_material_digests) = legacy_v2_material_digests(&value)?;
    Ok(LegacyV2Snapshot {
        lineage: LegacyV2Lineage {
            version: 2,
            phase: LineagePhase::Complete,
            full_digest,
            global_digest,
            client_material_digests,
            revoked_client_ids,
        },
    })
}

fn legacy_v2_material_digests(
    value: &serde_json::Value,
) -> Result<(String, HashMap<String, String>), DownstreamOauthError> {
    const MATERIAL_FIELDS: [&str; 6] = [
        "clients",
        "pending_consents",
        "completed_consents",
        "pending_codes",
        "access_tokens",
        "refresh_tokens",
    ];

    let mut client_ids = HashSet::new();
    for field in MATERIAL_FIELDS {
        let Some(records) = value.get(field).and_then(serde_json::Value::as_object) else {
            continue;
        };
        if field == "clients" {
            client_ids.extend(records.keys().cloned());
        } else {
            client_ids.extend(records.values().filter_map(|record| {
                record
                    .get("client_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            }));
        }
    }

    let mut global = value.clone();
    global["revoked_client_ids"] = serde_json::json!([]);
    for field in MATERIAL_FIELDS {
        if global.get(field).is_some() {
            global[field] = serde_json::json!({});
        }
    }

    let mut client_material_digests = HashMap::new();
    for client_id in client_ids {
        let mut material = serde_json::Map::new();
        for field in MATERIAL_FIELDS {
            let Some(records) = value.get(field).and_then(serde_json::Value::as_object) else {
                continue;
            };
            let records = records
                .iter()
                .filter(|(record_id, record)| {
                    if field == "clients" {
                        record_id.as_str() == client_id
                    } else {
                        record.get("client_id").and_then(serde_json::Value::as_str)
                            == Some(client_id.as_str())
                    }
                })
                .map(|(key, record)| (key.clone(), record.clone()))
                .collect();
            material.insert(field.to_string(), serde_json::Value::Object(records));
        }
        client_material_digests.insert(
            client_id,
            canonical_json_digest(&serde_json::Value::Object(material))?,
        );
    }

    Ok((canonical_json_digest(&global)?, client_material_digests))
}

fn canonical_json_digest(value: &serde_json::Value) -> Result<String, DownstreamOauthError> {
    use sha2::Digest as _;
    let canonical = canonical_json_value(value);
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    Ok(hex::encode(sha2::Sha256::digest(bytes)))
}

fn canonical_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json_value(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json_value).collect())
        }
        _ => value.clone(),
    }
}

fn require_durable(
    outcome: PersistOutcome,
    path: &std::path::Path,
) -> Result<(), DownstreamOauthError> {
    match outcome {
        PersistOutcome::Durable => Ok(()),
        PersistOutcome::CommittedDurabilityUncertain(error) => {
            Err(DownstreamOauthError::Persistence(format!(
                "{} was renamed but directory durability remains uncertain after {PARENT_DIR_SYNC_ATTEMPTS} attempts: {error}",
                path.display()
            )))
        }
    }
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
    if state.version == 2 {
        state.version = STATE_VERSION;
    } else if state.version != STATE_VERSION {
        return Err(DownstreamOauthError::Persistence(
            "unsupported downstream OAuth state version".to_string(),
        ));
    }
    let _ = state.evict_expired(epoch_secs());
    Ok(state)
}

#[derive(Debug)]
enum PersistOutcome {
    Durable,
    CommittedDurabilityUncertain(std::io::Error),
}

fn persist_state(
    path: &std::path::Path,
    state: &DownstreamOauthState,
) -> Result<PersistOutcome, DownstreamOauthError> {
    let json = serde_json::to_vec_pretty(state)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    persist_bytes(path, &json)
}

fn persist_bytes(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<PersistOutcome, DownstreamOauthError> {
    let dir = path
        .parent()
        .ok_or_else(|| DownstreamOauthError::Persistence("invalid state path".to_string()))?;
    crate::fs_perm::ensure_dir_0700(dir)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    let tmp = temporary_state_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    {
        use std::io::Write as _;
        let mut file = options
            .open(&tmp)
            .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
        let write_result = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(DownstreamOauthError::Persistence(error.to_string()));
        }
    }
    #[cfg(unix)]
    if let Err(error) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownstreamOauthError::Persistence(error.to_string()));
    }
    #[cfg(test)]
    if FAIL_NEXT_STATE_RENAME.replace(false) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownstreamOauthError::Persistence(
            "injected state rename failure".to_string(),
        ));
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownstreamOauthError::Persistence(error.to_string()));
    }
    Ok(match sync_parent_dir_with_retry(dir) {
        Ok(()) => PersistOutcome::Durable,
        Err(error) => PersistOutcome::CommittedDurabilityUncertain(error),
    })
}

#[cfg(test)]
std::thread_local! {
    static FAIL_PARENT_DIR_SYNC_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FAIL_NEXT_STATE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_parent_dir_sync_attempts_for_tests(attempts: usize) {
    FAIL_PARENT_DIR_SYNC_ATTEMPTS.set(attempts);
}

#[cfg(test)]
fn fail_next_state_rename_for_tests() {
    FAIL_NEXT_STATE_RENAME.set(true);
}

fn temporary_state_path(path: &std::path::Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()))
}

fn acquire_state_lock(path: &std::path::Path) -> Result<std::fs::File, DownstreamOauthError> {
    let dir = path
        .parent()
        .ok_or_else(|| DownstreamOauthError::Persistence("invalid state path".to_string()))?;
    crate::fs_perm::ensure_dir_0700(dir)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    let lock_path = path.with_extension("lock");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(&lock_path)
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| DownstreamOauthError::Persistence(error.to_string()))?;
    fs4::FileExt::try_lock(&file).map_err(|error| {
        DownstreamOauthError::Persistence(format!(
            "downstream OAuth issuer state writer is already active for {}: {error}",
            path.display()
        ))
    })?;
    Ok(file)
}

fn sync_parent_dir_with_retry(dir: &std::path::Path) -> std::io::Result<()> {
    let mut last_error = None;
    for attempt in 0..PARENT_DIR_SYNC_ATTEMPTS {
        match sync_parent_dir(dir) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < PARENT_DIR_SYNC_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("directory sync was not attempted")))
}

fn sync_parent_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let remaining = FAIL_PARENT_DIR_SYNC_ATTEMPTS.get();
        if remaining > 0 {
            FAIL_PARENT_DIR_SYNC_ATTEMPTS.set(remaining - 1);
            return Err(std::io::Error::other(
                "injected parent directory sync failure",
            ));
        }
    }

    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downstream_oauth::owner::tests::BrowserAuthenticator;

    fn temp_state_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "plug-downstream-oauth-{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    fn write_state_fixture(fixture: serde_json::Value) -> PathBuf {
        let path = temp_state_path();
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&fixture).expect("serialize fixture"),
        )
        .expect("write state fixture");
        path
    }

    fn test_config() -> DownstreamOauthConfig {
        DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
            local_port: 3282,
        }
    }

    fn test_manager() -> (DownstreamOauthManager, PathBuf) {
        let path = temp_state_path();
        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("test manager");
        (manager, path)
    }

    async fn enroll_owner(
        manager: &DownstreamOauthManager,
        authenticator: &BrowserAuthenticator,
    ) -> OwnerCredentialSummary {
        let bootstrap = manager
            .create_owner_bootstrap()
            .await
            .expect("owner bootstrap");
        let challenge = manager
            .start_owner_registration(&bootstrap)
            .await
            .expect("start owner registration");
        let response = authenticator.registration_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        manager
            .finish_owner_registration(&challenge.ceremony_id, response)
            .await
            .expect("finish owner registration")
    }

    async fn begin_test_authorization(
        manager: &DownstreamOauthManager,
        state: &str,
    ) -> ConsentRequest {
        let client = register(manager, state, &format!("https://{state}.example/callback")).await;
        manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state,
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin test authorization")
    }

    #[tokio::test]
    async fn owner_bootstrap_is_single_use() {
        let (manager, _) = test_manager();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        manager.start_owner_registration(&bootstrap).await.unwrap();
        assert_eq!(
            manager
                .start_owner_registration(&bootstrap)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerBootstrap
        );
    }

    #[tokio::test]
    async fn approval_challenge_is_bound_to_original_consent() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let first = begin_test_authorization(&manager, "first-state").await;
        let second = begin_test_authorization(&manager, "second-state").await;
        let challenge = manager
            .start_owner_approval(&first.consent_id)
            .await
            .unwrap();
        let assertion = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        let redirect = manager
            .finish_owner_approval(&challenge.ceremony_id, assertion)
            .await
            .unwrap();
        assert!(redirect.location.contains("state=first-state"));
        assert!(
            manager
                .pending_consent_exists_for_tests(&second.consent_id)
                .await
        );
    }

    fn malformed_owner_assertion() -> PublicKeyCredential {
        PublicKeyCredential {
            id: "malformed".to_string(),
            authenticator_data: "malformed".to_string(),
            signature: "malformed".to_string(),
            client_data_json: "malformed".to_string(),
            user_handle: None,
        }
    }

    #[tokio::test]
    async fn all_tied_approval_ceremonies_replay_first_approval() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "approval-race").await;
        let first = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let second = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let first_response = authenticator.authentication_response(
            &first.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        let first_redirect = manager
            .finish_owner_approval(&first.ceremony_id, first_response)
            .await
            .unwrap();
        let second_redirect = manager
            .finish_owner_approval(&second.ceremony_id, malformed_owner_assertion())
            .await
            .unwrap();
        assert_eq!(second_redirect.location, first_redirect.location);
        assert_eq!(manager.state.lock().await.pending_codes.len(), 1);
    }

    #[tokio::test]
    async fn approval_then_denial_replays_first_approval() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "approval-then-denial").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        let approved = manager
            .finish_owner_approval(&challenge.ceremony_id, response)
            .await
            .unwrap();
        let denied_late = manager
            .deny_consent(&consent.consent_id, &consent.csrf_token)
            .await
            .unwrap();
        assert_eq!(denied_late.location, approved.location);
        assert_eq!(manager.state.lock().await.pending_codes.len(), 1);
    }

    #[tokio::test]
    async fn denial_then_tied_approval_replays_first_denial() {
        let (manager, _) = test_manager();
        let authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "denial-race").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let denied = manager
            .deny_consent(&consent.consent_id, &consent.csrf_token)
            .await
            .unwrap();
        let late_approval = manager
            .finish_owner_approval(&challenge.ceremony_id, malformed_owner_assertion())
            .await
            .unwrap();
        assert_eq!(late_approval.location, denied.location);
        assert!(manager.state.lock().await.pending_codes.is_empty());
    }

    #[tokio::test]
    async fn tied_approval_and_denial_replay_links_survive_restart() {
        let approval_path = temp_state_path();
        let approval_manager =
            DownstreamOauthManager::new_with_state_path(test_config(), approval_path.clone())
                .unwrap();
        let mut approval_authenticator = BrowserAuthenticator::new();
        enroll_owner(&approval_manager, &approval_authenticator).await;
        let approval_consent =
            begin_test_authorization(&approval_manager, "approval-restart-race").await;
        let approval_first = approval_manager
            .start_owner_approval(&approval_consent.consent_id)
            .await
            .unwrap();
        let approval_second = approval_manager
            .start_owner_approval(&approval_consent.consent_id)
            .await
            .unwrap();
        let response = approval_authenticator.authentication_response(
            &approval_first.public_key.challenge,
            &approval_manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        let approved = approval_manager
            .finish_owner_approval(&approval_first.ceremony_id, response)
            .await
            .unwrap();
        drop(approval_manager);
        let approval_restarted =
            DownstreamOauthManager::new_with_state_path(test_config(), approval_path).unwrap();
        assert_eq!(
            approval_restarted
                .finish_owner_approval(&approval_second.ceremony_id, malformed_owner_assertion())
                .await
                .unwrap()
                .location,
            approved.location
        );
        assert_eq!(approval_restarted.state.lock().await.pending_codes.len(), 1);

        let denial_path = temp_state_path();
        let denial_manager =
            DownstreamOauthManager::new_with_state_path(test_config(), denial_path.clone())
                .unwrap();
        let denial_authenticator = BrowserAuthenticator::new();
        enroll_owner(&denial_manager, &denial_authenticator).await;
        let denial_consent = begin_test_authorization(&denial_manager, "denial-restart-race").await;
        let denial_challenge = denial_manager
            .start_owner_approval(&denial_consent.consent_id)
            .await
            .unwrap();
        let denied = denial_manager
            .deny_consent(&denial_consent.consent_id, &denial_consent.csrf_token)
            .await
            .unwrap();
        drop(denial_manager);
        let denial_restarted =
            DownstreamOauthManager::new_with_state_path(test_config(), denial_path).unwrap();
        assert_eq!(
            denial_restarted
                .finish_owner_approval(&denial_challenge.ceremony_id, malformed_owner_assertion())
                .await
                .unwrap()
                .location,
            denied.location
        );
        assert!(denial_restarted.state.lock().await.pending_codes.is_empty());
    }

    #[tokio::test]
    async fn owner_bootstrap_can_be_exchanged_once_after_restart() {
        let path = temp_state_path();
        let manager =
            DownstreamOauthManager::new_with_state_path(test_config(), path.clone()).unwrap();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        drop(manager);
        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path).unwrap();
        restarted
            .start_owner_registration(&bootstrap)
            .await
            .unwrap();
        assert_eq!(
            restarted
                .start_owner_registration(&bootstrap)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerBootstrap
        );
    }

    #[tokio::test]
    async fn registration_write_failure_keeps_ceremony_and_publishes_no_credential() {
        let (manager, _) = test_manager();
        let authenticator = BrowserAuthenticator::new();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        let challenge = manager.start_owner_registration(&bootstrap).await.unwrap();
        let response = authenticator.registration_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        fail_next_state_rename_for_tests();
        assert!(matches!(
            manager
                .finish_owner_registration(&challenge.ceremony_id, response)
                .await,
            Err(DownstreamOauthError::Persistence(_))
        ));
        {
            let state = manager.state.lock().await;
            assert!(state.owner_credentials.is_empty());
            assert!(
                state
                    .owner_registration_ceremonies
                    .contains_key(&challenge.ceremony_id)
            );
        }
        let retry = authenticator.registration_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        manager
            .finish_owner_registration(&challenge.ceremony_id, retry)
            .await
            .unwrap();
        assert!(manager.owner_enrolled().await);
    }

    #[tokio::test]
    async fn approval_post_rename_uncertainty_publishes_committed_state_and_restart_replays() {
        let path = temp_state_path();
        let manager =
            DownstreamOauthManager::new_with_state_path(test_config(), path.clone()).unwrap();
        let mut authenticator = BrowserAuthenticator::new();
        let summary = enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "uncertain-approval").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        fail_next_parent_dir_sync_attempts_for_tests(PARENT_DIR_SYNC_ATTEMPTS);
        assert!(matches!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await,
            Err(DownstreamOauthError::Persistence(_))
        ));
        assert!(manager.durability_degraded());
        let committed_redirect = {
            let state = manager.state.lock().await;
            assert!(!state.pending_consents.contains_key(&consent.consent_id));
            assert_eq!(state.pending_codes.len(), 1);
            assert_eq!(
                state
                    .owner_credentials
                    .get(&summary.credential_id)
                    .unwrap()
                    .passkey
                    .counter,
                1
            );
            state
                .completed_consents
                .get(&consent.consent_id)
                .unwrap()
                .redirect
                .clone()
        };
        drop(manager);
        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path).unwrap();
        assert_eq!(
            restarted
                .finish_owner_approval(&challenge.ceremony_id, malformed_owner_assertion())
                .await
                .unwrap()
                .location,
            committed_redirect.location
        );
        assert_eq!(restarted.state.lock().await.pending_codes.len(), 1);
    }

    #[tokio::test]
    async fn owner_bootstrap_expiry_is_rejected_without_creating_ceremony() {
        let (manager, _) = test_manager();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        {
            let mut state = manager.state.lock().await;
            state
                .owner_bootstraps
                .values_mut()
                .for_each(|record| record.expires_at = 0);
        }
        assert_eq!(
            manager
                .start_owner_registration(&bootstrap)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerBootstrap
        );
        assert!(
            manager
                .state
                .lock()
                .await
                .owner_registration_ceremonies
                .is_empty()
        );
    }

    #[tokio::test]
    async fn owner_registration_requires_uv_and_consumes_failed_challenge() {
        let (manager, _) = test_manager();
        let authenticator = BrowserAuthenticator::new();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        let challenge = manager.start_owner_registration(&bootstrap).await.unwrap();
        let response = authenticator.registration_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            false,
        );
        assert_eq!(
            manager
                .finish_owner_registration(&challenge.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
        let valid_replay = authenticator.registration_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_registration(&challenge.ceremony_id, valid_replay)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
        assert!(!manager.owner_enrolled().await);
    }

    #[tokio::test]
    async fn owner_registration_rejects_wrong_origin_and_survives_restart() {
        let path = temp_state_path();
        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("test manager");
        let authenticator = BrowserAuthenticator::new();
        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        let wrong_origin = manager.start_owner_registration(&bootstrap).await.unwrap();
        let response = authenticator.registration_response(
            &wrong_origin.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://evil.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_registration(&wrong_origin.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );

        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        let wrong_rp = manager.start_owner_registration(&bootstrap).await.unwrap();
        let response = authenticator.registration_response(
            &wrong_rp.public_key.challenge,
            "evil.example.com",
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_registration(&wrong_rp.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );

        let bootstrap = manager.create_owner_bootstrap().await.unwrap();
        let challenge = manager.start_owner_registration(&bootstrap).await.unwrap();
        drop(manager);
        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("restart manager");
        let response = authenticator.registration_response(
            &challenge.public_key.challenge,
            &restarted.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        restarted
            .finish_owner_registration(&challenge.ceremony_id, response)
            .await
            .expect("finish after restart");
        drop(restarted);
        let reloaded = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("reload manager");
        assert!(reloaded.owner_enrolled().await);
        assert_eq!(reloaded.list_owner_credentials().await.len(), 1);
    }

    #[tokio::test]
    async fn owner_approval_requires_uv_consumes_failure_and_preserves_consent() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "uv-state").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            false,
        );
        assert_eq!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
        assert!(
            manager
                .pending_consent_exists_for_tests(&consent.consent_id)
                .await
        );
        let replay = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, replay)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
    }

    #[tokio::test]
    async fn owner_approval_survives_restart_persists_counter_and_is_idempotent() {
        let path = temp_state_path();
        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("test manager");
        let mut authenticator = BrowserAuthenticator::new();
        let summary = enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "restart-state").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("restart manager");
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &restarted.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        let redirect = restarted
            .finish_owner_approval(&challenge.ceremony_id, response)
            .await
            .expect("finish owner approval after restart");
        assert!(redirect.location.contains("state=restart-state"));
        let replay = PublicKeyCredential {
            id: "malformed".to_string(),
            authenticator_data: "malformed".to_string(),
            signature: "malformed".to_string(),
            client_data_json: "malformed".to_string(),
            user_handle: None,
        };
        assert_eq!(
            restarted
                .finish_owner_approval(&challenge.ceremony_id, replay)
                .await
                .unwrap()
                .location,
            redirect.location
        );
        assert_eq!(
            restarted
                .state
                .lock()
                .await
                .owner_credentials
                .get(&summary.credential_id)
                .unwrap()
                .passkey
                .counter,
            1
        );
        drop(restarted);
        let reloaded = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("reload manager");
        assert_eq!(
            reloaded
                .state
                .lock()
                .await
                .owner_credentials
                .get(&summary.credential_id)
                .unwrap()
                .passkey
                .counter,
            1
        );
    }

    #[tokio::test]
    async fn approval_write_failure_publishes_nothing_and_can_retry() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        let summary = enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "atomic-state").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        fail_next_state_rename_for_tests();
        assert!(matches!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await,
            Err(DownstreamOauthError::Persistence(_))
        ));
        {
            let state = manager.state.lock().await;
            assert!(state.pending_consents.contains_key(&consent.consent_id));
            assert!(
                state
                    .owner_authentication_ceremonies
                    .contains_key(&challenge.ceremony_id)
            );
            assert!(state.pending_codes.is_empty());
            assert_eq!(
                state
                    .owner_credentials
                    .get(&summary.credential_id)
                    .unwrap()
                    .passkey
                    .counter,
                0
            );
        }
        let retry = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        manager
            .finish_owner_approval(&challenge.ceremony_id, retry)
            .await
            .expect("retry approval after pre-rename failure");
        assert_eq!(
            manager
                .state
                .lock()
                .await
                .owner_credentials
                .get(&summary.credential_id)
                .unwrap()
                .passkey
                .counter,
            2
        );
    }

    #[tokio::test]
    async fn consent_substitution_and_denial_csrf_fail_closed() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let first = begin_test_authorization(&manager, "bound-first").await;
        let second = begin_test_authorization(&manager, "bound-second").await;
        let challenge = manager
            .start_owner_approval(&first.consent_id)
            .await
            .unwrap();
        {
            let mut state = manager.state.lock().await;
            state
                .owner_authentication_ceremonies
                .get_mut(&challenge.ceremony_id)
                .unwrap()
                .consent_id = second.consent_id.clone();
        }
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
        assert!(
            manager
                .pending_consent_exists_for_tests(&first.consent_id)
                .await
        );
        assert!(
            manager
                .pending_consent_exists_for_tests(&second.consent_id)
                .await
        );

        assert_eq!(
            manager
                .deny_consent(&first.consent_id, "wrong-csrf")
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidAuthorizationRequest
        );
        let denied = manager
            .deny_consent(&first.consent_id, &first.csrf_token)
            .await
            .expect("deny consent");
        assert!(denied.location.contains("error=access_denied"));
        assert!(manager.state.lock().await.pending_codes.is_empty());
        assert_eq!(
            manager
                .deny_consent(&first.consent_id, "conflicting-replay")
                .await
                .unwrap()
                .location,
            denied.location
        );
    }

    #[tokio::test]
    async fn removing_owner_credential_removes_tied_approval_ceremonies() {
        let (manager, _) = test_manager();
        let authenticator = BrowserAuthenticator::new();
        let summary = enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "remove-state").await;
        manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        assert!(
            manager
                .remove_owner_credential(&summary.credential_id)
                .await
                .unwrap()
        );
        assert!(!manager.owner_enrolled().await);
        assert!(manager.list_owner_credentials().await.is_empty());
        assert!(
            manager
                .state
                .lock()
                .await
                .owner_authentication_ceremonies
                .is_empty()
        );
        assert!(
            !manager
                .remove_owner_credential(&summary.credential_id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn expired_approval_challenge_is_consumed_without_approving() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        enroll_owner(&manager, &authenticator).await;
        let consent = begin_test_authorization(&manager, "expired-approval").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        {
            let mut state = manager.state.lock().await;
            state
                .owner_authentication_ceremonies
                .get_mut(&challenge.ceremony_id)
                .unwrap()
                .expires_at = 0;
        }
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::OwnerChallengeExpired
        );
        let state = manager.state.lock().await;
        assert!(state.pending_consents.contains_key(&consent.consent_id));
        assert!(state.pending_codes.is_empty());
        assert!(
            !state
                .owner_authentication_ceremonies
                .contains_key(&challenge.ceremony_id)
        );
    }

    #[tokio::test]
    async fn owner_counter_regression_fails_closed_and_keeps_durable_counter() {
        let (manager, _) = test_manager();
        let mut authenticator = BrowserAuthenticator::new();
        let summary = enroll_owner(&manager, &authenticator).await;
        for state_value in ["counter-one", "counter-two"] {
            let consent = begin_test_authorization(&manager, state_value).await;
            let challenge = manager
                .start_owner_approval(&consent.consent_id)
                .await
                .unwrap();
            let response = authenticator.authentication_response(
                &challenge.public_key.challenge,
                &manager.owner_security.rp_id,
                "https://plug.example.com",
                true,
            );
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await
                .unwrap();
        }
        assert_eq!(
            manager
                .state
                .lock()
                .await
                .owner_credentials
                .get(&summary.credential_id)
                .unwrap()
                .passkey
                .counter,
            2
        );

        let consent = begin_test_authorization(&manager, "counter-regression").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .unwrap();
        authenticator.set_counter(0);
        let response = authenticator.authentication_response(
            &challenge.public_key.challenge,
            &manager.owner_security.rp_id,
            "https://plug.example.com",
            true,
        );
        assert_eq!(
            manager
                .finish_owner_approval(&challenge.ceremony_id, response)
                .await
                .unwrap_err(),
            DownstreamOauthError::InvalidOwnerAssertion
        );
        let state = manager.state.lock().await;
        assert_eq!(
            state
                .owner_credentials
                .get(&summary.credential_id)
                .unwrap()
                .passkey
                .counter,
            2
        );
        assert!(state.pending_consents.contains_key(&consent.consent_id));
        assert!(
            !state
                .owner_authentication_ceremonies
                .contains_key(&challenge.ceremony_id)
        );
    }

    #[tokio::test]
    async fn v2_state_migrates_without_losing_grants() {
        let now = epoch_secs();
        let fixture = serde_json::json!({
            "version": 2,
            "clients": {
                "plug_existing": {
                    "client_id": "plug_existing",
                    "client_name": "Existing client",
                    "redirect_uris": ["https://client.example/callback"],
                    "source": "dynamic_registration",
                    "created_at": now,
                    "last_used_at": now,
                    "expires_at": now + 3600
                }
            },
            "access_tokens": {
                "access-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "issued_at": now,
                    "expires_at": now + 3600
                }
            },
            "refresh_tokens": {
                "refresh-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "expires_at": now + 3600
                }
            },
            "revoked_client_ids": []
        });
        let path = write_state_fixture(fixture);

        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("migrate version 2 state");
        assert_eq!(manager.persisted_state_version_for_tests().await, 3);
        let state = manager.state.lock().await;

        assert!(state.clients.contains_key("plug_existing"));
        assert!(state.access_tokens.contains_key("access-existing"));
        assert!(state.refresh_tokens.contains_key("refresh-existing"));
    }

    #[tokio::test]
    async fn try_new_migrates_into_v3_without_mutating_v2_source() {
        let temp = tempfile::tempdir().expect("state tempdir");
        let state_dir = temp.path().join("downstream_oauth");
        std::fs::create_dir_all(&state_dir).expect("state directory");
        let config = test_config();
        let legacy_path = state_file_path_in_dir(&config, &state_dir, 2);
        let current_path = state_file_path_in_dir(&config, &state_dir, STATE_VERSION);
        let now = epoch_secs();
        let fixture = serde_json::json!({
            "version": 2,
            "clients": {
                "plug_existing": {
                    "client_id": "plug_existing",
                    "client_name": "Existing client",
                    "redirect_uris": ["https://client.example/callback"],
                    "source": "dynamic_registration",
                    "created_at": now,
                    "last_used_at": now,
                    "expires_at": now + 3600
                }
            },
            "access_tokens": {
                "access-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "issued_at": now,
                    "expires_at": now + 3600
                }
            },
            "refresh_tokens": {
                "refresh-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "expires_at": now + 3600
                }
            },
            "revoked_client_ids": ["plug_revoked"]
        });
        let legacy_bytes = serde_json::to_vec_pretty(&fixture).expect("serialize fixture");
        std::fs::write(&legacy_path, &legacy_bytes).expect("write legacy fixture");

        let manager = DownstreamOauthManager::try_new_with_state_dir(config, &state_dir)
            .expect("migrate production state paths");

        assert_eq!(
            std::fs::read(&legacy_path).expect("read legacy source"),
            legacy_bytes,
            "version 2 rollback source must remain byte-for-byte unchanged"
        );
        let current: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&current_path).expect("read version 3 target"))
                .expect("parse version 3 target");
        assert_eq!(current["version"], 3);
        assert!(current["clients"].get("plug_existing").is_some());
        assert!(current["access_tokens"].get("access-existing").is_some());
        assert!(current["refresh_tokens"].get("refresh-existing").is_some());
        assert_eq!(
            current["revoked_client_ids"],
            serde_json::json!(["plug_revoked"])
        );

        let state = manager.state.lock().await;
        assert!(state.clients.contains_key("plug_existing"));
        assert!(state.access_tokens.contains_key("access-existing"));
        assert!(state.refresh_tokens.contains_key("refresh-existing"));
        assert!(state.revoked_client_ids.contains("plug_revoked"));
    }

    #[tokio::test]
    async fn roll_forward_merges_rollback_revocation_by_digest_and_preserves_owner_state() {
        let temp = tempfile::tempdir().expect("state tempdir");
        let state_dir = temp.path().join("downstream_oauth");
        std::fs::create_dir_all(&state_dir).expect("state directory");
        let config = test_config();
        let legacy_path = state_file_path_in_dir(&config, &state_dir, 2);
        let current_path = state_file_path_in_dir(&config, &state_dir, STATE_VERSION);
        let now = epoch_secs();
        let mut rollback_state = serde_json::json!({
            "version": 2,
            "clients": {
                "plug_existing": {
                    "client_id": "plug_existing",
                    "client_name": "Existing client",
                    "redirect_uris": ["https://client.example/callback"],
                    "source": "dynamic_registration",
                    "created_at": now,
                    "last_used_at": now,
                    "expires_at": now + 3600
                }
            },
            "access_tokens": {
                "access-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "issued_at": now,
                    "expires_at": now + 3600
                }
            },
            "refresh_tokens": {
                "refresh-existing": {
                    "client_id": "plug_existing",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "expires_at": now + 3600
                }
            },
            "revoked_client_ids": []
        });
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&rollback_state).expect("serialize initial v2"),
        )
        .expect("write initial v2");

        let initial = DownstreamOauthManager::try_new_with_state_dir(config.clone(), &state_dir)
            .expect("initial roll-forward migration");
        {
            let mut live = initial.state.lock().await;
            let mut next = live.clone();
            next.owner_bootstraps.insert(
                "owner-only-v3".to_string(),
                OwnerBootstrap {
                    secret_hash: "owner-secret-hash".to_string(),
                    expires_at: now + 3600,
                },
            );
            assert!(matches!(
                persist_state(&current_path, &next).expect("persist v3 owner state"),
                PersistOutcome::Durable
            ));
            *live = next;
        }
        drop(initial);
        let v3_modified = std::fs::metadata(&current_path)
            .and_then(|metadata| metadata.modified())
            .expect("version 3 modification time");

        rollback_state["clients"]
            .as_object_mut()
            .expect("version 2 clients")
            .remove("plug_existing");
        rollback_state["access_tokens"]
            .as_object_mut()
            .expect("version 2 access tokens")
            .remove("access-existing");
        rollback_state["refresh_tokens"]
            .as_object_mut()
            .expect("version 2 refresh tokens")
            .remove("refresh-existing");
        rollback_state["revoked_client_ids"] = serde_json::json!(["plug_existing"]);
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&rollback_state).expect("serialize rollback mutation"),
        )
        .expect("write rollback mutation");
        filetime::set_file_mtime(
            &legacy_path,
            filetime::FileTime::from_system_time(
                v3_modified
                    .checked_sub(std::time::Duration::from_secs(60))
                    .expect("clock-skewed timestamp"),
            ),
        )
        .expect("set rollback file time behind version 3");

        let reconciled = DownstreamOauthManager::try_new_with_state_dir(config.clone(), &state_dir)
            .expect("digest lineage merges rollback revocation despite backward clock");
        let state = reconciled.state.lock().await;
        assert!(state.revoked_client_ids.contains("plug_existing"));
        assert!(!state.clients.contains_key("plug_existing"));
        assert!(state.owner_bootstraps.contains_key("owner-only-v3"));
        drop(state);
        drop(reconciled);

        rollback_state["revoked_client_ids"] =
            serde_json::json!(["plug_existing", "plug_equal_clock_revocation"]);
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&rollback_state).expect("serialize equal-clock rollback"),
        )
        .expect("write equal-clock rollback");
        let current_mtime = std::fs::metadata(&current_path)
            .and_then(|metadata| metadata.modified())
            .expect("current v3 modification time");
        filetime::set_file_mtime(
            &legacy_path,
            filetime::FileTime::from_system_time(current_mtime),
        )
        .expect("set equal version 2 and version 3 timestamps");
        let equal_clock =
            DownstreamOauthManager::try_new_with_state_dir(config.clone(), &state_dir)
                .expect("digest lineage merges revocation with equal clocks");
        let equal_clock_state = equal_clock.state.lock().await;
        assert!(
            equal_clock_state
                .revoked_client_ids
                .contains("plug_equal_clock_revocation")
        );
        assert!(
            equal_clock_state
                .owner_bootstraps
                .contains_key("owner-only-v3")
        );
        drop(equal_clock_state);
        drop(equal_clock);

        assert!(lineage_file_path_in_dir(&config, &state_dir).exists());
        std::fs::remove_file(&current_path).expect("simulate lost version 3 file");
        let error = DownstreamOauthManager::try_new_with_state_dir(config, &state_dir)
            .expect_err("completed lineage must block stale v2 reimport when v3 is missing");
        assert!(error.to_string().contains("version 3 state is missing"));
    }

    #[test]
    fn roll_forward_blocks_ambiguous_rollback_grant_changes() {
        let temp = tempfile::tempdir().expect("state tempdir");
        let state_dir = temp.path().join("downstream_oauth");
        std::fs::create_dir_all(&state_dir).expect("state directory");
        let config = test_config();
        let legacy_path = state_file_path_in_dir(&config, &state_dir, 2);
        let now = epoch_secs();
        let mut rollback_state = serde_json::json!({
            "version": 2,
            "clients": {},
            "access_tokens": {},
            "refresh_tokens": {},
            "revoked_client_ids": []
        });
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&rollback_state).expect("serialize initial v2"),
        )
        .expect("write initial v2");
        drop(
            DownstreamOauthManager::try_new_with_state_dir(config.clone(), &state_dir)
                .expect("initial migration"),
        );

        rollback_state["clients"]["plug_rollback_grant"] = serde_json::json!({
            "client_id": "plug_rollback_grant",
            "client_name": "Rollback grant",
            "redirect_uris": ["https://client.example/callback"],
            "source": "dynamic_registration",
            "created_at": now,
            "last_used_at": now,
            "expires_at": now + 3600
        });
        std::fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&rollback_state).expect("serialize ambiguous rollback"),
        )
        .expect("write ambiguous rollback");

        let error = DownstreamOauthManager::try_new_with_state_dir(config, &state_dir)
            .expect_err("rollback grants require explicit operator reconciliation");
        assert!(error.to_string().contains("ambiguous version 2 changes"));
    }

    #[tokio::test]
    async fn pending_authorization_survives_restart() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "restart-state",
                code_challenge: "restart-challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart manager");

        assert!(
            restarted
                .pending_consent_exists_for_tests(&consent.consent_id)
                .await
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authorization_start_sync_failure_fails_closed_with_aligned_recovery() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        fail_next_parent_dir_sync_attempts_for_tests(PARENT_DIR_SYNC_ATTEMPTS);

        let result = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "failure-state",
                code_challenge: "failure-challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await;

        assert!(matches!(result, Err(DownstreamOauthError::Persistence(_))));
        assert!(manager.durability_degraded());
        let consent_id = manager
            .state
            .lock()
            .await
            .pending_consents
            .keys()
            .next()
            .cloned()
            .expect("renamed consent remains published internally");
        let disk = load_persisted_state(&path).expect("read renamed state");
        assert!(disk.pending_consents.contains_key(&consent_id));
        assert!(
            manager
                .state
                .lock()
                .await
                .pending_consents
                .contains_key(&consent_id)
        );
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart from committed state");
        assert!(
            restarted
                .state
                .lock()
                .await
                .pending_consents
                .contains_key(&consent_id)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_sync_failure_returns_committed_rotation_and_restart_matches() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let original = issue_tokens(&manager, &client).await;
        let original_refresh = original.refresh_token.expect("original refresh token");
        fail_next_parent_dir_sync_attempts_for_tests(PARENT_DIR_SYNC_ATTEMPTS);

        let result = manager
            .exchange_refresh_token(
                &client.client_id,
                &original_refresh,
                "https://plug.example.com/mcp",
            )
            .await;
        assert!(matches!(result, Err(DownstreamOauthError::Persistence(_))));
        assert!(manager.durability_degraded());

        let disk = load_persisted_state(&path).expect("read renamed rotation");
        assert!(!disk.refresh_tokens.contains_key(&original_refresh));
        let rotated_refresh = disk
            .refresh_tokens
            .keys()
            .find(|token| token.as_str() != original_refresh)
            .cloned()
            .expect("committed replacement remains on disk");
        assert!(disk.refresh_tokens.contains_key(&rotated_refresh));
        let memory = manager.state.lock().await;
        assert!(!memory.refresh_tokens.contains_key(&original_refresh));
        assert!(memory.refresh_tokens.contains_key(&rotated_refresh));
        drop(memory);
        assert_eq!(
            manager
                .validate_access_token_for(
                    &original.access_token,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp",
                )
                .await,
            AccessTokenValidation::Invalid
        );
        assert!(matches!(
            manager
                .exchange_refresh_token(
                    &client.client_id,
                    &rotated_refresh,
                    "https://plug.example.com/mcp",
                )
                .await,
            Err(DownstreamOauthError::Persistence(_))
        ));
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart from committed rotation");
        let restarted_state = restarted.state.lock().await;
        assert!(
            !restarted_state
                .refresh_tokens
                .contains_key(&original_refresh)
        );
        assert!(
            restarted_state
                .refresh_tokens
                .contains_key(&rotated_refresh)
        );
        drop(restarted_state);
        restarted
            .exchange_refresh_token(
                &client.client_id,
                &rotated_refresh,
                "https://plug.example.com/mcp",
            )
            .await
            .expect("restart can rotate the committed replacement token");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn revoke_sync_failure_degrades_lifecycle_and_restart_stays_revoked() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let tokens = issue_tokens(&manager, &client).await;
        let lease = match manager
            .validate_access_token_for(
                &tokens.access_token,
                &["tools:read".to_string()],
                "https://plug.example.com/mcp",
            )
            .await
        {
            AccessTokenValidation::Valid(claims) => claims.principal_lifecycle,
            other => panic!("expected valid token before revoke, got {other:?}"),
        };
        fail_next_parent_dir_sync_attempts_for_tests(PARENT_DIR_SYNC_ATTEMPTS);

        let result = manager.revoke_client(&client.client_id).await;

        assert!(matches!(result, Err(DownstreamOauthError::Persistence(_))));
        assert!(manager.durability_degraded());
        assert!(!lease.is_active());
        assert_eq!(
            manager
                .validate_access_token_for(
                    &tokens.access_token,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp",
                )
                .await,
            AccessTokenValidation::Invalid
        );
        let disk = load_persisted_state(&path).expect("read committed revocation");
        assert!(disk.revoked_client_ids.contains(&client.client_id));
        drop(manager);

        fail_next_parent_dir_sync_attempts_for_tests(PARENT_DIR_SYNC_ATTEMPTS);
        let restart_error =
            DownstreamOauthManager::new_with_state_path(test_config(), path.clone()).expect_err(
                "restart must refuse service while directory durability remains uncertain",
            );
        assert!(restart_error.to_string().contains("startup directory sync"));

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart from committed revocation");
        assert!(
            restarted
                .state
                .lock()
                .await
                .revoked_client_ids
                .contains(&client.client_id)
        );
        assert_eq!(
            restarted
                .validate_access_token_for(
                    &tokens.access_token,
                    &["tools:read".to_string()],
                    "https://plug.example.com/mcp",
                )
                .await,
            AccessTokenValidation::Invalid
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_rename_failure_publishes_nothing_and_restart_matches() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        fail_next_state_rename_for_tests();

        let result = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "pre-rename-failure",
                code_challenge: "failure-challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await;

        assert!(matches!(result, Err(DownstreamOauthError::Persistence(_))));
        assert!(!manager.durability_degraded());
        assert!(manager.state.lock().await.pending_consents.is_empty());
        assert!(
            load_persisted_state(&path)
                .expect("read unchanged disk state")
                .pending_consents
                .is_empty()
        );
        drop(manager);
        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart unchanged state");
        assert!(restarted.state.lock().await.pending_consents.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parent_directory_sync_retry_can_recover_without_degrading() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        fail_next_parent_dir_sync_attempts_for_tests(1);

        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "retry-recovers",
                code_challenge: "retry-challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("second directory sync attempt succeeds");

        assert!(!manager.durability_degraded());
        assert!(
            manager
                .state
                .lock()
                .await
                .pending_consents
                .contains_key(&consent.consent_id)
        );
    }

    #[test]
    fn persistence_temp_paths_are_unique_per_writer() {
        let path = temp_state_path();
        let first = temporary_state_path(&path);
        let second = temporary_state_path(&path);

        assert_eq!(first.parent(), path.parent());
        assert_eq!(second.parent(), path.parent());
        assert_ne!(first, second);
        assert_ne!(first, path.with_extension("json.tmp"));
        assert_ne!(second, path.with_extension("json.tmp"));
    }

    #[test]
    fn issuer_state_allows_only_one_live_writer() {
        let path = temp_state_path();
        let first = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect("first writer acquires issuer lock");

        let error = DownstreamOauthManager::new_with_state_path(test_config(), path.clone())
            .expect_err("second writer must fail instead of racing state publication");
        assert!(error.to_string().contains("already active"));

        drop(first);
        DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("writer lock is released with manager");
    }

    #[tokio::test]
    async fn pending_code_survives_restart() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "restart-state",
                code_challenge: "restart-challenge",
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
        let code = url::Url::parse(&redirect.location)
            .expect("redirect URL")
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("authorization code");
        drop(manager);

        let restarted = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("restart manager");

        assert!(
            restarted
                .state
                .lock()
                .await
                .pending_codes
                .contains_key(&code)
        );
    }

    #[tokio::test]
    async fn startup_evicts_expired_short_lived_records() {
        let now = epoch_secs();
        let fixture = serde_json::json!({
            "version": 3,
            "clients": {},
            "pending_consents": {
                "expired-consent": {
                    "client_id": "plug_expired",
                    "redirect_uri": "https://client.example/callback",
                    "state": "state",
                    "code_challenge": "challenge",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "csrf_token": "expired-csrf",
                    "expires_at": now
                }
            },
            "completed_consents": {
                "expired-completed": {
                    "client_id": "plug_expired",
                    "redirect": { "location": "https://client.example/callback" },
                    "expires_at": now
                }
            },
            "pending_codes": {
                "expired-code": {
                    "client_id": "plug_expired",
                    "redirect_uri": "https://client.example/callback",
                    "code_challenge": "challenge",
                    "scopes": ["tools:read"],
                    "resource": "https://plug.example.com/mcp",
                    "expires_at": now
                }
            },
            "access_tokens": {},
            "refresh_tokens": {},
            "revoked_client_ids": []
        });
        let path = write_state_fixture(fixture);

        let manager = DownstreamOauthManager::new_with_state_path(test_config(), path)
            .expect("load version 3 state");
        let state = manager.state.lock().await;

        assert!(state.pending_consents.is_empty());
        assert!(state.completed_consents.is_empty());
        assert!(state.pending_codes.is_empty());
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

    #[derive(Clone, Copy)]
    enum CapabilityFixtureKind {
        DynamicRegistration,
        MetadataDocument(&'static str),
    }

    fn validate_recorded_capability_fixture(
        kind: CapabilityFixtureKind,
        document: &str,
    ) -> Result<(), DownstreamOauthError> {
        match kind {
            CapabilityFixtureKind::DynamicRegistration => {
                let request: ClientRegistrationRequest =
                    serde_json::from_str(document).expect("recorded DCR fixture");
                validate_registration_request(&request)
            }
            CapabilityFixtureKind::MetadataDocument(client_id) => {
                let document: ClientMetadataDocument =
                    serde_json::from_str(document).expect("recorded CIMD fixture");
                validate_metadata_document(client_id, &document)
            }
        }
    }

    #[tokio::test]
    async fn repeated_consent_approval_replays_first_redirect_and_mints_one_code() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-123",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");

        let first = manager
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("approve consent");
        let repeated = manager
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("repeat approval");
        let later_denial = manager
            .decide_consent(&consent.consent_id, false)
            .await
            .expect("later denial");

        assert_eq!(repeated.location, first.location);
        assert_eq!(later_denial.location, first.location);
        assert_eq!(manager.state.lock().await.pending_codes.len(), 1);
    }

    #[tokio::test]
    async fn repeated_consent_denial_replays_first_access_denied_redirect() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-123",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");

        let first = manager
            .decide_consent(&consent.consent_id, false)
            .await
            .expect("deny consent");
        let repeated = manager
            .decide_consent(&consent.consent_id, false)
            .await
            .expect("repeat denial");

        assert_eq!(repeated.location, first.location);
        assert_eq!(
            first.location,
            "http://localhost:8787/callback?error=access_denied&state=state-123"
        );
        assert!(manager.state.lock().await.pending_codes.is_empty());
    }

    #[tokio::test]
    async fn completed_denials_count_toward_outstanding_consent_cap() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;

        for index in 0..MAX_PENDING_CONSENTS_PER_CLIENT {
            let consent = manager
                .begin_authorization(AuthorizationRequest {
                    response_type: "code",
                    client_id: &client.client_id,
                    redirect_uri: &client.redirect_uris[0],
                    state: &format!("state-{index}"),
                    code_challenge: "challenge",
                    code_challenge_method: "S256",
                    scope: Some("tools:read"),
                    resource: "https://plug.example.com/mcp",
                })
                .await
                .expect("begin authorization within cap");
            manager
                .decide_consent(&consent.consent_id, false)
                .await
                .expect("deny consent");
        }

        let limited = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-over-cap",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await;

        assert_eq!(
            limited.expect_err("completed decisions must keep capacity reserved"),
            DownstreamOauthError::RateLimited
        );
        let state = manager.state.lock().await;
        assert!(state.pending_consents.is_empty());
        assert_eq!(
            state.completed_consents.len(),
            MAX_PENDING_CONSENTS_PER_CLIENT
        );
        drop(state);

        for completed in manager.state.lock().await.completed_consents.values_mut() {
            completed.expires_at = 0;
        }
        let after_expiry = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-after-expiry",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await;
        assert!(
            after_expiry.is_ok(),
            "expired completed decisions must release capacity: {after_expiry:?}"
        );
    }

    #[tokio::test]
    async fn completed_consents_persist_expire_and_clear_on_revocation() {
        let (manager, path) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-123",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");
        manager
            .decide_consent(&consent.consent_id, false)
            .await
            .expect("deny consent");

        let loaded = load_persisted_state(&path).expect("reload persisted state");
        assert!(loaded.pending_consents.is_empty());
        assert!(loaded.completed_consents.contains_key(&consent.consent_id));

        {
            let mut state = manager.state.lock().await;
            state
                .completed_consents
                .get_mut(&consent.consent_id)
                .expect("completed consent")
                .expires_at = 0;
            state.evict_expired(epoch_secs());
            assert!(state.completed_consents.is_empty());
        }

        let second = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "state-456",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin second authorization");
        manager
            .decide_consent(&second.consent_id, false)
            .await
            .expect("deny second consent");
        assert!(manager.revoke_client(&client.client_id).await.unwrap());
        assert!(manager.state.lock().await.completed_consents.is_empty());
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
    fn recorded_metadata_document_accepts_known_extension_superset() {
        let document: ClientMetadataDocument = serde_json::from_str(
            r#"{
                "client_id": "https://claude.ai/oauth/mcp-oauth-client-metadata",
                "client_name": "Claude",
                "client_uri": "https://claude.ai",
                "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
                "grant_types": [
                    "authorization_code",
                    "refresh_token",
                    "urn:ietf:params:oauth:grant-type:jwt-bearer"
                ],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none"
            }"#,
        )
        .expect("known metadata document fixture");

        assert_eq!(
            validate_metadata_document(
                "https://claude.ai/oauth/mcp-oauth-client-metadata",
                &document,
            ),
            Ok(())
        );
    }

    #[test]
    fn client_neutral_capability_fixture_matrix() {
        for (class, kind, document, expected) in [
            (
                "strict DCR",
                CapabilityFixtureKind::DynamicRegistration,
                r#"{
                    "client_name": "Strict public client",
                    "redirect_uris": ["https://client.example/callback"],
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"]
                }"#,
                Ok(()),
            ),
            (
                "baseline CIMD",
                CapabilityFixtureKind::MetadataDocument("https://client.example/metadata.json"),
                r#"{
                    "client_id": "https://client.example/metadata.json",
                    "client_name": "Baseline metadata client",
                    "redirect_uris": ["https://client.example/callback"],
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"]
                }"#,
                Ok(()),
            ),
            (
                "extension-rich CIMD",
                CapabilityFixtureKind::MetadataDocument("https://client.example/metadata.json"),
                r#"{
                    "client_id": "https://client.example/metadata.json",
                    "client_name": "Extension-rich metadata client",
                    "redirect_uris": ["https://client.example/callback"],
                    "token_endpoint_auth_method": "none",
                    "grant_types": [
                        "authorization_code",
                        "refresh_token",
                        "urn:example:grant-type:future"
                    ],
                    "response_types": ["code", "future"]
                }"#,
                Ok(()),
            ),
            (
                "missing authorization-code grant CIMD",
                CapabilityFixtureKind::MetadataDocument("https://client.example/metadata.json"),
                r#"{
                    "client_id": "https://client.example/metadata.json",
                    "client_name": "Missing code grant metadata client",
                    "redirect_uris": ["https://client.example/callback"],
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["urn:example:grant-type:future"],
                    "response_types": ["code"]
                }"#,
                Err(DownstreamOauthError::InvalidClientMetadata),
            ),
            (
                "missing code response type CIMD",
                CapabilityFixtureKind::MetadataDocument("https://client.example/metadata.json"),
                r#"{
                    "client_id": "https://client.example/metadata.json",
                    "client_name": "Missing code response metadata client",
                    "redirect_uris": ["https://client.example/callback"],
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["authorization_code"],
                    "response_types": ["future"]
                }"#,
                Err(DownstreamOauthError::InvalidClientMetadata),
            ),
        ] {
            assert_eq!(
                validate_recorded_capability_fixture(kind, document),
                expected,
                "{class}"
            );
        }
    }

    #[test]
    fn dynamic_registration_does_not_adopt_unimplemented_grants() {
        let request = ClientRegistrationRequest {
            redirect_uris: vec!["https://client.example/callback".to_string()],
            client_name: Some("Client".to_string()),
            token_endpoint_auth_method: Some("none".to_string()),
            grant_types: Some(vec![
                "authorization_code".to_string(),
                "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string(),
            ]),
            response_types: Some(vec!["code".to_string()]),
            scope: None,
        };
        assert_eq!(
            validate_registration_request(&request),
            Err(DownstreamOauthError::InvalidClientMetadata)
        );
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
        drop(restarted);
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
    async fn committed_revocation_removes_lifecycle_entry_but_held_lease_stays_inactive() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let tokens = issue_tokens(&manager, &client).await;
        let lease = match manager
            .validate_access_token_for(
                &tokens.access_token,
                &["tools:read".to_string()],
                "https://plug.example.com/mcp",
            )
            .await
        {
            AccessTokenValidation::Valid(claims) => claims.principal_lifecycle,
            other => panic!("token must validate before revocation: {other:?}"),
        };

        assert!(manager.revoke_client(&client.client_id).await.unwrap());

        assert!(!manager.principal_lifecycles.contains_key(&client.client_id));
        assert!(!lease.is_active());
    }

    #[tokio::test]
    async fn committed_expiry_removes_lifecycle_entry_but_held_lease_stays_inactive() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let tokens = issue_tokens(&manager, &client).await;
        let lease = match manager
            .validate_access_token_for(
                &tokens.access_token,
                &["tools:read".to_string()],
                "https://plug.example.com/mcp",
            )
            .await
        {
            AccessTokenValidation::Valid(claims) => claims.principal_lifecycle,
            other => panic!("token must validate before expiry: {other:?}"),
        };
        manager
            .state
            .lock()
            .await
            .clients
            .get_mut(&client.client_id)
            .expect("client")
            .expires_at = 0;
        manager.registration_rate.lock().await.clear();

        register(&manager, "expiry-trigger", "http://localhost:8788/callback").await;

        assert!(!manager.principal_lifecycles.contains_key(&client.client_id));
        assert!(!lease.is_active());
    }

    #[tokio::test]
    async fn repeated_registration_and_revocation_does_not_grow_lifecycle_map() {
        let (manager, _) = test_manager();
        for index in 0..32 {
            manager.registration_rate.lock().await.clear();
            let client = register(
                &manager,
                &format!("client-{index}"),
                "http://localhost:8787/callback",
            )
            .await;
            assert!(manager.revoke_client(&client.client_id).await.unwrap());
        }

        assert!(manager.principal_lifecycles.is_empty());
    }

    #[tokio::test]
    async fn validation_before_revocation_cannot_enqueue_after_revocation_barrier() {
        let (manager, _) = test_manager();
        let client = register(&manager, "Cursor", "http://localhost:8787/callback").await;
        let tokens = issue_tokens(&manager, &client).await;

        let server_manager = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager,
            crate::proxy::RouterConfig {
                prefix_delimiter: "__".to_string(),
                priority_tools: Vec::new(),
                disabled_tools: Vec::new(),
                tool_description_max_chars: None,
                tool_search_threshold: 50,
                meta_tool_mode: false,
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        router.replace_snapshot(crate::proxy::RouterSnapshot {
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),

            routes: std::collections::HashMap::from([(
                "Mock__echo".to_string(),
                ("missing".to_string(), "echo".to_string()),
            )]),
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(Vec::new()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: std::collections::HashMap::new(),
            prompt_routes: std::collections::HashMap::new(),
            tool_definition_fingerprints: std::collections::HashMap::new(),
            tool_risk_inventory: std::collections::HashMap::new(),
        });

        let principal = crate::types::PrincipalId::downstream_oauth(
            manager.base_url(),
            client.client_id.clone(),
            manager.resource(),
        );
        let owner = crate::tasks::TaskOwner::new(principal.owner_key());
        let request_manager = manager.clone();
        let request_router = Arc::clone(&router);
        let request_owner = owner.clone();
        let access_token = tokens.access_token.clone();
        let (validated_tx, validated_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();

        let request = tokio::spawn(async move {
            let claims = match request_manager
                .validate_access_token_for(
                    &access_token,
                    &["tools:read".to_string()],
                    &request_manager.resource(),
                )
                .await
            {
                AccessTokenValidation::Valid(claims) => claims,
                other => panic!("token must validate before barrier: {other:?}"),
            };
            validated_tx.send(()).expect("signal validated request");
            resume_rx.await.expect("release request after revocation");

            let context = crate::proxy::DownstreamCallContext::http_for_client_with_trace(
                "session-before-revoke",
                rmcp::model::RequestId::from(rmcp::model::NumberOrString::Number(1)),
                crate::types::ClientType::Unknown,
                Arc::<str>::from("00000000000000000000000000000001"),
            )
            .with_authorization(
                crate::types::PrincipalId::downstream_oauth(
                    request_manager.base_url(),
                    claims.client_id,
                    claims.resource,
                ),
                ["tools:read".to_string(), "tasks:use".to_string()],
            )
            .with_principal_lifecycle(claims.principal_lifecycle);

            request_router
                .enqueue_tool_task("Mock__echo", None, None, request_owner, None, Some(context))
                .await
        });

        validated_rx
            .await
            .expect("request reached validation barrier");
        assert!(
            manager
                .revoke_client(&client.client_id)
                .await
                .expect("revoke client")
        );
        router.cleanup_tasks_for_owner(&owner).await;
        resume_tx.send(()).expect("release revoked request");

        let error = request
            .await
            .expect("request task")
            .expect_err("revoked validation generation must fail before task creation");
        assert_eq!(error.code, rmcp::model::ErrorCode(-32001));
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
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
