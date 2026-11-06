//! Where Plug keeps its files on this Mac, and the names Plug.app, the CLI
//! and the daemon must agree on. The app reads the same names from
//! `testdata/paths.json`, which a test here holds to these values.
//!
//! The config, the socket, and everything Plug keeps between runs share one
//! folder, `~/Library/Application Support/plug`.

use std::path::{Path, PathBuf};

/// The folder under the home folder that holds the config and the socket.
pub const DIR: &str = "Library/Application Support/plug";
/// The folder under the home folder that holds the logs.
pub const LOG_DIR: &str = "Library/Logs/plug";
/// Each day's log is `plug.log.YYYY-MM-DD`.
pub const LOG_FILE_PREFIX: &str = "plug.log";
pub const CONFIG_FILE: &str = "config.toml";
pub const SOCKET_FILE: &str = "plug.sock";
pub const PID_FILE: &str = "plug.pid";
/// The daemon's IPC token, readable by this user only.
pub const TOKEN_FILE: &str = "plug.token";
/// Beside the socket: the owner turned Plug off, so nothing starts it.
pub const SERVICE_DISABLED_MARKER: &str = "service-disabled";
/// Beside the socket: the owner hid the menu bar icon, so a closed app is
/// not reopened.
pub const MENU_BAR_HIDDEN_MARKER: &str = "menu-bar-hidden";

/// The launchd job Plug.app registers for the daemon.
pub const DAEMON_LABEL: &str = "com.plug.daemon";
pub const APP_BUNDLE_ID: &str = "com.cyberpapiii.plug";
/// The Developer ID team that signs Plug.app.
pub const TEAM_ID: &str = "HJF7LN64XX";
/// Where in Plug.app the `plug` binary is.
pub const BUNDLED_BINARY: &str = "Contents/Resources/plug";
/// Where in Plug.app the app's own executable is.
pub const APP_EXECUTABLE: &str = "Contents/MacOS/Plug";
/// What launchd runs the bundled binary with.
pub const DAEMON_ARGS: [&str; 2] = ["serve", "--daemon"];
/// The Keychain service every item Plug stores is under.
pub const KEYCHAIN_SERVICE: &str = "plug";

/// `PLUG_DEV=1` marks a source-development run: it may use another socket
/// and need not be the copy inside Plug.app.
pub const DEV_ENV: &str = "PLUG_DEV";
/// The socket a development run uses in place of the usual one.
pub const SOCKET_ENV: &str = "PLUG_SOCKET_PATH";

pub fn is_development() -> bool {
    std::env::var_os(DEV_ENV).is_some_and(|value| value == "1")
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/Library/Application Support/plug`: the config, the socket, the PID
/// file, the IPC token, and the state Plug keeps between runs.
pub fn dir() -> PathBuf {
    #[cfg(any(test, feature = "test-helpers"))]
    if let Some((dir, _)) = test::paths() {
        return dir;
    }
    home().join(DIR)
}

pub fn log_dir() -> PathBuf {
    #[cfg(any(test, feature = "test-helpers"))]
    if let Some((_, logs)) = test::paths() {
        return logs;
    }
    home().join(LOG_DIR)
}

pub fn default_config_path() -> PathBuf {
    dir().join(CONFIG_FILE)
}

/// The daemon's socket. Only a development run may move it, with
/// `PLUG_SOCKET_PATH`.
pub fn socket_path() -> PathBuf {
    socket_override(is_development(), std::env::var_os(SOCKET_ENV))
        .unwrap_or_else(|| dir().join(SOCKET_FILE))
}

fn socket_override(development: bool, value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value
        .filter(|path| development && !path.is_empty())
        .map(PathBuf::from)
}

pub fn pid_path() -> PathBuf {
    dir().join(PID_FILE)
}

pub fn token_path() -> PathBuf {
    dir().join(TOKEN_FILE)
}

/// A marker file `name`, which sits beside the socket.
pub fn marker(name: &str) -> PathBuf {
    socket_path().with_file_name(name)
}

/// Upstream OAuth sign-ins, one file a server.
pub fn tokens_dir() -> PathBuf {
    dir().join("tokens")
}

/// Downstream OAuth: clients, grants, and the owner's passkeys.
pub fn downstream_oauth_dir() -> PathBuf {
    dir().join("downstream_oauth")
}

/// Event subscriptions, one folder a source.
pub fn events_dir() -> PathBuf {
    dir().join("events")
}

/// Slack event state, one folder a source config. It predates
/// `events_dir`, and existing installs keep their state here.
pub fn slack_events_dir() -> PathBuf {
    dir().join("slack-events")
}

/// Large tool results kept for a client to read in parts.
pub fn artifacts_dir() -> PathBuf {
    home().join("Library/Caches/plug/artifacts")
}

/// The bundled `plug` binary of the app at `bundle`.
pub fn bundled_binary(bundle: &Path) -> PathBuf {
    bundle.join(BUNDLED_BINARY)
}

/// The app bundle `binary` is the bundled `plug` of, if it is one.
pub fn app_bundle_of(binary: &Path) -> Option<PathBuf> {
    let resources = binary.parent()?;
    let contents = resources.parent()?;
    let bundle = contents.parent()?;
    let inside = binary.file_name()? == "plug"
        && resources.file_name()? == "Resources"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    inside.then(|| bundle.to_path_buf())
}

/// A process-wide stand-in for the home folders, for tests that run a
/// daemon.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock, PoisonError};

    fn slot() -> &'static Mutex<Option<(PathBuf, PathBuf)>> {
        static SLOT: OnceLock<Mutex<Option<(PathBuf, PathBuf)>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn paths() -> Option<(PathBuf, PathBuf)> {
        slot()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn active() -> bool {
        paths().is_some()
    }

    /// Put Plug's folder under `runtime` and its logs under `state`.
    pub fn set(runtime: PathBuf, state: PathBuf) {
        *slot().lock().unwrap_or_else(PoisonError::into_inner) =
            Some((runtime.join("plug"), state.join("plug/logs")));
    }

    pub fn clear() {
        *slot().lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Held by every test that sets the paths, for as long as they are set,
    /// so tests running in parallel do not see each other's. Take it with
    /// `.lock().await` in a tokio test and `.blocking_lock()` in a plain one.
    pub fn lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        dir: String,
        log_dir: String,
        log_file_prefix: String,
        config: String,
        socket: String,
        pid: String,
        token: String,
        service_disabled_marker: String,
        menu_bar_hidden_marker: String,
        daemon_label: String,
        app_bundle_id: String,
        team_id: String,
        bundled_binary: String,
        app_executable: String,
        daemon_args: Vec<String>,
        keychain_service: String,
    }

    #[test]
    fn the_app_reads_the_same_names() {
        let fixture: Fixture = serde_json::from_str(include_str!("../../testdata/paths.json"))
            .expect("testdata/paths.json");
        assert_eq!(fixture.dir, DIR);
        assert_eq!(fixture.log_dir, LOG_DIR);
        assert_eq!(fixture.log_file_prefix, LOG_FILE_PREFIX);
        assert_eq!(fixture.config, CONFIG_FILE);
        assert_eq!(fixture.socket, SOCKET_FILE);
        assert_eq!(fixture.pid, PID_FILE);
        assert_eq!(fixture.token, TOKEN_FILE);
        assert_eq!(fixture.service_disabled_marker, SERVICE_DISABLED_MARKER);
        assert_eq!(fixture.menu_bar_hidden_marker, MENU_BAR_HIDDEN_MARKER);
        assert_eq!(fixture.daemon_label, DAEMON_LABEL);
        assert_eq!(fixture.app_bundle_id, APP_BUNDLE_ID);
        assert_eq!(fixture.team_id, TEAM_ID);
        assert_eq!(fixture.bundled_binary, BUNDLED_BINARY);
        assert_eq!(fixture.app_executable, APP_EXECUTABLE);
        assert_eq!(fixture.daemon_args, DAEMON_ARGS);
        assert_eq!(fixture.keychain_service, KEYCHAIN_SERVICE);
    }

    #[test]
    fn only_a_development_run_moves_the_socket() {
        let elsewhere = std::ffi::OsString::from("/tmp/plug-elsewhere.sock");
        assert_eq!(socket_override(false, Some(elsewhere.clone())), None);
        assert_eq!(
            socket_override(true, Some(elsewhere)),
            Some(PathBuf::from("/tmp/plug-elsewhere.sock"))
        );
        assert_eq!(socket_override(true, Some("".into())), None);
    }

    #[test]
    fn the_bundle_is_found_only_from_its_own_binary() {
        let bundle = Path::new("/Applications/Plug.app");
        assert_eq!(
            app_bundle_of(&bundled_binary(bundle)).as_deref(),
            Some(bundle)
        );
        assert_eq!(app_bundle_of(Path::new("/usr/local/bin/plug")), None);
        assert_eq!(
            app_bundle_of(Path::new("/opt/Contents/Resources/plug")),
            None
        );
        assert_eq!(
            app_bundle_of(Path::new("/Applications/Plug.app/Contents/Resources/other")),
            None
        );
    }
}
