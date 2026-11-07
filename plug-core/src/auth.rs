use std::path::{Path, PathBuf};

/// Generate a 256-bit (32-byte) cryptographic random auth token, hex-encoded.
pub fn generate_auth_token() -> String {
    use rand::TryRng as _;
    let mut bytes = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .expect("operating system random source unavailable");
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

/// Load the auth token kept at `path`, or generate one and keep it there.
///
/// A token already there is reused, and its file made owner-only if it was
/// not. A new one replaces the file only when there was none or it held
/// something that is not a token. Any other failure to read it is returned:
/// replacing the token would disconnect every client that holds it.
pub fn load_or_generate_token(path: &Path) -> anyhow::Result<String> {
    if let Some(token) = load_token(path)? {
        return Ok(token);
    }
    let token = generate_auth_token();
    write_token_file(path, &token)?;
    Ok(token)
}

fn load_token(path: &Path) -> anyhow::Result<Option<String>> {
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode != 0o600 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{mode:o}"),
            "auth token file has incorrect permissions, fixing to 0600"
        );
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut contents = String::new();
    match file.read_to_string(&mut contents) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {}
        Err(error) => return Err(error.into()),
    }
    let token = contents.trim();
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(token.to_string()))
    } else {
        tracing::warn!(path = %path.display(), "auth token file has invalid content, regenerating");
        Ok(None)
    }
}

/// Keep `token` at `path`, readable by its owner only.
pub fn write_token_file(path: &Path, token: &str) -> anyhow::Result<()> {
    crate::fs_perm::write_private_atomic(path, token.as_bytes())?;
    Ok(())
}

/// Return the path for an HTTP auth token file for a given port.
pub fn http_auth_token_path(port: u16) -> PathBuf {
    crate::paths::dir().join(format!("http_auth_token_{port}"))
}

/// Return the path for the local operator inventory token for a given port.
pub fn http_operator_token_path(port: u16) -> PathBuf {
    crate::paths::dir().join(format!("http_operator_token_{port}"))
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
    fn a_token_that_cannot_be_read_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::paths::test::scratch("auth-unreadable");
        let path = dir.join("token");
        let token = generate_auth_token();
        write_token_file(&path, &token).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // Root reads through permissions, so there is no read error to test.
            return;
        }

        let loaded = load_or_generate_token(&path);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            loaded.is_err(),
            "an unreadable token is an error, not a reason to replace it"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
    }

    #[test]
    fn load_or_generate_creates_new_token() {
        let path = crate::paths::test::scratch("auth-token").join("test_token");

        let token = load_or_generate_token(&path).unwrap();
        assert_eq!(token.len(), 64);

        // Second call reuses the same token
        let token2 = load_or_generate_token(&path).unwrap();
        assert_eq!(token, token2);
    }
}
