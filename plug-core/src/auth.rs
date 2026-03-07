use std::path::{Path, PathBuf};

/// Generate a 256-bit (32-byte) cryptographic random auth token, hex-encoded.
pub fn generate_auth_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Constant-time comparison of auth tokens to prevent timing side-channel.
pub fn verify_auth_token(provided: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    // Lengths must match first (not timing-sensitive since token length is public)
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Load an existing auth token from a file, or generate and persist a new one.
///
/// If the file exists with correct permissions (0600 on Unix), its contents are reused.
/// Otherwise a fresh token is generated, written with 0600 permissions, and returned.
pub fn load_or_generate_token(path: &Path) -> anyhow::Result<String> {
    if path.exists() {
        // Check permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(path)?;
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                tracing::warn!(
                    path = %path.display(),
                    mode = format!("{mode:o}"),
                    "auth token file has incorrect permissions, fixing to 0600"
                );
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        let token = std::fs::read_to_string(path)?.trim().to_string();
        if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(token);
        }
        tracing::warn!(path = %path.display(), "auth token file has invalid content, regenerating");
    }

    let token = generate_auth_token();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(token.as_bytes())?;
    }

    #[cfg(not(unix))]
    std::fs::write(path, &token)?;

    Ok(token)
}

/// Return the path for an HTTP auth token file for a given port.
pub fn http_auth_token_path(port: u16) -> PathBuf {
    crate::config::config_dir().join(format!("http_auth_token_{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_is_64_hex_chars() {
        let token = generate_auth_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn verify_matching_tokens() {
        let token = generate_auth_token();
        assert!(verify_auth_token(&token, &token));
    }

    #[test]
    fn verify_mismatched_tokens() {
        let a = generate_auth_token();
        let b = generate_auth_token();
        assert!(!verify_auth_token(&a, &b));
    }

    #[test]
    fn verify_different_lengths() {
        assert!(!verify_auth_token("short", "longer_token"));
    }

    #[test]
    fn load_or_generate_creates_new_token() {
        let dir = std::env::temp_dir().join(format!("plug_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_token");

        let token = load_or_generate_token(&path).unwrap();
        assert_eq!(token.len(), 64);

        // Second call reuses the same token
        let token2 = load_or_generate_token(&path).unwrap();
        assert_eq!(token, token2);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
