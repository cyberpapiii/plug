//! Which program started this `plug connect`.
//!
//! A client names itself in MCP `initialize`, and many send something generic
//! (`mcp`) or nothing a person would recognise. The process table is a better
//! witness: the connector's parent is the program that launched it, whatever
//! that program chooses to say.

use plug_core::ipc::ClientHost;

/// Launchers that start a connector on behalf of something else. Their parent
/// is the program worth naming.
const WRAPPERS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "env", "npx", "uvx"];

/// How far up to look past wrappers before settling for what is there.
const MAX_HOPS: usize = 6;

/// The host of this process, read once. `None` when the process table cannot
/// be read; a session without a host is shown as it was before.
pub(crate) fn current() -> Option<ClientHost> {
    #[cfg(test)]
    return None;

    #[cfg(not(test))]
    {
        static HOST: std::sync::OnceLock<Option<ClientHost>> = std::sync::OnceLock::new();
        HOST.get_or_init(|| {
            let output = std::process::Command::new("/bin/ps")
                .args(["-axo", "pid=,ppid=,comm="])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            host_of(std::process::id(), &String::from_utf8_lossy(&output.stdout))
        })
        .clone()
    }
}

/// The host of `pid` in a `ps -axo pid=,ppid=,comm=` listing.
fn host_of(pid: u32, table: &str) -> Option<ClientHost> {
    let processes: std::collections::HashMap<u32, (u32, &str)> =
        table.lines().filter_map(parse_row).collect();
    let (mut parent, _) = *processes.get(&pid)?;
    let mut found = None;
    for _ in 0..MAX_HOPS {
        // launchd adopts orphans and starts services; it is nobody's host.
        if parent <= 1 {
            break;
        }
        let Some(&(grandparent, executable)) = processes.get(&parent) else {
            break;
        };
        found = Some(executable);
        if !WRAPPERS.contains(&file_name(executable)) {
            break;
        }
        parent = grandparent;
    }
    found.map(describe)
}

fn parse_row(line: &str) -> Option<(u32, (u32, &str))> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let (ppid, executable) = rest.trim_start().split_once(char::is_whitespace)?;
    let executable = executable.trim();
    if executable.is_empty() {
        return None;
    }
    Some((pid.parse().ok()?, (ppid.parse().ok()?, executable)))
}

fn file_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A login shell shows up as `-zsh`.
    name.strip_prefix('-').unwrap_or(name)
}

fn describe(executable: &str) -> ClientHost {
    // The outermost bundle: a helper nested inside an app belongs to that app.
    let app = executable
        .match_indices(".app/")
        .next()
        .map(|(index, _)| &executable[..index + ".app".len()]);
    let name = match app {
        Some(app) => file_name(app).trim_end_matches(".app"),
        None => file_name(executable),
    };
    ClientHost {
        name: name.to_string(),
        executable: executable.to_string(),
        app: app.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connector_started_by_an_app_is_hosted_by_that_app() {
        let table = "\
  100     1 /Applications/Cursor.app/Contents/MacOS/Cursor
  200   100 /Applications/Cursor.app/Contents/Frameworks/Cursor Helper (Plugin).app/Contents/MacOS/Cursor Helper (Plugin)
  300   200 /Users/me/.local/bin/plug
";
        let host = host_of(300, table).expect("host");
        assert_eq!(host.name, "Cursor");
        assert_eq!(host.app.as_deref(), Some("/Applications/Cursor.app"));
        assert!(host.executable.ends_with("Cursor Helper (Plugin)"));
    }

    #[test]
    fn a_command_line_host_is_named_by_its_executable() {
        let table = "\
  583     1 /usr/bin/osascript
 1152   583 /Users/me/.hermes/tools/python/bin/python3
 4227  1152 /Users/me/.local/bin/plug
";
        let host = host_of(4227, table).expect("host");
        assert_eq!(host.name, "python3");
        assert_eq!(host.app, None);
    }

    #[test]
    fn shells_and_launchers_are_looked_through() {
        let table = "\
   50     1 /Applications/Hermes.app/Contents/MacOS/Hermes
   60    50 -zsh
   70    60 /usr/bin/env
   80    70 /Users/me/.local/bin/plug
";
        assert_eq!(host_of(80, table).expect("host").name, "Hermes");
    }

    #[test]
    fn a_shell_with_nothing_above_it_is_still_a_host() {
        let table = "\
   60     1 /bin/zsh
   80    60 /Users/me/.local/bin/plug
";
        assert_eq!(host_of(80, table).expect("host").name, "zsh");
    }

    #[test]
    fn a_connector_started_by_launchd_has_no_host() {
        assert_eq!(host_of(80, "   80     1 /Users/me/.local/bin/plug\n"), None);
        assert_eq!(host_of(80, ""), None);
    }
}
