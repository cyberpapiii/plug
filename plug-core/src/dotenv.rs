//! Minimal `.env` file loader for plug.
//!
//! Reads `KEY=VALUE` lines from `<config_dir>/.env` and sets them as
//! environment variables. This ensures secrets are available regardless of
//! how plug was launched (terminal, launchd, GUI app, etc.).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Returns the path to the `.env` file (sibling of `config.toml`).
pub fn env_file_path() -> PathBuf {
    crate::config::default_config_path()
        .parent()
        .map(|p| p.join(".env"))
        .unwrap_or_else(|| PathBuf::from(".env"))
}

/// Load the `.env` file and return variables that aren't already set.
///
/// - Skips blank lines and comments (`#`).
/// - Does NOT include variables already set in the environment.
///
/// Caller is responsible for calling `std::env::set_var` (unsafe in Rust 2024).
pub fn load_dotenv() -> HashMap<String, String> {
    let path = env_file_path();
    load_dotenv_from(&path)
}

/// Load a `.env` file from a specific path, returning new vars only.
pub fn load_dotenv_from(path: &Path) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let vars = parse_dotenv(&content);
    vars.into_iter()
        .filter(|(key, _)| std::env::var(key).is_err())
        .collect()
}

/// Parse a `.env` file into key-value pairs.
///
/// Returns a map so that later lines override earlier ones (last wins).
pub fn parse_dotenv(content: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Skip lines without '='
        let Some(eq_pos) = trimmed.find('=') else {
            continue;
        };

        let key = trimmed[..eq_pos].trim();
        let mut value = trimmed[eq_pos + 1..].trim();

        // Skip invalid keys (must be non-empty, no spaces)
        if key.is_empty() || key.contains(' ') {
            continue;
        }

        // Strip matching quotes from value
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            if value.len() >= 2 {
                value = &value[1..value.len() - 1];
            }
        }

        // Strip inline comments (only for unquoted values)
        let value = if !trimmed[eq_pos + 1..].trim().starts_with('"')
            && !trimmed[eq_pos + 1..].trim().starts_with('\'')
        {
            value.split('#').next().unwrap_or(value).trim_end()
        } else {
            value
        };

        vars.insert(key.to_string(), value.to_string());
    }

    vars
}

/// Read the `.env` file and return its key-value pairs without setting them.
///
/// Used by `auto_start_daemon` to forward env vars to the daemon process.
pub fn read_dotenv_vars() -> HashMap<String, String> {
    let path = env_file_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => parse_dotenv(&content),
        Err(_) => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let content = "FOO=bar\nBAZ=qux\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "bar");
        assert_eq!(vars.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn parse_quoted_values() {
        let content = "FOO=\"hello world\"\nBAR='single quoted'\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "hello world");
        assert_eq!(vars.get("BAR").unwrap(), "single quoted");
    }

    #[test]
    fn parse_comments_and_blanks() {
        let content = "# This is a comment\n\nFOO=bar\n  # Another comment\nBAZ=qux\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn parse_inline_comment() {
        let content = "FOO=bar # this is a comment\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn parse_no_override_later_wins() {
        let content = "FOO=first\nFOO=second\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "second");
    }

    #[test]
    fn parse_empty_value() {
        let content = "FOO=\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "");
    }

    #[test]
    fn parse_spaces_around_equals() {
        let content = "FOO = bar\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn parse_skips_invalid_lines() {
        let content = "no_equals_here\n=no_key\nGOOD=value\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars.get("GOOD").unwrap(), "value");
    }
}
