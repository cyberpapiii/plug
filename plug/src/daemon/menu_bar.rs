//! Brings Plug's menu bar icon back when something other than its owner
//! closed the app.
//!
//! The daemon outlives `Plug.app`, so after a quit-all or a crash it keeps
//! serving with nothing on screen to say so. While the daemon runs from inside
//! the app bundle it watches for the app, and once the app has been gone for
//! two looks in a row, it opens it in the background. That covers login too.
//! The app writes `menu-bar-hidden` beside the socket when its owner chose to
//! quit it or turned this off, and the daemon then leaves it closed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

const LOOK_EVERY: Duration = Duration::from_secs(20);
/// Two looks, so an update that quits the app and starts it again is not
/// raced by a second copy.
const LOOKS_BEFORE_REOPEN: u8 = 2;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Watch {
    missing: u8,
}

impl Watch {
    /// One look. Returns true when the app should be opened now.
    fn look(&mut self, running: bool, hidden_by_owner: bool) -> bool {
        if running || hidden_by_owner {
            self.missing = 0;
            return false;
        }
        self.missing += 1;
        if self.missing < LOOKS_BEFORE_REOPEN {
            return false;
        }
        self.missing = 0;
        true
    }
}

/// The bundle this binary runs from, when it is `…/X.app/Contents/Resources/plug`.
fn app_bundle(daemon_exe: &Path) -> Option<PathBuf> {
    let resources = daemon_exe.parent()?;
    let contents = resources.parent()?;
    let bundle = contents.parent()?;
    let inside_bundle = resources.file_name()? == "Resources"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    inside_bundle.then(|| bundle.to_path_buf())
}

/// `ps -axo comm=` prints one executable path per line.
fn app_is_running(ps_output: &str, app_exe: &Path) -> bool {
    ps_output
        .lines()
        .any(|line| Path::new(line.trim()) == app_exe)
}

fn hidden_marker() -> PathBuf {
    super::paths::runtime_dir().join("menu-bar-hidden")
}

pub(super) fn spawn(cancel: CancellationToken) {
    let Some(bundle) = std::env::current_exe()
        .ok()
        .and_then(|exe| app_bundle(&exe))
    else {
        return;
    };
    let app_exe = bundle.join("Contents/MacOS/Plug");
    tokio::spawn(async move {
        let mut watch = Watch::default();
        let mut ticker = tokio::time::interval(LOOK_EVERY);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = ticker.tick() => {}
            }
            let Ok(ps) = tokio::process::Command::new("/bin/ps")
                .args(["-axo", "comm="])
                .output()
                .await
            else {
                continue;
            };
            let running = app_is_running(&String::from_utf8_lossy(&ps.stdout), &app_exe);
            if !watch.look(running, hidden_marker().exists()) || cancel.is_cancelled() {
                continue;
            }
            tracing::info!(app = %bundle.display(), "opening Plug.app again: it was closed while the daemon is serving");
            // -g: not brought to the front. -j: launched hidden.
            let opened = tokio::process::Command::new("/usr/bin/open")
                .args(["-g", "-j"])
                .arg(&bundle)
                .status()
                .await;
            if !matches!(&opened, Ok(status) if status.success()) {
                tracing::warn!(result = ?opened, "could not open Plug.app again");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_closed_by_something_else_is_opened_on_the_second_look() {
        let mut watch = Watch::default();
        assert!(!watch.look(true, false));
        assert!(!watch.look(false, false));
        assert!(watch.look(false, false));
        // Opened; it gets two more looks to appear before another try.
        assert!(!watch.look(false, false));
        assert!(watch.look(false, false));
    }

    #[test]
    fn an_app_that_comes_back_by_itself_is_left_alone() {
        let mut watch = Watch::default();
        watch.look(true, false);
        assert!(!watch.look(false, false));
        assert!(!watch.look(true, false));
        assert!(!watch.look(false, false));
    }

    #[test]
    fn an_app_its_owner_closed_stays_closed() {
        let mut watch = Watch::default();
        watch.look(true, false);
        for _ in 0..5 {
            assert!(!watch.look(false, true));
        }
    }

    #[test]
    fn an_app_not_open_when_the_daemon_starts_is_opened() {
        let mut watch = Watch::default();
        assert!(!watch.look(false, false));
        assert!(watch.look(false, false));
    }

    #[test]
    fn only_a_daemon_inside_the_app_watches_for_it() {
        assert_eq!(
            app_bundle(Path::new("/Applications/Plug.app/Contents/Resources/plug")),
            Some(PathBuf::from("/Applications/Plug.app"))
        );
        assert_eq!(
            app_bundle(Path::new("/Users/me/plug/target/debug/plug")),
            None
        );
        assert_eq!(app_bundle(Path::new("/opt/Contents/Resources/plug")), None);
    }

    #[test]
    fn the_app_is_found_by_its_own_executable_only() {
        let app = Path::new("/Applications/Plug.app/Contents/MacOS/Plug");
        let ps = "/sbin/launchd\n/Applications/Plug.app/Contents/Resources/plug\n";
        assert!(!app_is_running(ps, app));
        let ps = format!("{ps}/Applications/Plug.app/Contents/MacOS/Plug\n");
        assert!(app_is_running(&ps, app));
    }
}
