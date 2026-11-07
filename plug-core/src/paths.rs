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
/// Under the home folder: the `plug` command Plug.app links to its binary.
pub const COMMAND_LINK: &str = ".local/bin/plug";

/// How long a cold start of the daemon may take before it is treated as
/// failed, by the CLI and by Plug.app alike. Starting every configured
/// upstream takes tens of seconds, and each one can involve a Keychain read
/// or a network round trip. Both drive one daemon, so a shorter wait on one
/// side kills what the other is still waiting for.
pub const STARTUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

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

/// `~/.local/bin/plug`, the command a shell and every local client run.
pub fn command_link() -> PathBuf {
    home().join(COMMAND_LINK)
}

/// `~/Library/Application Support/plug`: the config, the socket, the PID
/// file, the IPC token, and the state Plug keeps between runs.
pub fn dir() -> PathBuf {
    folder(DIR, "r/plug")
}

pub fn log_dir() -> PathBuf {
    folder(LOG_DIR, "s/plug/logs")
}

/// `shipped` under the home folder. A test build has no home folder: it puts
/// `in_test` under the test thread's own root (see [`test`]).
#[cfg(not(any(test, feature = "test-helpers")))]
fn folder(shipped: &str, _in_test: &str) -> PathBuf {
    home().join(shipped)
}

#[cfg(any(test, feature = "test-helpers"))]
fn folder(_shipped: &str, in_test: &str) -> PathBuf {
    test::root().join(in_test)
}

pub fn default_config_path() -> PathBuf {
    dir().join(CONFIG_FILE)
}

/// The daemon's socket. Only a development run may move it, with
/// `PLUG_SOCKET_PATH`. A test build ignores it, so a shell that exports it
/// cannot point every test at one socket.
pub fn socket_path() -> PathBuf {
    let value = if cfg!(any(test, feature = "test-helpers")) {
        None
    } else {
        std::env::var_os(SOCKET_ENV)
    };
    socket_override(is_development(), value).unwrap_or_else(|| dir().join(SOCKET_FILE))
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
    folder("Library/Caches/plug/artifacts", "r/plug/artifacts")
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

/// Where a test build keeps Plug's files: never under the home folder.
///
/// Each thread has its own root, so tests running in parallel never share
/// one. A thread's root is a fresh temporary folder, removed when the thread
/// ends, unless [`isolate`] names another. libtest runs each test on its own
/// thread, and a current-thread Tokio runtime polls every task on it.
/// Another thread, such as a `spawn_blocking` one or a worker of a
/// multi-thread runtime, has a root of its own, so a path a test shares with
/// such a thread is resolved before it is handed over.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test {
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The root a thread starts with. Nothing is created until a test writes
    /// there.
    struct Own(PathBuf);

    impl Drop for Own {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // The one place a test names the system temp folder: a test writes only
    // under a root made here, or in a `tempfile::TempDir`.
    #[allow(clippy::disallowed_methods)]
    fn fresh() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "plug-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    thread_local! {
        static OWN: Own = Own(fresh());
        static CHOSEN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn root() -> PathBuf {
        CHOSEN
            .with_borrow(Clone::clone)
            .or_else(|| OWN.try_with(|own| own.0.clone()).ok())
            .unwrap_or_else(fresh)
    }

    /// A new empty folder for a test, under this thread's root, so it goes
    /// when the thread ends, also when the test panics. Each call gets a
    /// folder of its own; `label` only keeps the name readable.
    pub fn scratch(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = root().join(format!("{label}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).expect("create a test folder");
        dir
    }

    /// The root chosen with [`isolate`], until this drops, which it also
    /// does when the test panics. It belongs to the thread that made it.
    #[must_use = "the root is restored when this guard drops"]
    pub struct Isolated {
        previous: Option<PathBuf>,
        _thread: PhantomData<*const ()>,
    }

    impl Drop for Isolated {
        fn drop(&mut self) {
            CHOSEN.set(self.previous.take());
        }
    }

    /// Put this thread's Plug folders under `root`, for a test that wants to
    /// know where they are. The folder names stay short: a Unix socket path
    /// has to fit in SUN_LEN, and the macOS per-user temp directory nearly
    /// fills it.
    pub fn isolate(root: &Path) -> Isolated {
        Isolated {
            previous: CHOSEN.replace(Some(root.to_path_buf())),
            _thread: PhantomData,
        }
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
        command_link: String,
        startup_budget_secs: u64,
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
        assert_eq!(fixture.command_link, COMMAND_LINK);
        assert_eq!(fixture.startup_budget_secs, STARTUP_BUDGET.as_secs());
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

    #[test]
    fn a_test_thread_keeps_its_own_files_and_they_go_when_it_ends() {
        let home = dirs::home_dir().expect("home folder");
        let ours = [dir(), log_dir(), artifacts_dir(), socket_path(), pid_path()];
        assert!(ours.iter().all(|path| !path.starts_with(&home)), "{ours:?}");

        let theirs = std::thread::spawn(|| {
            let theirs = dir();
            std::fs::create_dir_all(&theirs).unwrap();
            theirs
        })
        .join()
        .unwrap();
        assert_ne!(theirs, dir());
        assert!(!theirs.exists(), "{theirs:?} outlived its thread");
    }

    #[test]
    fn isolate_moves_this_thread_only_until_it_drops() {
        let own = dir();
        let root = test::scratch("isolate-check");
        {
            let _paths = test::isolate(&root);
            assert_eq!(dir(), root.join("r/plug"));
            assert_eq!(log_dir(), root.join("s/plug/logs"));
            assert_ne!(std::thread::spawn(dir).join().unwrap(), dir());
        }
        assert_eq!(dir(), own);
    }
}
