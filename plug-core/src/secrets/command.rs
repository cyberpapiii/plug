//! A store that is a command: Plug runs it with the secret's name and takes
//! what it prints. This is how any password manager with a command-line tool
//! becomes a store, with nothing added to Plug.

use std::ffi::OsString;
use std::io::Read;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::{SecretError, SecretStore, valid_name};
use crate::types::SecretString;

/// Where `{name}` goes in the command.
pub const NAME: &str = "{name}";

/// The 1Password store's id: `op://vault/item/field` is a reference as it is.
pub const OP: &str = "op";

/// The login shell's PATH. The service starts with a bare one, and a
/// password manager's tool is rarely in `/usr/bin`.
static LOGIN_PATH: OnceLock<OsString> = OnceLock::new();

/// Give command stores the PATH a terminal would have.
pub fn use_login_path(path: OsString) {
    let _ = LOGIN_PATH.set(path);
}

pub struct Command {
    id: String,
    argv: Vec<String>,
    timeout: Duration,
    /// What a name may be. A command of the person's own gets a name safe to
    /// hand to any program; 1Password's own reference syntax is wider.
    accepts: fn(&str) -> bool,
}

impl Command {
    pub fn new(id: &str, argv: Vec<String>, timeout: Duration) -> Self {
        Self {
            id: id.to_string(),
            argv,
            timeout,
            accepts: valid_name,
        }
    }

    /// `op://vault/item/field`, read with the 1Password command-line tool.
    pub fn one_password() -> Self {
        Self {
            id: OP.to_string(),
            argv: ["op", "read", "--no-newline", "op:{name}"]
                .map(str::to_string)
                .to_vec(),
            timeout: Duration::from_secs(15),
            accepts: |name| {
                name.starts_with("//") && name.len() > 2 && !name.chars().any(char::is_control)
            },
        }
    }

    fn unavailable(&self, reason: impl ToString) -> SecretError {
        SecretError::Unavailable {
            store: self.id.clone(),
            reason: reason.to_string(),
        }
    }

    fn read_only(&self) -> SecretError {
        self.unavailable("Plug only reads this store; change the value where it lives")
    }
}

/// Everything a pipe gives, read on its own thread so a full pipe never
/// stalls the command.
/// How long past the deadline a finished command's output may take to arrive.
const PIPE_GRACE: Duration = Duration::from_millis(250);

/// Read a pipe to its end on another thread. The bytes arrive on the
/// channel, so the caller can stop waiting: a program the command started
/// may hold the pipe open long after the command itself is done.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

impl SecretStore for Command {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        let mut argv = self.argv.iter().map(|arg| arg.replace(NAME, name));
        let program = argv
            .next()
            .ok_or_else(|| self.unavailable("no command is configured"))?;
        let mut command = std::process::Command::new(&program);
        command
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = LOGIN_PATH.get() {
            command.env("PATH", path);
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                self.unavailable(format!("`{program}` is not installed"))
            } else {
                self.unavailable(error)
            }
        })?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SecretError::TimedOut {
                        store: self.id.clone(),
                    });
                }
                Err(error) => return Err(self.unavailable(error)),
            }
        };
        let left = || deadline.saturating_duration_since(Instant::now()) + PIPE_GRACE;
        let Ok(stdout) = stdout.recv_timeout(left()) else {
            return Err(SecretError::TimedOut {
                store: self.id.clone(),
            });
        };
        let stderr = stderr.recv_timeout(left()).unwrap_or_default();

        if !status.success() {
            // The tool's own first line says it best: locked, not signed in,
            // no such item.
            let said = String::from_utf8_lossy(&stderr);
            let reason = said
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(200).collect::<String>())
                .unwrap_or_else(|| format!("`{program}` failed ({status})"));
            return Err(self.unavailable(reason));
        }
        let value = String::from_utf8(stdout)
            .map_err(|_| self.unavailable("the command printed something that is not text"))?;
        let value = value.strip_suffix('\n').unwrap_or(&value);
        let value = value.strip_suffix('\r').unwrap_or(value);
        Ok((!value.is_empty()).then(|| value.to_string().into()))
    }

    fn set(&self, _: &str, _: &SecretString) -> Result<(), SecretError> {
        Err(self.read_only())
    }

    fn remove(&self, _: &str) -> Result<(), SecretError> {
        Err(self.read_only())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn accepts(&self, name: &str) -> bool {
        (self.accepts)(name)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(script: &str, timeout: Duration) -> Command {
        Command::new(
            "vault",
            ["sh", "-c", script, "sh", NAME]
                .map(str::to_string)
                .to_vec(),
            timeout,
        )
    }

    #[test]
    fn what_the_command_prints_is_the_value() {
        let store = shell("printf 'value-for-%s\\n' \"$1\"", Duration::from_secs(5));
        assert_eq!(
            store.get("github").unwrap().unwrap().as_str(),
            "value-for-github"
        );
        let silent = shell("true", Duration::from_secs(5));
        assert!(silent.get("github").unwrap().is_none());
    }

    #[test]
    fn a_command_that_fails_gives_its_own_reason() {
        let store = shell("echo 'vault is locked' >&2; exit 1", Duration::from_secs(5));
        assert_eq!(
            store.get("github").unwrap_err().to_string(),
            "waiting for vault: vault is locked"
        );
        let absent = Command::new(
            "vault",
            vec!["plug-no-such-tool".to_string(), NAME.to_string()],
            Duration::from_secs(5),
        );
        assert_eq!(
            absent.get("github").unwrap_err().to_string(),
            "waiting for vault: `plug-no-such-tool` is not installed"
        );
    }

    #[test]
    fn a_command_that_waits_for_a_person_is_stopped() {
        let store = shell("sleep 30", Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(
            store.get("github").unwrap_err(),
            SecretError::TimedOut {
                store: "vault".to_string()
            }
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_program_the_command_left_running_does_not_hold_the_read() {
        // The command is done at once; what it started keeps the pipe open.
        let store = shell("sleep 30 & echo value", Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(
            store.get("github").unwrap_err(),
            SecretError::TimedOut {
                store: "vault".to_string()
            }
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_command_store_is_read_only_and_one_password_takes_its_own_names() {
        let store = shell("true", Duration::from_secs(1));
        assert!(store.set("a", &"b".to_string().into()).is_err());
        assert!(store.accepts("github") && !store.accepts("//vault/item/field"));
        let op = Command::one_password();
        assert!(op.accepts("//Private/My Item/password"));
        assert!(!op.accepts("github") && !op.accepts("//"));
    }
}
