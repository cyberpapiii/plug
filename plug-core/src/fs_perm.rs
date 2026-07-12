//! Filesystem permission helpers for secret-bearing directories.

use std::path::Path;

/// Create `path` (and any missing parents) with mode 0700 on Unix.
///
/// Only directories this call actually creates get the restrictive mode
/// (`DirBuilder` semantics) — directories that already exist are left
/// untouched, so deliberately customized permissions are preserved.
/// On non-Unix platforms this is a plain recursive create.
pub fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_nested_dir_with_0700_and_is_idempotent() {
        let base = std::env::temp_dir().join(format!(
            "plug-fs-perm-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let leaf = base.join("nested").join("secrets");

        ensure_dir_0700(&leaf).expect("first create succeeds");
        assert!(leaf.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&leaf).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "leaf dir must be created 0700");
        }

        // A second call on the already-existing dir is an Ok no-op.
        ensure_dir_0700(&leaf).expect("second create is Ok");

        let _ = std::fs::remove_dir_all(&base);
    }
}
