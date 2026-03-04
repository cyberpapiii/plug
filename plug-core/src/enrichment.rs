//! Tool enrichment: annotation inference and name normalization.
//!
//! Opt-in per server via `enrichment = true` in config.toml.
//! Fills in missing annotations — never overrides upstream values.

use rmcp::model::Tool;

/// Apply enrichment to a tool: infer annotations from name patterns,
/// normalize title from snake_case. Only fills missing values.
pub fn enrich_tool(tool: &mut Tool) {
    infer_annotations(tool);
    infer_title(tool);
}

/// Infer annotation hints from tool name patterns.
///
/// Rules:
/// - `get_*`, `list_*`, `search_*`, `read_*`, `fetch_*` → readOnlyHint: true
/// - `delete_*`, `remove_*`, `drop_*`, `destroy_*` → destructiveHint: true
/// - `create_*`, `add_*`, `insert_*`, `set_*`, `update_*`, `write_*` → readOnlyHint: false
fn infer_annotations(tool: &mut Tool) {
    let name: &str = &tool.name;

    // Only fill in if annotations aren't already set
    let annotations = tool.annotations.get_or_insert_with(Default::default);

    let read_prefixes = ["get_", "list_", "search_", "read_", "fetch_"];
    let destructive_prefixes = ["delete_", "remove_", "drop_", "destroy_"];
    let write_prefixes = ["create_", "add_", "insert_", "set_", "update_", "write_"];

    if annotations.read_only_hint.is_none() {
        if read_prefixes.iter().any(|p| name.starts_with(p)) {
            annotations.read_only_hint = Some(true);
        } else if write_prefixes.iter().any(|p| name.starts_with(p))
            || destructive_prefixes.iter().any(|p| name.starts_with(p))
        {
            annotations.read_only_hint = Some(false);
        }
    }

    if annotations.destructive_hint.is_none()
        && destructive_prefixes.iter().any(|p| name.starts_with(p))
    {
        annotations.destructive_hint = Some(true);
    }
}

/// Infer a human-readable title from a snake_case tool name.
/// `create_github_issue` → `"Create Github Issue"`
/// Only sets `title` if not already present.
fn infer_title(tool: &mut Tool) {
    let annotations = tool.annotations.get_or_insert_with(Default::default);
    if annotations.title.is_some() {
        return;
    }

    let title = tool
        .name
        .split('_')
        .filter(|s| !s.is_empty())
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ");

    if !title.is_empty() {
        annotations.title = Some(title);
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{Tool, ToolAnnotations};
    use serde_json::json;

    fn make_tool(name: &str) -> Tool {
        serde_json::from_value(json!({
            "name": name,
            "inputSchema": { "type": "object" }
        }))
        .unwrap()
    }

    #[test]
    fn read_only_hint_inferred() {
        for prefix in ["get_", "list_", "search_", "read_", "fetch_"] {
            let name = format!("{prefix}items");
            let mut tool = make_tool(&name);
            enrich_tool(&mut tool);
            assert_eq!(
                tool.annotations.as_ref().unwrap().read_only_hint,
                Some(true),
                "expected readOnlyHint=true for {name}"
            );
        }
    }

    #[test]
    fn destructive_hint_inferred() {
        for prefix in ["delete_", "remove_", "drop_", "destroy_"] {
            let name = format!("{prefix}items");
            let mut tool = make_tool(&name);
            enrich_tool(&mut tool);
            let ann = tool.annotations.as_ref().unwrap();
            assert_eq!(
                ann.destructive_hint,
                Some(true),
                "expected destructiveHint=true for {name}"
            );
            assert_eq!(
                ann.read_only_hint,
                Some(false),
                "expected readOnlyHint=false for {name}"
            );
        }
    }

    #[test]
    fn write_hint_inferred() {
        for prefix in ["create_", "add_", "insert_", "set_", "update_", "write_"] {
            let name = format!("{prefix}items");
            let mut tool = make_tool(&name);
            enrich_tool(&mut tool);
            let ann = tool.annotations.as_ref().unwrap();
            assert_eq!(
                ann.read_only_hint,
                Some(false),
                "expected readOnlyHint=false for {name}"
            );
        }
    }

    #[test]
    fn title_inferred_from_snake_case() {
        let mut tool = make_tool("create_github_issue");
        enrich_tool(&mut tool);
        assert_eq!(
            tool.annotations.as_ref().unwrap().title.as_deref(),
            Some("Create Github Issue")
        );
    }

    #[test]
    fn existing_annotations_not_overridden() {
        let mut tool: Tool = serde_json::from_value(json!({
            "name": "get_items",
            "inputSchema": { "type": "object" },
            "annotations": {
                "readOnlyHint": false,
                "title": "Custom Title"
            }
        }))
        .unwrap();

        enrich_tool(&mut tool);
        let ann = tool.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false)); // not overridden
        assert_eq!(ann.title.as_deref(), Some("Custom Title")); // not overridden
    }

    #[test]
    fn no_pattern_match_no_annotation() {
        let mut tool = make_tool("do_something");
        enrich_tool(&mut tool);
        let ann = tool.annotations.as_ref().unwrap();
        assert!(ann.read_only_hint.is_none());
        assert!(ann.destructive_hint.is_none());
        // But title should still be set
        assert_eq!(ann.title.as_deref(), Some("Do Something"));
    }
}
