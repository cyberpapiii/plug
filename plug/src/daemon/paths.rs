//! Filesystem and socket path bootstrap for the daemon.
//!
//! Runtime/log directory resolution, the daemon's well-known file paths
//! (socket, PID file, auth token), and the 0700/0600 permission helpers
//! used when creating them.

use std::path::PathBuf;

use anyhow::Context as _;

/// Return the daemon runtime directory (for socket + PID file + auth token).
///
/// - macOS: `~/Library/Application Support/plug/`
/// - Linux: `$XDG_RUNTIME_DIR/plug/` (fallback: `~/.local/state/plug/`)
pub fn runtime_dir() -> PathBuf {
    #[cfg(test)]
    if let Some((runtime, _)) = test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned")
        .clone()
    {
        return runtime.join("plug");
    }

    #[cfg(target_os = "macos")]
    {
        dirs_path("Library/Application Support/plug")
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(dir).join("plug")
        } else {
            dirs_path(".local/state/plug")
        }
    }
}

/// Return the daemon log directory.
///
/// - macOS: `~/Library/Logs/plug/`
/// - Linux: `$XDG_STATE_HOME/plug/logs/` (fallback: `~/.local/state/plug/logs/`)
pub fn log_dir() -> PathBuf {
    #[cfg(test)]
    if let Some((_, state)) = test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned")
        .clone()
    {
        return state.join("plug/logs");
    }

    #[cfg(target_os = "macos")]
    {
        dirs_path("Library/Logs/plug")
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
            PathBuf::from(dir).join("plug/logs")
        } else {
            dirs_path(".local/state/plug/logs")
        }
    }
}

/// Expand `~/subpath` to an absolute path.
fn dirs_path(subpath: &str) -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(subpath))
        .unwrap_or_else(|| PathBuf::from(".").join(subpath))
}

#[cfg(test)]
fn test_runtime_paths() -> &'static std::sync::Mutex<Option<(PathBuf, PathBuf)>> {
    static TEST_RUNTIME_PATHS: std::sync::OnceLock<std::sync::Mutex<Option<(PathBuf, PathBuf)>>> =
        std::sync::OnceLock::new();
    TEST_RUNTIME_PATHS.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn set_test_runtime_paths(runtime: PathBuf, state: PathBuf) {
    *test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned") = Some((runtime, state));
}

#[cfg(test)]
pub(crate) fn clear_test_runtime_paths() {
    *test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned") = None;
}

/// Single process-wide lock serializing every test that reads or writes the
/// global `test_runtime_paths` slot (daemon, ipc_proxy, and runtime tests).
///
/// The runtime/state paths are a process global, so a test must hold this lock
/// for the whole window it has them set — otherwise a concurrently-running test
/// (under parallel threads) could clobber the slot or resolve `socket_path()`
/// to a foreign temp dir. Using ONE shared lock across all three test modules
/// (rather than a per-module lock) is what makes dropping `--test-threads=1`
/// safe: the ~15 path-touching tests serialize among themselves while the rest
/// of the suite runs in parallel.
///
/// Acquire from a `#[tokio::test]` with `.lock().await`, and from a plain
/// `#[test]` with `.blocking_lock()`. Never call `.blocking_lock()` from inside a
/// tokio runtime — it panics. The two forms interoperate correctly: a sync
/// `blocking_lock` holder and an async `.lock().await` waiter mutually exclude.
#[cfg(test)]
pub(crate) fn runtime_paths_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("plug.sock")
}

pub fn pid_path() -> PathBuf {
    runtime_dir().join("plug.pid")
}

pub fn token_path() -> PathBuf {
    runtime_dir().join("plug.token")
}

/// Create a directory with 0700 permissions, creating parents as needed.
/// On unix, uses DirBuilder to set mode atomically at creation time.
pub(super) fn ensure_dir(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }
    Ok(())
}

/// Set file permissions to 0600 (owner read/write only).
#[cfg(unix)]
pub(super) fn set_file_permissions_0600(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}
