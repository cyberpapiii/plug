//! The PATH stdio servers run with. On macOS the service starts with
//! launchd's bare PATH, so Plug asks the user's login shell for theirs.

use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::future::Future;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::time::Duration;

/// Whether a stdio `command` is a name looked up in PATH. Anything with more
/// than one path component runs as written, spaces and all.
pub(crate) fn is_bare_name(command: &str) -> bool {
    Path::new(command).components().count() <= 1
}

/// Resolve a bare stdio command through PATH, then through `login_path`, the
/// PATH [`stdio_path`] gives stdio servers.
pub(crate) fn resolve_stdio_command(command: &str, login_path: Option<&OsStr>) -> PathBuf {
    let configured = PathBuf::from(command);
    if !is_bare_name(command) {
        return configured;
    }

    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(command))
            .find(|path| path.is_file())
    }) {
        return path;
    }

    // SMAppService launch agents intentionally receive launchd's minimal
    // PATH. Resolve through the user's login PATH, which is also inherited
    // by the child below so script launchers such as `#!/usr/bin/env node`
    // can find their own interpreter.
    if let Some(path) = login_path.and_then(|paths| {
        std::env::split_paths(paths)
            .map(|dir| dir.join(command))
            .find(|path| path.is_file())
    }) {
        return path;
    }

    configured
}

/// The PATH for stdio servers. See [`stdio_path`].
pub(crate) async fn stdio_login_path() -> Option<&'static OsStr> {
    stdio_path().await.0
}

/// Where the PATH handed to stdio servers came from, for error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StdioPathSource {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    LoginShell,
    /// The login-shell probe failed; see [`fallback_stdio_path`].
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Fallback,
    /// Not macOS: the daemon's own PATH.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Inherited,
}

impl StdioPathSource {
    fn describe(self) -> &'static str {
        match self {
            Self::LoginShell => "the login shell PATH",
            Self::Fallback => "the fallback PATH (the login shell PATH probe failed)",
            Self::Inherited => "PATH",
        }
    }
}

/// The PATH for stdio servers and where it came from: the user's login-shell
/// PATH on macOS, `None` elsewhere.
///
/// A successful probe is cached for the life of the process and saved next to
/// the config. A failed one is not cached: after a reboot the login shell can
/// take longer than the probe allows, and caching that failure left every Node
/// server unable to find `node` until the daemon restarted. Until a probe
/// succeeds, callers get the PATH an earlier probe saved, so a boot does not
/// wait on the shell, or else the inherited PATH plus the standard Homebrew
/// directories.
pub(super) async fn stdio_path() -> (Option<&'static OsStr>, StdioPathSource) {
    #[cfg(target_os = "macos")]
    {
        static STDIO_PATH: StdioPath = StdioPath::new();
        let (probe, store) = login_shell_probe();
        STDIO_PATH
            .resolve(LOGIN_SHELL_PROBE_RETRY, store, probe)
            .await
    }
    #[cfg(not(target_os = "macos"))]
    (None, StdioPathSource::Inherited)
}

/// The probe [`stdio_path`] runs, and the file its result is saved to.
///
/// An interactive login shell runs the user's dotfiles, which can hang, so a
/// stuck shell delays the first stdio start by at most five seconds.
#[cfg(all(target_os = "macos", not(any(test, feature = "test-helpers"))))]
fn login_shell_probe() -> (
    impl Future<Output = Option<OsString>> + Send + 'static,
    Option<PathBuf>,
) {
    (
        probe_login_shell_path(
            "/bin/zsh",
            &["-lic", "printf '__PLUG_STDIO_PATH__%s\\n' \"$PATH\""],
            Duration::from_secs(5),
        ),
        Some(crate::paths::dir().join(SAVED_LOGIN_PATH_FILE)),
    )
}

/// Tests answer with the test runner's PATH and save nothing: a real login
/// shell runs the developer's dotfiles and would write the live daemon's saved
/// PATH, and its wait can outlive a paused Tokio clock and use up a test's
/// virtual retry window.
#[cfg(all(target_os = "macos", any(test, feature = "test-helpers")))]
fn login_shell_probe() -> (
    impl Future<Output = Option<OsString>> + Send + 'static,
    Option<PathBuf>,
) {
    (async { std::env::var_os("PATH") }, None)
}

/// The PATH launchd gives an agent. The daemon runs with this when Plug.app
/// starts it.
#[cfg(target_os = "macos")]
const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Resolve a bare `command` the way the daemon does when its login-shell
/// probe fails: launchd's PATH plus Homebrew. `None` if it is not found there,
/// and always `None` off macOS, where there is no such fallback.
pub(crate) fn resolve_without_login_shell(command: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let path = fallback_stdio_path(Some(OsString::from(LAUNCHD_PATH)));
        std::env::split_paths(&path)
            .map(|dir| dir.join(command))
            .find(|path| path.is_file())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        None
    }
}

/// A spawn failure that names the command and, when it was not found, the
/// PATH that was searched. Never includes args or env, which can hold secrets.
pub(super) fn spawn_error(
    command: &str,
    error: &std::io::Error,
    source: StdioPathSource,
) -> anyhow::Error {
    let bare = Path::new(command).components().count() == 1;
    if error.kind() == std::io::ErrorKind::NotFound && bare {
        anyhow::anyhow!(
            "failed to spawn `{command}`: not found on {}",
            source.describe()
        )
    } else {
        anyhow::anyhow!("failed to spawn `{command}`: {error}")
    }
}

/// The login-shell PATH cache, and the fallback used until a probe succeeds.
#[cfg(target_os = "macos")]
struct StdioPath {
    cache: LoginPathCache,
    fallback: std::sync::OnceLock<OsString>,
}

#[cfg(target_os = "macos")]
impl StdioPath {
    const fn new() -> Self {
        Self {
            cache: LoginPathCache::new(),
            fallback: std::sync::OnceLock::new(),
        }
    }

    async fn resolve(
        &'static self,
        retry_after: Duration,
        store: Option<PathBuf>,
        probe: impl Future<Output = Option<OsString>> + Send + 'static,
    ) -> (Option<&'static OsStr>, StdioPathSource) {
        match cached_login_path(&self.cache, retry_after, store, probe).await {
            Some(path) => (Some(path), StdioPathSource::LoginShell),
            None => (
                Some(
                    self.fallback
                        .get_or_init(|| fallback_stdio_path(std::env::var_os("PATH")))
                        .as_os_str(),
                ),
                StdioPathSource::Fallback,
            ),
        }
    }
}

#[cfg(target_os = "macos")]
struct LoginPathCache {
    path: std::sync::OnceLock<OsString>,
    /// The PATH an earlier successful probe saved to disk, read once.
    saved: std::sync::OnceLock<Option<OsString>>,
    /// When the last probe failed. The lock also keeps concurrent server
    /// starts from probing at once.
    last_failure: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

#[cfg(target_os = "macos")]
impl LoginPathCache {
    const fn new() -> Self {
        Self {
            path: std::sync::OnceLock::new(),
            saved: std::sync::OnceLock::new(),
            last_failure: tokio::sync::Mutex::const_new(None),
        }
    }
}

/// The cached login PATH, probing when there is none and the last failure
/// is older than `retry_after`. The probe runs on a spawned task, so a caller
/// cancelled mid-probe (a start timeout shorter than the probe's) cannot
/// leave it half done and make the next start pay for it again.
///
/// When `store` holds a PATH from an earlier successful probe, callers get it
/// at once and the probe refreshes the cache and the file in the background.
/// Right after a reboot the probe can take its full timeout, and every stdio
/// server would otherwise wait on it.
#[cfg(target_os = "macos")]
async fn cached_login_path(
    cache: &'static LoginPathCache,
    retry_after: Duration,
    store: Option<PathBuf>,
    probe: impl Future<Output = Option<OsString>> + Send + 'static,
) -> Option<&'static OsStr> {
    if let Some(path) = cache.path.get() {
        return Some(path);
    }
    let saved = cache
        .saved
        .get_or_init(|| store.as_deref().and_then(read_saved_login_path));
    let refresh = tokio::spawn(async move {
        let mut last_failure = cache.last_failure.lock().await;
        if cache.path.get().is_some() || last_failure.is_some_and(|at| at.elapsed() < retry_after) {
            return;
        }
        match probe.await {
            Some(path) => {
                if let Some(store) = &store {
                    save_login_path(store, &path);
                }
                let _ = cache.path.set(path);
            }
            None => *last_failure = Some(tokio::time::Instant::now()),
        }
    });
    if let Some(saved) = saved {
        return Some(saved);
    }
    let _ = refresh.await;
    cache.path.get().map(OsString::as_os_str)
}

/// The file in the config directory that holds the last successful probe's
/// PATH.
#[cfg(all(target_os = "macos", any(test, not(feature = "test-helpers"))))]
const SAVED_LOGIN_PATH_FILE: &str = "login-shell-path";

/// A saved PATH, or `None` when the file is missing, not UTF-8, or names no
/// absolute directory.
#[cfg(target_os = "macos")]
fn read_saved_login_path(store: &Path) -> Option<OsString> {
    let contents = std::fs::read_to_string(store).ok()?;
    let path = contents.trim();
    std::env::split_paths(path)
        .any(|dir| dir.is_absolute())
        .then(|| OsString::from(path))
}

/// Save the PATH, readable only by the user. A failure costs the next boot
/// the probe's wait and nothing else, so it is only logged.
#[cfg(target_os = "macos")]
fn save_login_path(store: &Path, path: &OsStr) {
    use std::os::unix::ffi::OsStrExt;

    if let Err(error) = crate::fs_perm::write_private_atomic(store, path.as_bytes()) {
        tracing::debug!(%error, path = %store.display(), "could not save the login shell PATH");
    }
}

/// The inherited PATH plus the Homebrew directories where `node`, `npx`, and
/// `uvx` usually live. launchd gives the daemon only the system directories.
#[cfg(target_os = "macos")]
fn fallback_stdio_path(inherited: Option<OsString>) -> OsString {
    let mut dirs: Vec<PathBuf> = inherited
        .as_deref()
        .map(|paths| std::env::split_paths(paths).collect())
        .unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if !dirs.iter().any(|dir| dir == Path::new(extra)) {
            dirs.push(PathBuf::from(extra));
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// A failed probe is retried on the next stdio start after this long.
#[cfg(target_os = "macos")]
const LOGIN_SHELL_PROBE_RETRY: Duration = Duration::from_secs(30);

#[cfg(all(target_os = "macos", any(test, not(feature = "test-helpers"))))]
async fn probe_login_shell_path(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Option<OsString> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(timeout, output).await {
        Ok(Ok(output)) if output.status.success() => login_shell_path(&output.stdout),
        Ok(_) => None,
        Err(_) => {
            tracing::warn!(
                "login shell PATH probe timed out after {timeout:?}; using the saved or fallback PATH"
            );
            None
        }
    }
}

#[cfg(all(target_os = "macos", any(test, not(feature = "test-helpers"))))]
fn login_shell_path(stdout: &[u8]) -> Option<OsString> {
    const MARKER: &str = "__PLUG_STDIO_PATH__";
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(MARKER))
        .find(|path| !path.is_empty())
        .map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn login_shell_probe_gives_up_on_a_hung_shell() {
        let started = std::time::Instant::now();
        let path =
            probe_login_shell_path("/bin/sh", &["-c", "sleep 30"], Duration::from_millis(200))
                .await;
        assert_eq!(path, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn login_shell_probe_reads_marked_path() {
        let path = probe_login_shell_path(
            "/bin/sh",
            &["-c", "echo noise; echo __PLUG_STDIO_PATH__/bin:/opt/bin"],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(path, Some(OsString::from("/bin:/opt/bin")));
    }

    /// A probe that answers `path` once the returned sender fires.
    #[cfg(target_os = "macos")]
    fn gated_probe(
        path: &'static str,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        impl Future<Output = Option<OsString>> + Send + 'static,
    ) {
        let (release, released) = tokio::sync::oneshot::channel();
        (release, async move {
            released.await.ok().map(|()| OsString::from(path))
        })
    }

    /// Yield until `cache` holds a PATH, failing after a virtual second.
    #[cfg(target_os = "macos")]
    async fn wait_for_cached_path(cache: &LoginPathCache) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while cache.path.get().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the probe should fill the cache");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn login_shell_probe_survives_a_cancelled_caller() {
        static CACHE: LoginPathCache = LoginPathCache::new();
        let (release, probe) = gated_probe("/bin");

        let cancelled = tokio::time::timeout(
            Duration::from_millis(50),
            cached_login_path(&CACHE, Duration::from_secs(30), None, probe),
        )
        .await;
        assert!(cancelled.is_err(), "caller should time out mid-probe");

        release.send(()).unwrap();
        wait_for_cached_path(&CACHE).await;
        assert_eq!(
            cached_login_path(&CACHE, Duration::from_secs(30), None, async { None }).await,
            Some(OsStr::new("/bin")),
            "the probe must finish and fill the cache after its caller is gone"
        );
    }

    /// After a reboot the login shell can miss the probe's deadline. That
    /// failure must not stick for the life of the daemon.
    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn a_failed_login_shell_probe_is_retried_later() {
        static CACHE: LoginPathCache = LoginPathCache::new();
        let retry = Duration::from_secs(30);
        let failing = || async { None };
        let working = || async { Some(OsString::from("/opt/homebrew/bin")) };

        assert_eq!(
            cached_login_path(&CACHE, retry, None, failing()).await,
            None
        );
        assert_eq!(
            cached_login_path(&CACHE, retry, None, working()).await,
            None,
            "a probe inside the retry window is skipped"
        );
        tokio::time::advance(retry).await;
        assert_eq!(
            cached_login_path(&CACHE, retry, None, working()).await,
            Some(OsStr::new("/opt/homebrew/bin"))
        );
        assert_eq!(
            cached_login_path(&CACHE, retry, None, failing()).await,
            Some(OsStr::new("/opt/homebrew/bin")),
            "a successful probe is kept"
        );
    }

    /// Right after a reboot the probe can take its full timeout. A PATH saved
    /// by an earlier probe must be used at once, not after the probe.
    #[cfg(target_os = "macos")]
    #[tokio::test(start_paused = true)]
    async fn a_saved_login_path_is_used_without_waiting_for_the_probe() {
        static CACHE: LoginPathCache = LoginPathCache::new();
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join(SAVED_LOGIN_PATH_FILE);
        std::fs::write(&store, "/saved/bin:/usr/bin\n").unwrap();
        let (release, probe) = gated_probe("/probed/bin");

        assert_eq!(
            cached_login_path(&CACHE, Duration::from_secs(30), Some(store.clone()), probe).await,
            Some(OsStr::new("/saved/bin:/usr/bin")),
            "the saved PATH is answered while the probe is still running"
        );

        release.send(()).unwrap();
        wait_for_cached_path(&CACHE).await;
        assert_eq!(
            cached_login_path(
                &CACHE,
                Duration::from_secs(30),
                Some(store.clone()),
                async { None }
            )
            .await,
            Some(OsStr::new("/probed/bin")),
            "the background probe replaces the saved PATH"
        );
        assert_eq!(std::fs::read_to_string(&store).unwrap(), "/probed/bin");
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    /// Until a probe succeeds, stdio servers get the inherited PATH plus
    /// Homebrew, and errors say so.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_failed_probe_falls_back_to_homebrew() {
        static STDIO_PATH: StdioPath = StdioPath::new();
        let (path, source) = STDIO_PATH
            .resolve(Duration::from_secs(30), None, async { None })
            .await;
        assert_eq!(source, StdioPathSource::Fallback);
        let path = path.expect("fallback PATH");
        assert!(std::env::split_paths(path).any(|dir| dir == Path::new("/opt/homebrew/bin")));

        let (path, source) = STDIO_PATH
            .resolve(Duration::ZERO, None, async {
                Some(OsString::from("/login/bin"))
            })
            .await;
        assert_eq!(source, StdioPathSource::LoginShell);
        assert_eq!(path, Some(OsStr::new("/login/bin")));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_successful_probe_is_saved_for_the_next_boot() {
        static CACHE: LoginPathCache = LoginPathCache::new();
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join(SAVED_LOGIN_PATH_FILE);

        assert_eq!(
            cached_login_path(
                &CACHE,
                Duration::from_secs(30),
                Some(store.clone()),
                async { Some(OsString::from("/probed/bin")) }
            )
            .await,
            Some(OsStr::new("/probed/bin"))
        );
        assert_eq!(
            read_saved_login_path(&store),
            Some(OsString::from("/probed/bin"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unusable_saved_login_path_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join(SAVED_LOGIN_PATH_FILE);
        assert_eq!(read_saved_login_path(&store), None, "missing");
        std::fs::write(&store, "").unwrap();
        assert_eq!(read_saved_login_path(&store), None, "empty");
        std::fs::write(&store, " \n").unwrap();
        assert_eq!(read_saved_login_path(&store), None, "blank");
        std::fs::write(&store, "relative/bin").unwrap();
        assert_eq!(read_saved_login_path(&store), None, "no absolute dir");
        std::fs::write(&store, [0xff, 0xfe, b'/']).unwrap();
        assert_eq!(read_saved_login_path(&store), None, "not UTF-8");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fallback_stdio_path_adds_homebrew_once() {
        assert_eq!(
            fallback_stdio_path(Some(OsString::from("/usr/bin:/bin"))),
            OsString::from("/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
        );
        assert_eq!(
            fallback_stdio_path(Some(OsString::from("/opt/homebrew/bin:/usr/bin"))),
            OsString::from("/opt/homebrew/bin:/usr/bin:/usr/local/bin")
        );
        assert_eq!(
            fallback_stdio_path(None),
            OsString::from("/opt/homebrew/bin:/usr/local/bin")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn login_shell_path_ignores_shell_noise() {
        assert_eq!(
            login_shell_path(b"welcome from shell config\n__PLUG_STDIO_PATH__/bin:/opt/bin\n"),
            Some(OsString::from("/bin:/opt/bin"))
        );
        assert_eq!(login_shell_path(b"/bin:/opt/bin\n"), None);
    }

    #[test]
    fn spawn_error_names_the_command_and_the_path_searched() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            spawn_error("npx", &not_found, StdioPathSource::Fallback).to_string(),
            "failed to spawn `npx`: not found on the fallback PATH (the login shell PATH probe failed)"
        );
        assert_eq!(
            spawn_error("/opt/tool", &not_found, StdioPathSource::LoginShell).to_string(),
            format!("failed to spawn `/opt/tool`: {not_found}")
        );
    }
}
