use std::sync::OnceLock;

/// Ensure the process-wide rustls crypto provider is installed.
///
/// This is safe to call repeatedly. The first caller installs ring-backed rustls;
/// later callers become no-ops.
pub fn ensure_rustls_provider_installed() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
