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

/// rmcp's OAuth manager for `url`. It builds its own reqwest client inside.
#[allow(clippy::disallowed_methods)]
pub async fn authorization_manager<U: reqwest::IntoUrl>(
    url: U,
) -> Result<AuthorizationManager, AuthError> {
    ensure_rustls_provider_installed();
    AuthorizationManager::new(url).await
}
