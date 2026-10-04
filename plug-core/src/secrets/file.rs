//! The file store: the `.env` file beside `config.toml`, one `NAME=value` a
//! line. Plain text, for a machine with no credential store or a person who
//! wants to see and carry their keys as a file.

use std::path::PathBuf;

use super::{SecretError, SecretStore};
use crate::types::SecretString;

pub const FILE: &str = "file";

pub struct File {
    path: PathBuf,
}

impl File {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn unavailable(reason: impl ToString) -> SecretError {
        SecretError::Unavailable {
            store: FILE.to_string(),
            reason: reason.to_string(),
        }
    }

    /// The file's lines, or none when there is no file yet.
    fn lines(&self) -> Result<Vec<String>, SecretError> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(content.lines().map(str::to_string).collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(Self::unavailable(error)),
        }
    }

    /// Replace the file with `lines`, readable by its owner only.
    fn write(&self, lines: &[String]) -> Result<(), SecretError> {
        let mut content = lines.join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent).map_err(Self::unavailable)?;
        let temp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
        let written = (|| -> std::io::Result<()> {
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temp)?;
                file.write_all(content.as_bytes())?;
                file.sync_all()?;
            }
            #[cfg(not(unix))]
            std::fs::write(&temp, &content)?;
            std::fs::rename(&temp, &self.path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        written.map_err(Self::unavailable)
    }
}

/// Whether `line` assigns `name`.
fn assigns(line: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix(name)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// `value` as the right-hand side of a line that reads back unchanged.
fn quoted(value: &str) -> Option<String> {
    if value.contains(['\n', '\r']) {
        None
    } else if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else {
        None
    }
}

impl SecretStore for File {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        let content = self.lines()?.join("\n");
        Ok(crate::dotenv::parse_dotenv(&content)
            .remove(name)
            .map(SecretString::from))
    }

    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
        let value = quoted(value.as_str()).ok_or_else(|| {
            Self::unavailable(
                "a value with both kinds of quote, or a line break, does not fit a line",
            )
        })?;
        let mut lines = self.lines()?;
        lines.retain(|line| !assigns(line, name));
        lines.push(format!("{name}={value}"));
        self.write(&lines)
    }

    fn remove(&self, name: &str) -> Result<(), SecretError> {
        let mut lines = self.lines()?;
        let before = lines.len();
        lines.retain(|line| !assigns(line, name));
        if lines.len() == before {
            return Ok(());
        }
        self.write(&lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_set_is_the_value_read_and_other_lines_stay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# mine\nOTHER=1\nAPI = old\n").unwrap();
        let store = File::new(path.clone());

        for value in ["plain", "with # hash", " padded ", "it's", "say \"hi\""] {
            store.set("API", &value.to_string().into()).unwrap();
            assert_eq!(store.get("API").unwrap().unwrap().as_str(), value);
        }
        assert_eq!(store.get("OTHER").unwrap().unwrap().as_str(), "1");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# mine\nOTHER=1\n"), "{content}");
        assert_eq!(content.matches("API").count(), 1, "{content}");

        store.remove("API").unwrap();
        assert!(store.get("API").unwrap().is_none());
        assert!(store.set("API", &"a'b\"c".to_string().into()).is_err());
    }

    #[test]
    fn no_file_is_an_empty_store_and_a_new_file_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let store = File::new(path.clone());
        assert!(store.get("API").unwrap().is_none());
        store.remove("API").unwrap();
        assert!(!path.exists());

        store.set("API", &"value".to_string().into()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
