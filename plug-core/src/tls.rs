use std::sync::OnceLock;

use rmcp::transport::auth::{AuthError, AuthorizationManager};

/// Ensure the process-wide rustls crypto provider is installed.
///
/// Building any TLS-capable client or server config panics without one. HTTP
/// clients get it through [`http_client_builder`] and [`authorization_manager`];
/// call this directly only for a rustls server config.
pub fn ensure_rustls_provider_installed() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The start of every reqwest client Plug builds.
#[allow(clippy::disallowed_methods)]
pub fn http_client_builder() -> reqwest::ClientBuilder {
    ensure_rustls_provider_installed();
    reqwest::Client::builder()
}

/// rmcp's OAuth manager for `url`, sending its requests through Plug's
/// upstream clients so they follow the same redirect rules.
pub async fn authorization_manager<U: reqwest::IntoUrl>(
    url: U,
) -> Result<AuthorizationManager, AuthError> {
    let client = crate::upstream_http::oauth_client()
        .map_err(|error| AuthError::InternalError(error.to_string()))?;
    AuthorizationManager::new_with_oauth_http_client(url, client).await
}
