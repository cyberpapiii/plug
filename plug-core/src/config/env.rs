//! `$NAME` in config values, and the `.env` file beside `config.toml` that
//! supplies names the environment lacks.
//!
//! One rule decides what `$NAME` is, for config values and for the
//! environment servers inherit alike: a variable the process environment
//! already has wins, and `.env` fills in the rest.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// The variables `$NAME` in a config at `config_path` resolves against.
pub fn env_source(config_path: &Path) -> HashMap<String, String> {
    let mut vars = read_dotenv(&env_file_path(config_path));
    vars.extend(std::env::vars());
    vars
}

/// The `.env` variables the process environment does not have yet, for the
/// caller to set before anything reads the environment.
pub fn dotenv_to_apply(config_path: &Path) -> HashMap<String, String> {
    let mut vars = read_dotenv(&env_file_path(config_path));
    vars.retain(|key, _| std::env::var_os(key).is_none());
    vars
}

/// The `.env` file beside `config_path`.
pub fn env_file_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(".env")
}

/// The file's variables, or none when it cannot be read.
pub fn read_dotenv(path: &Path) -> HashMap<String, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_dotenv(&content),
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), %error, "could not read .env");
            }
            HashMap::new()
        }
    }
}

/// Each `$NAME` in `input` with its byte range. A name is `[A-Z_][A-Z0-9_]*`,
/// so `$(...)`, `${...}` and `$lower` are left alone.
fn references(input: &str) -> impl Iterator<Item = (Range<usize>, &str)> {
    let bytes = input.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            let dollar = i;
            i += 1;
            if bytes[dollar] != b'$'
                || !bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_uppercase() || *b == b'_')
            {
                continue;
            }
            let start = i;
            while bytes
                .get(i)
                .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            {
                i += 1;
            }
            return Some((dollar..i, &input[start..i]));
        }
        None
    })
}

/// `input` with each `$NAME` that `vars` has replaced by its value. An
/// unknown name is left as written.
pub fn expand(input: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(input.len());
    let mut copied = 0;
    for (range, name) in references(input) {
        if let Some(value) = vars.get(name) {
            result.push_str(&input[copied..range.start]);
            result.push_str(value);
            copied = range.end;
        }
    }
    result.push_str(&input[copied..]);
    result
}

/// The names `input` refers to with `$NAME`.
pub fn env_refs(input: &str) -> Vec<String> {
    references(input)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// Parse a `.env` file into key-value pairs.
///
/// Returns a map so that later lines override earlier ones (last wins).
pub fn parse_dotenv(content: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(eq_pos) = trimmed.find('=') else {
            continue;
        };

        let key = trimmed[..eq_pos].trim();
        let mut value = trimmed[eq_pos + 1..].trim();

        if key.is_empty() || key.contains(' ') {
            continue;
        }

        // Strip matching quotes from value
        if ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
            && value.len() >= 2
        {
            value = &value[1..value.len() - 1];
        }

        // Strip inline comments only when '#' begins a comment (`<ws>#comment`).
        let value = if !trimmed[eq_pos + 1..].trim().starts_with('"')
            && !trimmed[eq_pos + 1..].trim().starts_with('\'')
        {
            let mut comment_idx = None;
            for (idx, ch) in value.char_indices() {
                if ch == '#' {
                    if idx == 0 {
                        comment_idx = Some(idx);
                        break;
                    }
                    if value[..idx].chars().last().is_some_and(char::is_whitespace) {
                        comment_idx = Some(idx);
                        break;
                    }
                }
            }
            match comment_idx {
                Some(idx) => value[..idx].trim_end(),
                None => value,
            }
        } else {
            value
        };

        vars.insert(key.to_string(), value.to_string());
    }

    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn expands_known_names_and_leaves_the_rest() {
        let vars = vars(&[("HOME", "/Users/me"), ("A_1", "x")]);
        assert_eq!(expand("path=$HOME/config", &vars), "path=/Users/me/config");
        assert_eq!(expand("$A_1$A_1", &vars), "xx");
        assert_eq!(expand("$PLUG_NONEXISTENT", &vars), "$PLUG_NONEXISTENT");
        assert_eq!(
            expand("$(whoami) ${HOME} $lower trail$", &vars),
            "$(whoami) ${HOME} $lower trail$"
        );
        assert_eq!(expand("caf\u{e9} $HOME", &vars), "caf\u{e9} /Users/me");
    }

    #[test]
    fn finds_the_names_expand_would_replace() {
        assert_eq!(env_refs("$HOME/bin"), vec!["HOME"]);
        assert_eq!(env_refs("$FOO and $BAR"), vec!["FOO", "BAR"]);
        assert!(env_refs("no vars $(shell) $lowercase").is_empty());
    }

    #[test]
    fn the_process_environment_wins_over_dotenv() {
        let dir = crate::paths::test::scratch("env-source");
        std::fs::write(
            dir.join(".env"),
            "HOME=/from/dotenv\nPLUG_ENV_SOURCE_ONLY_IN_FILE=file\n",
        )
        .unwrap();
        let config = dir.join("config.toml");

        let source = env_source(&config);
        assert_eq!(source["HOME"], std::env::var("HOME").unwrap());
        assert_eq!(source["PLUG_ENV_SOURCE_ONLY_IN_FILE"], "file");

        let apply = dotenv_to_apply(&config);
        assert!(!apply.contains_key("HOME"));
        assert_eq!(apply["PLUG_ENV_SOURCE_ONLY_IN_FILE"], "file");
    }

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
    fn parse_hash_without_comment_whitespace_keeps_value() {
        let content = "FOO=abc#def\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.get("FOO").unwrap(), "abc#def");
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
