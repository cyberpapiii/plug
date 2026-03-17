//! OAuth credential store and auth manager for upstream MCP server authentication.
//!
//! Provides [`CompositeCredentialStore`] (OS keyring + file fallback) and
//! [`CompositeStateStore`] (file-based PKCE state) that implement the rmcp
//! `CredentialStore` and `StateStore` traits respectively.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use dashmap::DashMap;
use rmcp::transport::auth::{
    AuthError, CredentialStore, StateStore, StoredAuthorizationState, StoredCredentials,
};
use tracing::{debug, warn};

use crate::config;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default token lifetime when `expires_in` is missing or invalid.
pub const DEFAULT_TOKEN_LIFETIME_SECS: u64 = 3600;

/// Refresh window — refresh this many seconds before expiry.
pub const TOKEN_REFRESH_WINDOW_SECS: u64 = 300;

/// Minimum clamped `expires_in` value.
const MIN_EXPIRES_IN: u64 = 60;

/// Maximum clamped `expires_in` value.
const MAX_EXPIRES_IN: u64 = 86400;

/// Short-lived threshold: if `expires_in < 600`, use the 50% rule.
const SHORT_LIVED_THRESHOLD: u64 = 600;

// ---------------------------------------------------------------------------
// Global store registry
// ---------------------------------------------------------------------------

static STORES: LazyLock<DashMap<String, Arc<CompositeCredentialStore>>> =
    LazyLock::new(DashMap::new);

/// Get or create a per-server [`CompositeCredentialStore`].
///
/// Callers may share the returned `Arc` freely; stores are lazily
/// created and live for the process lifetime.
pub fn get_or_create_store(server_name: &str) -> Arc<CompositeCredentialStore> {
    STORES
        .entry(server_name.to_string())
        .or_insert_with(|| Arc::new(CompositeCredentialStore::new(server_name.to_string())))
        .clone()
}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

/// Return the `~/.config/plug/tokens/` directory path.
pub fn tokens_dir() -> PathBuf {
    config::config_dir().join("tokens")
}

/// Normalise an `expires_in` value: apply defaults and clamping.
fn effective_expires_in(expires_in: Option<u64>) -> u64 {
    match expires_in {
        Some(0) | None => DEFAULT_TOKEN_LIFETIME_SECS,
        Some(v) => v.clamp(MIN_EXPIRES_IN, MAX_EXPIRES_IN),
    }
}

/// Determine whether a token should be refreshed now.
///
/// Rules:
/// - If `expires_in` is `None`, `0`: use [`DEFAULT_TOKEN_LIFETIME_SECS`].
/// - Clamp to `[60, 86400]`.
/// - If `expires_in < 600`: refresh at 50% of lifetime.
/// - Otherwise: refresh at `expires_in - TOKEN_REFRESH_WINDOW_SECS`.
pub fn token_needs_refresh(received_at: u64, expires_in: Option<u64>) -> bool {
    let effective = effective_expires_in(expires_in);
    let refresh_at = if effective < SHORT_LIVED_THRESHOLD {
        effective / 2
    } else {
        effective.saturating_sub(TOKEN_REFRESH_WINDOW_SECS)
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let elapsed = now.saturating_sub(received_at);
    elapsed >= refresh_at
}

/// Compute the [`Duration`] until the refresh window opens.
///
/// Returns [`Duration::ZERO`] if the window is already open.
pub fn time_until_refresh_window(received_at: u64, expires_in: Option<u64>) -> Duration {
    let effective = effective_expires_in(expires_in);
    let refresh_at = if effective < SHORT_LIVED_THRESHOLD {
        effective / 2
    } else {
        effective.saturating_sub(TOKEN_REFRESH_WINDOW_SECS)
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let elapsed = now.saturating_sub(received_at);
    if elapsed >= refresh_at {
        Duration::ZERO
    } else {
        Duration::from_secs(refresh_at - elapsed)
    }
}

/// Get the current access token for a server from the in-memory cache.
///
/// Returns `None` if no token is cached for the server.
pub fn current_access_token(server_name: &str) -> Option<String> {
    let store = STORES.get(server_name)?;
    let guard = store.cache.load();
    let cached = guard.as_ref().as_ref()?;
    Some(cached.access_token.clone())
}

/// Return the current access token for a server, hydrating the in-memory cache
/// from persisted credentials on cache miss.
///
/// This lets a fresh daemon/runtime recover after a successful `plug auth login`
/// without requiring an injected-token IPC path to populate the cache first.
pub async fn current_or_stored_access_token(server_name: &str) -> Option<String> {
    if let Some(token) = current_access_token(server_name) {
        return Some(token);
    }

    let store = get_or_create_store(server_name);
    let creds = store.load().await.ok().flatten()?;

    use oauth2::TokenResponse;
    creds
        .token_response
        .as_ref()
        .map(|tr| tr.access_token().secret().to_string())
}

// ---------------------------------------------------------------------------
// CachedCredentials
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CachedCredentials {
    access_token: String,
    /// Epoch seconds when the token was received. Used by refresh checks.
    #[allow(dead_code)]
    token_received_at: u64,
    /// Token lifetime in seconds. Used by refresh checks.
    #[allow(dead_code)]
    expires_in: Option<u64>,
}

impl std::fmt::Debug for CachedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedCredentials")
            .field("access_token", &"[REDACTED]")
            .field("token_received_at", &self.token_received_at)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// CompositeCredentialStore
// ---------------------------------------------------------------------------

/// Credential store that tries the OS keyring first, falling back to a JSON
/// file at `~/.config/plug/tokens/{server_name}.json`.
///
/// An in-memory [`ArcSwap`] cache avoids I/O on the hot path.
pub struct CompositeCredentialStore {
    server_name: String,
    cache: Arc<ArcSwap<Option<CachedCredentials>>>,
}

impl CompositeCredentialStore {
    /// Create a new store for `server_name`.
    pub fn new(server_name: String) -> Self {
        Self {
            server_name,
            cache: Arc::new(ArcSwap::from_pointee(None)),
        }
    }

    // -- keyring helpers --------------------------------------------------

    fn keyring_entry(&self) -> Option<keyring::Entry> {
        let user = format!("oauth:{}", self.server_name);
        match keyring::Entry::new("plug", &user) {
            Ok(entry) => Some(entry),
            Err(e) => {
                debug!(server = %self.server_name, error = %e, "keyring entry creation failed");
                None
            }
        }
    }

    fn keyring_load(&self) -> Option<StoredCredentials> {
        let entry = self.keyring_entry()?;
        match entry.get_password() {
            Ok(json) => match serde_json::from_str::<StoredCredentials>(&json) {
                Ok(creds) => Some(creds),
                Err(e) => {
                    warn!(server = %self.server_name, error = %e, "keyring: invalid JSON, ignoring");
                    None
                }
            },
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                debug!(server = %self.server_name, error = %e, "keyring: load failed");
                None
            }
        }
    }

    fn keyring_save(&self, creds: &StoredCredentials) -> bool {
        let entry = match self.keyring_entry() {
            Some(e) => e,
            None => return false,
        };
        let json = match serde_json::to_string(creds) {
            Ok(j) => j,
            Err(e) => {
                warn!(server = %self.server_name, error = %e, "keyring: serialization failed");
                return false;
            }
        };
        match entry.set_password(&json) {
            Ok(()) => {
                debug!(server = %self.server_name, "keyring: credentials saved");
                true
            }
            Err(e) => {
                debug!(server = %self.server_name, error = %e, "keyring: save failed");
                false
            }
        }
    }

    fn keyring_clear(&self) {
        if let Some(entry) = self.keyring_entry() {
            match entry.delete_credential() {
                Ok(()) => debug!(server = %self.server_name, "keyring: credential deleted"),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => {
                    debug!(server = %self.server_name, error = %e, "keyring: delete failed");
                }
            }
        }
    }

    // -- file helpers -----------------------------------------------------

    fn token_file_path(&self) -> Result<PathBuf, AuthError> {
        let safe = config::sanitize_server_name_for_path(&self.server_name).map_err(|e| {
            AuthError::InternalError(format!("invalid server name for file path: {e}"))
        })?;
        Ok(tokens_dir().join(format!("{safe}.json")))
    }

    fn file_load(&self) -> Option<StoredCredentials> {
        let path = match self.token_file_path() {
            Ok(p) => p,
            Err(_) => return None,
        };
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!(server = %self.server_name, error = %e, "token file: read failed");
                return None;
            }
        };
        match serde_json::from_str::<StoredCredentials>(&data) {
            Ok(creds) => Some(creds),
            Err(e) => {
                warn!(server = %self.server_name, error = %e, "token file: invalid JSON, ignoring");
                None
            }
        }
    }

    fn file_save(&self, creds: &StoredCredentials) -> Result<(), AuthError> {
        use fs2::FileExt;
        use std::io::Write;

        let path = self.token_file_path()?;
        let dir = path.parent().ok_or_else(|| {
            AuthError::InternalError("token file path has no parent directory".into())
        })?;
        std::fs::create_dir_all(dir)
            .map_err(|e| AuthError::InternalError(format!("failed to create tokens dir: {e}")))?;

        let json = serde_json::to_string_pretty(creds)
            .map_err(|e| AuthError::InternalError(format!("serialization failed: {e}")))?;

        // Atomic write: write to temp file in the same directory, then rename.
        let tmp_path = path.with_extension("json.tmp");

        // Open with exclusive lock for cross-process safety.
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)
        };
        #[cfg(not(unix))]
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path);

        let mut file = file.map_err(|e| {
            AuthError::InternalError(format!("failed to open temp token file: {e}"))
        })?;

        file.lock_exclusive().map_err(|e| {
            AuthError::InternalError(format!("failed to lock temp token file: {e}"))
        })?;

        file.write_all(json.as_bytes()).map_err(|e| {
            let _ = FileExt::unlock(&file);
            AuthError::InternalError(format!("failed to write temp token file: {e}"))
        })?;

        FileExt::unlock(&file).map_err(|e| {
            AuthError::InternalError(format!("failed to unlock temp token file: {e}"))
        })?;

        // Atomic rename.
        std::fs::rename(&tmp_path, &path).map_err(|e| {
            // Clean up temp file on rename failure.
            let _ = std::fs::remove_file(&tmp_path);
            AuthError::InternalError(format!("failed to rename temp token file: {e}"))
        })?;

        // Ensure final file has 0600 permissions (rename preserves tmp perms
        // on most Unix systems, but be explicit).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }

        debug!(server = %self.server_name, "token file: credentials saved");
        Ok(())
    }

    fn file_clear(&self) {
        if let Ok(path) = self.token_file_path() {
            match std::fs::remove_file(&path) {
                Ok(()) => debug!(server = %self.server_name, "token file: deleted"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    debug!(server = %self.server_name, error = %e, "token file: delete failed");
                }
            }
        }
    }

    // -- cache helpers ----------------------------------------------------

    fn update_cache(&self, creds: &StoredCredentials) {
        let cached = creds.token_response.as_ref().map(|tr| {
            // Access the access token via the oauth2 TokenResponse trait.
            // OAuthTokenResponse = StandardTokenResponse which has .access_token() -> &AccessToken
            // and .expires_in() -> Option<Duration>.
            use oauth2::TokenResponse;
            let access_token = tr.access_token().secret().to_string();
            let expires_in = tr.expires_in().map(|d| d.as_secs());
            let token_received_at = creds.token_received_at.unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            CachedCredentials {
                access_token,
                token_received_at,
                expires_in,
            }
        });
        self.cache.store(Arc::new(cached));
    }

    fn clear_cache(&self) {
        self.cache.store(Arc::new(None));
    }

    /// Return the cached token timing info for refresh-check decisions.
    ///
    /// Returns `(token_received_at, expires_in)` or `None` if no token is cached.
    pub fn cached_expiry(&self) -> Option<(u64, Option<u64>)> {
        let guard = self.cache.load();
        let cached = guard.as_ref().as_ref()?;
        Some((cached.token_received_at, cached.expires_in))
    }
}

#[async_trait]
impl CredentialStore for CompositeCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // 1. Check in-memory cache — if present, the backing stores were
        //    already read at least once, so we can skip I/O for pure expiry
        //    checks.  However, the full StoredCredentials must be returned
        //    so we still hit the backing stores.

        // 2. Try keyring.
        if let Some(creds) = self.keyring_load() {
            self.update_cache(&creds);
            return Ok(Some(creds));
        }

        // 3. Try file.
        if let Some(creds) = self.file_load() {
            self.update_cache(&creds);
            return Ok(Some(creds));
        }

        Ok(None)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        // Try keyring first.
        let keyring_ok = self.keyring_save(&credentials);

        // Always try file as well (independent of keyring result).
        let file_result = self.file_save(&credentials);

        // Update in-memory cache regardless of backing-store outcome.
        self.update_cache(&credentials);

        // If both backends failed, propagate the file error.
        if !keyring_ok {
            file_result?;
        }

        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.keyring_clear();
        self.file_clear();
        self.clear_cache();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CompositeStateStore
// ---------------------------------------------------------------------------

/// File-based PKCE state store. Each state entry is stored at
/// `~/.config/plug/tokens/{server_name}_state_{csrf}.json`.
pub struct CompositeStateStore {
    server_name: String,
}

impl CompositeStateStore {
    /// Create a new state store for `server_name`.
    pub fn new(server_name: String) -> Self {
        Self { server_name }
    }

    fn state_file_path(&self, csrf_token: &str) -> Result<PathBuf, AuthError> {
        let safe_server =
            config::sanitize_server_name_for_path(&self.server_name).map_err(|e| {
                AuthError::InternalError(format!("invalid server name for state path: {e}"))
            })?;
        // Sanitise the CSRF token for use in filenames — replace non-alphanumeric
        // chars to avoid path injection.
        let safe_csrf: String = csrf_token
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Ok(tokens_dir().join(format!("{safe_server}_state_{safe_csrf}.json")))
    }
}

#[async_trait]
impl StateStore for CompositeStateStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        let path = self.state_file_path(csrf_token)?;
        let dir = path.parent().ok_or_else(|| {
            AuthError::InternalError("state file path has no parent directory".into())
        })?;
        std::fs::create_dir_all(dir)
            .map_err(|e| AuthError::InternalError(format!("failed to create tokens dir: {e}")))?;

        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| AuthError::InternalError(format!("state serialization failed: {e}")))?;

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| AuthError::InternalError(format!("failed to open state file: {e}")))?;
            file.write_all(json.as_bytes()).map_err(|e| {
                AuthError::InternalError(format!("failed to write state file: {e}"))
            })?;
        }

        #[cfg(not(unix))]
        std::fs::write(&path, &json)
            .map_err(|e| AuthError::InternalError(format!("failed to write state file: {e}")))?;

        debug!(server = %self.server_name, "state file saved for CSRF flow");
        Ok(())
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        let path = self.state_file_path(csrf_token)?;
        match std::fs::read_to_string(&path) {
            Ok(data) => {
                let state: StoredAuthorizationState = serde_json::from_str(&data).map_err(|e| {
                    AuthError::InternalError(format!("state deserialization failed: {e}"))
                })?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AuthError::InternalError(format!(
                "failed to read state file: {e}"
            ))),
        }
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        let path = self.state_file_path(csrf_token)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                debug!(server = %self.server_name, "state file deleted");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::InternalError(format!(
                "failed to delete state file: {e}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

/// Outcome of an OAuth token refresh attempt.
#[derive(Debug)]
pub enum RefreshResult {
    /// Token was successfully refreshed and the global cache updated.
    Refreshed,
    /// Cannot refresh: credentials were injected (no real OAuth client).
    InjectedToken,
    /// Cannot refresh: stored credentials have no refresh_token.
    NoRefreshToken,
    /// Cannot refresh: no stored credentials at all.
    NoCredentials,
    /// Refresh rejected by the authorization server (token revoked, etc.).
    AuthError(String),
    /// Refresh failed due to a transient error (network, metadata, etc.).
    TransientError(String),
}

fn has_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();

    if needle_bytes.is_empty() || bytes.len() < needle_bytes.len() {
        return false;
    }

    bytes
        .windows(needle_bytes.len())
        .enumerate()
        .any(|(idx, window)| {
            if window != needle_bytes {
                return false;
            }

            let start_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
            let end_idx = idx + needle_bytes.len();
            let end_ok = end_idx == bytes.len() || !bytes[end_idx].is_ascii_alphanumeric();

            start_ok && end_ok
        })
}

pub(crate) fn is_auth_error(message: &str) -> bool {
    let normalized = message.to_lowercase();

    normalized.contains("authorization required")
        || has_token(&normalized, "invalid_grant")
        || has_token(&normalized, "invalid_token")
        || has_token(&normalized, "unauthorized")
        || has_token(&normalized, "401")
}

/// Attempt to refresh the OAuth access token for `server_name`.
///
/// Builds an [`rmcp::transport::auth::AuthorizationManager`], discovers
/// server metadata, configures the OAuth client, and calls `refresh_token()`
/// to exchange the stored refresh token for a fresh access token.
///
/// On success the backing stores (keyring/file) and the global in-memory
/// cache are updated before returning.
///
/// Returns [`RefreshResult::InjectedToken`] when `client_id == "injected"`.
pub async fn refresh_access_token(
    server_name: &str,
    server_url: &str,
    oauth_client_id: Option<&str>,
) -> RefreshResult {
    use rmcp::transport::auth::{AuthorizationManager, OAuthClientConfig};

    // Ensure the TLS provider is available (reqwest needs it even for HTTP).
    crate::tls::ensure_rustls_provider_installed();

    // 1. Load stored credentials from the global store.
    let store = get_or_create_store(server_name);
    let creds = match CredentialStore::load(&*store).await {
        Ok(Some(c)) => c,
        Ok(None) => return RefreshResult::NoCredentials,
        Err(e) => return RefreshResult::TransientError(format!("failed to load credentials: {e}")),
    };

    // 2. Injected tokens have no real OAuth client — cannot refresh.
    if creds.client_id == "injected" {
        return RefreshResult::InjectedToken;
    }

    // 3. A refresh_token must be present.
    let has_refresh = creds.token_response.as_ref().is_some_and(|tr| {
        use oauth2::TokenResponse;
        tr.refresh_token().is_some()
    });
    if !has_refresh {
        return RefreshResult::NoRefreshToken;
    }

    // 4. Build an AuthorizationManager for the server URL.
    let mut auth_manager = match AuthorizationManager::new(server_url).await {
        Ok(m) => m,
        Err(e) => {
            return RefreshResult::TransientError(format!("failed to init auth manager: {e}"));
        }
    };

    // Fresh store instance sharing the same backing files/keyring — we
    // cannot reuse the global Arc because `set_credential_store` takes
    // ownership.
    let refresh_store = CompositeCredentialStore::new(server_name.to_string());
    auth_manager.set_credential_store(refresh_store);

    // 5. Discover OAuth server metadata.
    let metadata = match auth_manager.discover_metadata().await {
        Ok(m) => m,
        Err(e) => return RefreshResult::TransientError(format!("metadata discovery failed: {e}")),
    };
    auth_manager.set_metadata(metadata);

    // 6. Configure the OAuth client.
    let client_id = oauth_client_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| creds.client_id.clone());

    let config = OAuthClientConfig {
        client_id,
        client_secret: None,
        scopes: creds.granted_scopes.clone(),
        redirect_uri: "http://localhost:0/callback".to_string(),
    };
    if let Err(e) = auth_manager.configure_client(config) {
        return RefreshResult::TransientError(format!("failed to configure client: {e}"));
    }

    // 7. Exchange refresh_token for a new access_token.
    match auth_manager.refresh_token().await {
        Ok(_) => {
            // rmcp's refresh_token() saves via the fresh store's backing
            // files/keyring.  Reload the global store's cache so the next
            // `current_access_token()` call returns the new token.
            if let Err(e) = CredentialStore::load(&*store).await {
                return RefreshResult::TransientError(format!(
                    "token refreshed but cache reload failed: {e}"
                ));
            }
            debug!(server = %server_name, "OAuth token refreshed successfully");
            RefreshResult::Refreshed
        }
        Err(e) => {
            let err_str = e.to_string();
            if is_auth_error(&err_str) {
                RefreshResult::AuthError(format!("{e}"))
            } else {
                RefreshResult::TransientError(format!("{e}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Token just received — should not need refresh.
    #[test]
    fn test_token_needs_refresh_not_due() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!token_needs_refresh(now, Some(3600)));
    }

    /// Token near expiry — should need refresh.
    #[test]
    fn test_token_needs_refresh_due() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Received 3500 seconds ago with expires_in = 3600.
        // Refresh window is 3600 - 300 = 3300 seconds. 3500 >= 3300 → true.
        assert!(token_needs_refresh(now - 3500, Some(3600)));
    }

    /// When `expires_in` is `None`, uses the default lifetime.
    #[test]
    fn test_token_needs_refresh_none_expires_in() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Default lifetime is 3600, refresh at 3300.
        // Received 100s ago → not due.
        assert!(!token_needs_refresh(now - 100, None));
        // Received 3400s ago → due (3400 >= 3300).
        assert!(token_needs_refresh(now - 3400, None));
    }

    /// Short-lived token (expires_in < 600) uses the 50% rule.
    #[test]
    fn test_token_needs_refresh_short_lived() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // expires_in = 120, clamped to 120 (>= 60). Short-lived: refresh at 60s.
        assert!(!token_needs_refresh(now - 30, Some(120))); // 30 < 60
        assert!(token_needs_refresh(now - 70, Some(120))); // 70 >= 60
    }

    /// Basic computation of time until refresh window.
    #[test]
    fn test_time_until_refresh_window() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // expires_in = 3600, refresh at 3300. Received now → 3300s until refresh.
        let dur = time_until_refresh_window(now, Some(3600));
        // Allow 2s tolerance for test execution time.
        assert!(dur.as_secs() >= 3298 && dur.as_secs() <= 3300);

        // Already past refresh window → zero.
        let dur = time_until_refresh_window(now - 3500, Some(3600));
        assert_eq!(dur, Duration::ZERO);
    }

    #[test]
    fn test_is_auth_error_matches_known_auth_failures() {
        assert!(is_auth_error("OAuth authorization required"));
        assert!(is_auth_error("OAuth token refresh failed: invalid_grant"));
        assert!(is_auth_error("server returned 401 unauthorized"));
    }

    #[test]
    fn test_is_auth_error_rejects_metadata_discovery_false_positive() {
        assert!(!is_auth_error(
            "metadata discovery failed: failed to fetch authorization server metadata"
        ));
    }

    #[test]
    fn test_is_auth_error_rejects_port_number_false_positive() {
        assert!(!is_auth_error(
            "connection refused while calling http://127.0.0.1:4018/callback"
        ));
    }

    /// Write credentials to file, read back, and verify.
    #[tokio::test]
    async fn test_file_store_round_trip() {
        use rmcp::transport::auth::StoredCredentials;

        let dir = std::env::temp_dir().join(format!("plug_oauth_test_{}", std::process::id()));
        // Override tokens_dir by using the store's internal file path method
        // indirectly — we test via a store with a simple server name and
        // verify the file-level round trip.

        let server_name = format!("test-server-{}", std::process::id());
        let store = CompositeCredentialStore::new(server_name.clone());

        let creds = StoredCredentials {
            client_id: "test-client-id".to_string(),
            token_response: None,
            granted_scopes: vec!["read".to_string(), "write".to_string()],
            token_received_at: Some(1234567890),
        };

        // Save to file (keyring may or may not work in CI).
        let save_result = store.file_save(&creds);
        assert!(save_result.is_ok(), "file_save failed: {save_result:?}");

        // Load back from file.
        let loaded = store.file_load();
        assert!(loaded.is_some(), "file_load returned None");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.client_id, "test-client-id");
        assert_eq!(loaded.granted_scopes, vec!["read", "write"]);
        assert_eq!(loaded.token_received_at, Some(1234567890));

        // Verify file permissions on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = store.token_file_path().unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "token file should have 0600 permissions");
        }

        // Clean up.
        store.file_clear();
        assert!(store.file_load().is_none());

        // Clean up directory.
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify that token file paths use sanitized server names.
    #[test]
    fn test_sanitize_integration() {
        // Valid name produces a path.
        let store = CompositeCredentialStore::new("my-server".to_string());
        let path = store.token_file_path().unwrap();
        assert!(path.ends_with("my-server.json"));
        assert!(path.starts_with(tokens_dir()));

        // Invalid name (path separator) returns an error.
        let store = CompositeCredentialStore::new("../evil".to_string());
        assert!(store.token_file_path().is_err());
    }

    // -- refresh_access_token tests ----------------------------------------

    /// Helper: build a synthetic token response for test fixtures.
    fn make_test_token(
        access: &str,
        refresh: Option<&str>,
        expires_in: Option<u64>,
    ) -> rmcp::transport::auth::StoredCredentials {
        use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
        use rmcp::transport::auth::VendorExtraTokenFields;

        let mut token =
            oauth2::StandardTokenResponse::<VendorExtraTokenFields, BasicTokenType>::new(
                AccessToken::new(access.to_string()),
                BasicTokenType::Bearer,
                VendorExtraTokenFields::default(),
            );
        if let Some(rt) = refresh {
            token.set_refresh_token(Some(RefreshToken::new(rt.to_string())));
        }
        if let Some(secs) = expires_in {
            token.set_expires_in(Some(&std::time::Duration::from_secs(secs)));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        StoredCredentials {
            client_id: "test-client".to_string(),
            token_response: Some(token),
            granted_scopes: vec![],
            token_received_at: Some(now),
        }
    }

    /// No stored credentials → NoCredentials.
    #[tokio::test]
    async fn test_refresh_no_credentials() {
        let name = format!("refresh-nocreds-{}", std::process::id());
        let result = refresh_access_token(&name, "http://localhost:9999", None).await;
        assert!(matches!(result, RefreshResult::NoCredentials));
    }

    /// Injected token (client_id == "injected") → InjectedToken.
    #[tokio::test]
    async fn test_refresh_injected_token() {
        let name = format!("refresh-injected-{}", std::process::id());
        let store = get_or_create_store(&name);

        let mut creds = make_test_token("test-access", Some("test-refresh"), Some(3600));
        creds.client_id = "injected".to_string();
        store.save(creds).await.unwrap();

        let result = refresh_access_token(&name, "http://localhost:9999", None).await;
        assert!(matches!(result, RefreshResult::InjectedToken));

        // Clean up
        store.clear().await.unwrap();
    }

    /// No refresh_token in stored credentials → NoRefreshToken.
    #[tokio::test]
    async fn test_refresh_no_refresh_token() {
        let name = format!("refresh-norefresh-{}", std::process::id());
        let store = get_or_create_store(&name);

        // Token without a refresh_token
        let creds = make_test_token("test-access", None, Some(3600));
        store.save(creds).await.unwrap();

        let result = refresh_access_token(&name, "http://localhost:9999", None).await;
        assert!(matches!(result, RefreshResult::NoRefreshToken));

        // Clean up
        store.clear().await.unwrap();
    }

    /// Valid credentials but unreachable server → TransientError (metadata
    /// discovery fails).
    #[tokio::test]
    async fn test_refresh_transient_error_unreachable_server() {
        crate::tls::ensure_rustls_provider_installed();
        let name = format!("refresh-transient-{}", std::process::id());
        let store = get_or_create_store(&name);

        let creds = make_test_token("test-access", Some("test-refresh"), Some(3600));
        store.save(creds).await.unwrap();

        // Point at a non-existent server — metadata discovery will fail.
        let result = refresh_access_token(&name, "http://127.0.0.1:1", None).await;
        assert!(
            matches!(result, RefreshResult::TransientError(_)),
            "expected TransientError, got {result:?}"
        );

        // Clean up
        store.clear().await.unwrap();
    }

    /// Persisted credentials should rehydrate the cache on first access after a
    /// cold start / `AuthRequired` state.
    #[tokio::test]
    async fn test_current_or_stored_access_token_hydrates_cache() {
        let name = format!("oauth-hydrate-{}", std::process::id());
        let store = get_or_create_store(&name);

        let creds = make_test_token("persisted-access", Some("persisted-refresh"), Some(3600));
        store.save(creds).await.unwrap();
        store.clear_cache();

        assert!(current_access_token(&name).is_none());

        let token = current_or_stored_access_token(&name).await;
        assert_eq!(token.as_deref(), Some("persisted-access"));
        assert_eq!(
            current_access_token(&name).as_deref(),
            Some("persisted-access")
        );

        store.clear().await.unwrap();
    }
}
