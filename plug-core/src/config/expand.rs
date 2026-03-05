use std::env;

/// Expand `$VAR_NAME` references in a string value.
///
/// Only expands variables matching the allowlist pattern `$[A-Z_][A-Z0-9_]*`
/// to prevent shell injection via `$(...)` or `${...}` patterns.
///
/// Unknown variables are left as-is (not expanded).
pub fn expand_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            // Only expand $UPPER_CASE_NAME, not $(...) or ${...}
            if next.is_ascii_uppercase() || next == b'_' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_uppercase()
                        || bytes[end].is_ascii_digit()
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                let var_name = &input[start..end];
                if !var_name.is_empty() {
                    match env::var(var_name) {
                        Ok(val) => result.push_str(&val),
                        Err(_) => {
                            // Leave unexpanded
                            result.push('$');
                            result.push_str(var_name);
                        }
                    }
                    i = end;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        result.push(ch);
        i += ch.len_utf8();
    }

    result
}

/// Extract `$VAR_NAME` references from a string without expanding them.
///
/// Returns the list of variable names (without the `$` prefix).
pub(crate) fn extract_env_refs(input: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next.is_ascii_uppercase() || next == b'_' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_uppercase()
                        || bytes[end].is_ascii_digit()
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                let var_name = &input[start..end];
                if !var_name.is_empty() {
                    refs.push(var_name.to_string());
                    i = end;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }

    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_known_var() {
        // Use HOME which is always set on Unix systems
        let home = env::var("HOME").unwrap();
        assert_eq!(expand_env_vars("$HOME"), home);
        assert_eq!(
            expand_env_vars("path=$HOME/config"),
            format!("path={home}/config")
        );
    }

    #[test]
    fn leave_unknown_var() {
        assert_eq!(
            expand_env_vars("$PLUG_NONEXISTENT_12345"),
            "$PLUG_NONEXISTENT_12345"
        );
    }

    #[test]
    fn no_expand_shell_injection() {
        // $(command) should NOT be expanded
        assert_eq!(expand_env_vars("$(whoami)"), "$(whoami)");
        // ${VAR} should NOT be expanded (brace syntax)
        assert_eq!(expand_env_vars("${HOME}"), "${HOME}");
    }

    #[test]
    fn no_expand_lowercase() {
        assert_eq!(expand_env_vars("$lowercase"), "$lowercase");
    }

    #[test]
    fn empty_and_no_vars() {
        assert_eq!(expand_env_vars(""), "");
        assert_eq!(expand_env_vars("no vars here"), "no vars here");
    }

    #[test]
    fn dollar_at_end() {
        assert_eq!(expand_env_vars("trail$"), "trail$");
    }
}
