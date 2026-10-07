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
            let table = String::from_utf8_lossy(&output.stdout);
            let (pid, executable) = find_host(std::process::id(), &table)?;
            let mut host = describe(executable);
            if is_interpreter(executable) {
                host.script = command_line(pid).as_deref().and_then(|line| {
                    let script = script_of(line)?;
                    // Only a script typed without its folder needs it.
                    Some(if runs_a_module(line) || script.starts_with('/') {
                        script
                    } else {
                        anchored(&script, working_folder(pid).as_deref())
                    })
                });
                if let Some(name) = host.script.as_deref().and_then(script_name) {
                    host.name = name;
                }
            }
            Some(host)
        })
        .clone()
    }
}

/// The target this connector was linked as, from `plug connect --client`.
static LINK_TARGET: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

pub(crate) fn set_link_target(target: Option<String>) {
    let _ = LINK_TARGET.set(target);
}

pub(crate) fn link_target() -> Option<String> {
    LINK_TARGET.get().cloned().flatten()
}

/// Programs that run something else and say nothing about what. Two clients
/// started with a bare `python3` are the same executable, so the script is
/// what tells them apart.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "ruby", "perl"];

fn is_interpreter(executable: &str) -> bool {
    let name = file_name(executable);
    name.starts_with("python") || INTERPRETERS.contains(&name)
}

#[cfg(not(test))]
fn command_line(pid: u32) -> Option<String> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The folder a process was started in, which a script named without one
/// is found from.
#[cfg(not(test))]
fn working_folder(pid: u32) -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .filter(|folder| folder.starts_with('/'))
        .map(str::to_string)
}

/// Whether the command line names a module with `-m`, not a file.
fn runs_a_module(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .skip(1)
        .find(|word| !word.starts_with('-') || *word == "-m")
        == Some("-m")
}

/// A script's whole path. Typed as `main.py` it is whichever `main.py` the
/// folder holds, so two started that way from two folders are two clients.
/// Left as typed when the folder cannot be read.
fn anchored(script: &str, folder: Option<&str>) -> String {
    let Some(folder) = folder.filter(|_| !script.starts_with('/')) else {
        return script.to_string();
    };
    let mut parts: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    for part in script.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// What an interpreter's command line says it runs: the script path, or the
/// module after `-m`. `None` for inline code and for a bare interpreter.
/// `ps` does not quote, so a path with a space in it is cut at the space;
/// the key is still stable for that client.
fn script_of(command_line: &str) -> Option<String> {
    let mut words = command_line.split_whitespace().skip(1);
    while let Some(word) = words.next() {
        match word {
            "-m" => return words.next().map(str::to_string),
            "-c" | "-e" | "--eval" | "-p" | "--print" => return None,
            // `deno run`, `bun run`
            "run" => continue,
            flag if flag.starts_with('-') => continue,
            script => return Some(script.to_string()),
        }
    }
    None
}

/// File and folder names that say nothing about whose script it is.
const PLAIN_NAMES: &[&str] = &[
    "main", "index", "agent", "server", "app", "run", "cli", "start", "mcp", "__main__", "src",
    "bin", "dist", "build", "lib", "scripts",
];

/// What to call a client known only by its script: `python3` names every
/// script alike. The file's own name, or the nearest folder with a name of
/// its own when the file is a `main.py`; for a module, its package.
fn script_name(script: &str) -> Option<String> {
    let mut parts = script.rsplit('/').filter(|part| !part.is_empty());
    let file = parts.next()?;
    let own = if script.contains('/') {
        file.rsplit_once('.').map_or(file, |(stem, _)| stem)
    } else {
        // `-m package.module`, or a script named without a folder.
        match file.rsplit_once('.') {
            Some((package, last)) if PLAIN_NAMES.contains(&last) => {
                package.split('.').next().unwrap_or(package)
            }
            Some((stem, "py" | "js" | "mjs" | "cjs" | "ts" | "rb" | "pl")) => stem,
            Some((package, _)) => package.split('.').next().unwrap_or(package),
            None => file,
        }
    };
    let plain = |name: &str| PLAIN_NAMES.contains(&name) || name.starts_with('.');
    let name = if plain(own) {
        parts.find(|part| !plain(part)).unwrap_or(own)
    } else {
        own
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// The host of `pid` in a `ps -axo pid=,ppid=,comm=` listing.
#[cfg(test)]
fn host_of(pid: u32, table: &str) -> Option<ClientHost> {
    find_host(pid, table).map(|(_, executable)| describe(executable))
}

/// The pid and executable of the program that started `pid`.
fn find_host(pid: u32, table: &str) -> Option<(u32, &str)> {
    let processes: std::collections::HashMap<u32, (u32, &str)> =
        table.lines().filter_map(parse_row).collect();
    let (mut parent, _) = *processes.get(&pid)?;
    let mut found: Option<(u32, &str)> = None;
    for _ in 0..MAX_HOPS {
        // launchd adopts orphans and starts services; it is nobody's host.
        if parent <= 1 {
            break;
        }
        let Some(&(grandparent, executable)) = processes.get(&parent) else {
            break;
        };
        found = Some((parent, executable));
        if !WRAPPERS.contains(&file_name(executable)) {
            break;
        }
        parent = grandparent;
    }
    found
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
        script: None,
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
    fn an_interpreter_is_told_apart_by_what_it_runs() {
        assert!(is_interpreter(
            "/Users/me/.hermes/tools/python/bin/python3.12"
        ));
        assert!(is_interpreter("/opt/homebrew/bin/node"));
        assert!(!is_interpreter(
            "/Applications/Cursor.app/Contents/MacOS/Cursor"
        ));

        assert_eq!(
            script_of("python3 -u /opt/hermes/agent.py --port 1").as_deref(),
            Some("/opt/hermes/agent.py")
        );
        assert_eq!(
            script_of("/usr/bin/python3 -m hermes.agent").as_deref(),
            Some("hermes.agent")
        );
        assert_eq!(
            script_of("deno run --allow-all main.ts").as_deref(),
            Some("main.ts")
        );
        assert_eq!(script_of("node -e console.log(1)"), None);
        assert_eq!(script_of("python3"), None);
    }

    #[test]
    fn a_script_typed_without_its_folder_is_found_from_where_it_was_started() {
        assert_eq!(
            anchored("main.py", Some("/work/radar")),
            "/work/radar/main.py"
        );
        assert_eq!(
            anchored("./tools/run.py", Some("/work/radar")),
            "/work/radar/tools/run.py"
        );
        assert_eq!(
            anchored("../other/main.py", Some("/work/radar")),
            "/work/other/main.py"
        );
        assert_eq!(
            anchored("/opt/agent.py", Some("/work/radar")),
            "/opt/agent.py"
        );
        assert_eq!(anchored("main.py", None), "main.py");
        // A module is not a file and stays as it is named.
        assert!(runs_a_module("/usr/bin/python3 -u -m hermes.agent"));
        assert!(!runs_a_module("python3 -u main.py -m fast"));
        assert!(!runs_a_module("python3"));
    }

    #[test]
    fn a_script_is_called_by_its_own_name_not_its_interpreters() {
        let name = |script| script_name(script).unwrap();
        assert_eq!(
            name("/Users/me/radar/housing-listing-radar.py"),
            "housing-listing-radar"
        );
        // A `main.py` is named by the folder that holds it.
        assert_eq!(name("/opt/hermes/agent.py"), "hermes");
        assert_eq!(name("/Users/me/code/radar/src/main.ts"), "radar");
        assert_eq!(name("/Users/me/.hermes/bin/hermes"), "hermes");
        // A module is named by its package.
        assert_eq!(name("hermes.agent"), "hermes");
        assert_eq!(name("hermes.gateway.worker"), "hermes");
        assert_eq!(name("main.ts"), "main");
        assert_eq!(name("radar.py"), "radar");
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
