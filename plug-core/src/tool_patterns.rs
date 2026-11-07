//! The `*` patterns `disabled_tools` and similar lists are written in.

/// True when `patterns` (`disabled_tools` entries) hide `tool_name`.
pub fn is_disabled_tool(patterns: &[String], tool_name: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    // The router keeps its patterns lowercased; other callers may not.
    let tool_name = tool_name.to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        if pattern.bytes().all(|b| !b.is_ascii_uppercase()) {
            wildcard_match(pattern, &tool_name)
        } else {
            wildcard_match(&pattern.to_ascii_lowercase(), &tool_name)
        }
    })
}

pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut remainder = text;
    let mut first = true;

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if first && !pattern.starts_with('*') {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            first = false;
            continue;
        }

        if index == parts.len() - 1 && !pattern.ends_with('*') {
            return remainder.ends_with(part);
        }

        if let Some(found) = remainder.find(part) {
            remainder = &remainder[found + part.len()..];
            first = false;
        } else {
            return false;
        }
    }

    true
}

/// The wildcard entry in `patterns` hiding `tool_name`, if the tool is not
/// listed by its exact name. Operator surfaces use this to explain why a
/// single tool cannot be switched back on by itself.
pub fn disabling_wildcard(patterns: &[String], tool_name: &str) -> Option<String> {
    if patterns
        .iter()
        .any(|pattern| pattern.eq_ignore_ascii_case(tool_name))
    {
        return None;
    }
    patterns
        .iter()
        .find(|pattern| is_disabled_tool(std::slice::from_ref(*pattern), tool_name))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_tool_patterns_support_exact_and_wildcard_matches() {
        assert!(is_disabled_tool(
            &["slack__search_messages".into()],
            "Slack__search_messages"
        ));
        assert!(is_disabled_tool(
            &["slack__*".into()],
            "Slack__search_messages"
        ));
        assert!(is_disabled_tool(
            &["*search*".into()],
            "Slack__search_messages"
        ));
        assert!(!is_disabled_tool(
            &["gmail__*".into()],
            "Slack__search_messages"
        ));
    }
}
