//! Tool enrichment: annotation inference and name normalization.
//!
//! Opt-in per server via `enrichment = true` in config.toml.
//! `plug` also applies semantic normalization to all routed tools so clients
//! do not inherit obviously wrong upstream safety hints.

use rmcp::model::{TaskSupport, Tool, ToolExecution};

/// Apply enrichment to a tool: infer annotations from name patterns,
/// normalize title from snake_case. Only fills missing values.
pub fn enrich_tool(tool: &mut Tool) {
    let name = tool.name.to_string();
    normalize_annotations(tool, &name);
    infer_title(tool);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolSemantics {
    ReadOnly,
    Mutating,
    Destructive,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolRepeatability {
    Idempotent,
    NonIdempotent,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolWorldScope {
    Closed,
    Open,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolTaskSupportMode {
    Forbidden,
    Optional,
    Required,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolProfile {
    semantics: ToolSemantics,
    repeatability: ToolRepeatability,
    world_scope: ToolWorldScope,
    task_support: ToolTaskSupportMode,
}

impl ToolProfile {
    fn infer(tool: &Tool, name: &str) -> Self {
        let action_values = action_values(tool);
        let semantics = classify_tool_semantics_with_metadata(
            name,
            tool.title.as_deref(),
            tool.description.as_deref(),
            action_values.as_deref(),
        );

        let repeatability = infer_repeatability(name, semantics, tool.description.as_deref());
        let world_scope = infer_world_scope(tool, name);
        let task_support = infer_task_support(tool, name, semantics);

        Self {
            semantics,
            repeatability,
            world_scope,
            task_support,
        }
    }
}

fn classify_tool_semantics_with_metadata(
    name: &str,
    title: Option<&str>,
    description: Option<&str>,
    action_values: Option<&[String]>,
) -> ToolSemantics {
    let from_name = classify_tool_semantics_from_tokens(name);
    if from_name != ToolSemantics::Unknown {
        return from_name;
    }

    let from_actions = classify_tool_semantics_from_actions(action_values);
    if from_actions != ToolSemantics::Unknown {
        return from_actions;
    }

    classify_tool_semantics_from_text(title, description)
}

fn classify_tool_semantics_from_tokens(name: &str) -> ToolSemantics {
    let read_tokens = [
        "get", "list", "search", "read", "fetch", "watch", "query", "resolve", "browse", "audit",
        "check", "inspect", "validate", "verify", "diagnose", "lint", "find", "capture", "reload",
        "download", "export", "generate", "history", "debug", "replies",
    ];
    let destructive_tokens = ["delete", "remove", "drop", "destroy"];
    let write_tokens = [
        "create",
        "add",
        "insert",
        "set",
        "update",
        "write",
        "edit",
        "modify",
        "post",
        "send",
        "draft",
        "rename",
        "move",
        "manage",
        "clone",
        "arrange",
        "instantiate",
        "batch",
        "start",
        "run",
        "clear",
        "duplicate",
        "resize",
        "skip",
        "format",
        "copy",
        "import",
        "toggle",
        "focus",
        "navigate",
        "reconnect",
        "mark",
        "join",
        "leave",
    ];

    let tokens: Vec<&str> = name.split('_').filter(|token| !token.is_empty()).collect();
    let first_match = |candidates: &[&str]| {
        tokens
            .iter()
            .position(|token| candidates.iter().any(|candidate| token == candidate))
    };

    let destructive_at = first_match(&destructive_tokens);
    let write_at = first_match(&write_tokens);
    let read_at = first_match(&read_tokens);

    match [destructive_at, write_at, read_at]
        .into_iter()
        .flatten()
        .min()
    {
        Some(pos) if destructive_at == Some(pos) => ToolSemantics::Destructive,
        Some(pos) if write_at == Some(pos) => ToolSemantics::Mutating,
        Some(pos) if read_at == Some(pos) => ToolSemantics::ReadOnly,
        _ => ToolSemantics::Unknown,
    }
}

fn classify_tool_semantics_from_actions(action_values: Option<&[String]>) -> ToolSemantics {
    let Some(values) = action_values else {
        return ToolSemantics::Unknown;
    };

    let mut saw_destructive = false;
    let mut saw_mutating = false;
    let mut saw_read = false;

    for action in values {
        match classify_tool_semantics_from_tokens(action) {
            ToolSemantics::Destructive => saw_destructive = true,
            ToolSemantics::Mutating => saw_mutating = true,
            ToolSemantics::ReadOnly => saw_read = true,
            ToolSemantics::Unknown => {}
        }
    }

    if saw_destructive {
        ToolSemantics::Destructive
    } else if saw_mutating {
        ToolSemantics::Mutating
    } else if saw_read {
        ToolSemantics::ReadOnly
    } else {
        ToolSemantics::Unknown
    }
}

fn classify_tool_semantics_from_text(
    title: Option<&str>,
    description: Option<&str>,
) -> ToolSemantics {
    let mut read_score = 0;
    let mut mutating_score = 0;
    let mut destructive_score = 0;

    if let Some(title) = title {
        score_text(
            title,
            2,
            &mut read_score,
            &mut mutating_score,
            &mut destructive_score,
        );
    }

    if let Some(description) = description {
        let first_sentence = description
            .split('\n')
            .find(|line| !line.trim().is_empty())
            .unwrap_or(description);
        score_text(
            first_sentence,
            3,
            &mut read_score,
            &mut mutating_score,
            &mut destructive_score,
        );
        score_text(
            description,
            1,
            &mut read_score,
            &mut mutating_score,
            &mut destructive_score,
        );
    }

    if destructive_score > 0
        && destructive_score >= mutating_score
        && destructive_score >= read_score
    {
        ToolSemantics::Destructive
    } else if mutating_score > 0 && mutating_score >= read_score {
        ToolSemantics::Mutating
    } else if read_score > 0 {
        ToolSemantics::ReadOnly
    } else {
        ToolSemantics::Unknown
    }
}

fn score_text(
    text: &str,
    weight: i32,
    read_score: &mut i32,
    mutating_score: &mut i32,
    destructive_score: &mut i32,
) {
    let lowered = text.to_ascii_lowercase();
    let starts_with = |prefixes: &[&str]| prefixes.iter().any(|prefix| lowered.starts_with(prefix));
    let contains_any = |patterns: &[&str]| patterns.iter().any(|pattern| lowered.contains(pattern));

    if starts_with(&[
        "get ",
        "list ",
        "read ",
        "search ",
        "find ",
        "fetch ",
        "retrieve ",
        "inspect ",
        "check ",
        "query ",
        "resolve ",
        "return ",
        "returns ",
        "given ",
        "export an image",
    ]) {
        *read_score += weight * 3;
    }

    if contains_any(&[
        "returns",
        "retrieve",
        "extract",
        "extracting",
        "debugging tool",
        "shows you",
        "current date/time",
        "date range enumeration",
        "playground link",
        "suggestions to fix",
        "research any company",
        "full content of a specific webpage",
        "image url",
        "thread of messages",
        "dashboard data",
        "token data",
    ]) {
        *read_score += weight;
    }

    if starts_with(&[
        "create ",
        "add ",
        "insert ",
        "set ",
        "update ",
        "write ",
        "edit ",
        "modify ",
        "post ",
        "send ",
        "draft ",
        "rename ",
        "move ",
        "manage ",
        "start ",
        "run ",
        "execute ",
        "clear ",
        "duplicate ",
        "toggle ",
        "apply ",
        "import ",
        "copy ",
        "mark ",
        "navigate ",
        "focus ",
        "force ",
        "resize ",
    ]) {
        *mutating_score += weight * 3;
    }

    if contains_any(&[
        "can modify your document",
        "create a complete",
        "applies formatting",
        "toggle whether",
        "imports a file",
        "copy of an existing",
        "mark a channel or dm as read",
        "join/leave",
        "authentication flow",
        "begin capturing console logs",
        "reconnection",
        "clone is placed",
        "executes a function",
        "research id",
        "clear the console log buffer",
        "duplicate an existing slide",
    ]) {
        *mutating_score += weight;
    }

    if contains_any(&[
        "destructive operation",
        "cannot be undone",
        "permanently delete",
        "warning: this is a destructive operation",
    ]) {
        *destructive_score += weight * 4;
    }
}

fn action_values(tool: &Tool) -> Option<Vec<String>> {
    let properties = tool.input_schema.get("properties")?.as_object()?;
    let action = properties.get("action")?.as_object()?;
    let values = action.get("enum")?.as_array()?;
    let actions = values
        .iter()
        .filter_map(|value| value.as_str().map(|s| s.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    (!actions.is_empty()).then_some(actions)
}

fn infer_repeatability(
    name: &str,
    semantics: ToolSemantics,
    description: Option<&str>,
) -> ToolRepeatability {
    if semantics == ToolSemantics::ReadOnly {
        return ToolRepeatability::Idempotent;
    }

    let lowered_name = name.to_ascii_lowercase();
    let description = description.unwrap_or("").to_ascii_lowercase();

    let idempotent_tokens = [
        "set",
        "update",
        "rename",
        "resize",
        "format",
        "mark",
        "skip",
        "focus",
        "navigate",
        "reconnect",
        "reload",
        "clear",
    ];
    if lowered_name
        .split('_')
        .any(|token| idempotent_tokens.contains(&token))
    {
        return ToolRepeatability::Idempotent;
    }

    let non_idempotent_tokens = [
        "create",
        "add",
        "insert",
        "post",
        "send",
        "draft",
        "duplicate",
        "copy",
        "import",
        "clone",
        "start",
        "join",
        "leave",
    ];
    if lowered_name
        .split('_')
        .any(|token| non_idempotent_tokens.contains(&token))
    {
        return ToolRepeatability::NonIdempotent;
    }

    if description.contains("same arguments")
        || description.contains("same call")
        || description.contains("without additional effect")
    {
        return ToolRepeatability::Idempotent;
    }

    match semantics {
        ToolSemantics::ReadOnly => ToolRepeatability::Idempotent,
        ToolSemantics::Destructive => ToolRepeatability::NonIdempotent,
        ToolSemantics::Mutating => ToolRepeatability::Unknown,
        ToolSemantics::Unknown => ToolRepeatability::Unknown,
    }
}

fn infer_world_scope(tool: &Tool, name: &str) -> ToolWorldScope {
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(|value| value.as_object());
    let property_names = properties
        .map(|props| {
            props
                .keys()
                .map(|key| key.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();

    let description = tool
        .description
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let lowered_name = name.to_ascii_lowercase();

    let collaborative_content_signals = [
        "comment",
        "comments",
        "message",
        "messages",
        "thread",
        "threads",
        "attachment content",
        "conversation",
        "transcript",
        "meeting notes",
        "meeting transcript",
        "markdown with optional comment context",
        "content of a google doc",
        "content of a specific google drive file",
        "full content",
        "plain text body",
        "search messages",
        "full-text search across messages",
        "unread messages",
        "message content",
    ];
    if collaborative_content_signals
        .iter()
        .any(|signal| description.contains(signal) || lowered_name.contains(signal))
    {
        return ToolWorldScope::Open;
    }

    let open_world_signals = [
        "url",
        "uri",
        "domain",
        "http",
        "https",
        "slack",
        "web search",
        "search the web",
        "specific webpage",
        "webpage",
        "website",
        "remote url",
        "public url",
        "external",
    ];

    let closed_world_signals = [
        "figma",
        "slide",
        "nodeid",
        "document_id",
        "spreadsheet_id",
        "presentation_id",
        "script_id",
        "collectionid",
        "variableid",
        "message_id",
        "thread_id",
        "channel_id",
        "chat_id",
        "calendar_id",
        "event_id",
        "file_id",
        "attachment_id",
        "user_google_email",
        "current page",
        "desktop bridge",
        "plugin context",
        "google doc",
        "google drive",
        "google sheet",
        "presentation",
        "workspace",
    ];
    if closed_world_signals.iter().any(|signal| {
        property_names.contains(signal)
            || description.contains(signal)
            || lowered_name.contains(signal)
    }) {
        return ToolWorldScope::Closed;
    }

    if open_world_signals
        .iter()
        .any(|signal| property_names.contains(signal) || description.contains(signal))
    {
        return ToolWorldScope::Open;
    }

    ToolWorldScope::Unknown
}

fn infer_task_support(tool: &Tool, name: &str, semantics: ToolSemantics) -> ToolTaskSupportMode {
    if let Some(task_support) = tool
        .execution
        .as_ref()
        .and_then(|execution| execution.task_support)
    {
        return match task_support {
            TaskSupport::Forbidden => ToolTaskSupportMode::Forbidden,
            TaskSupport::Optional => ToolTaskSupportMode::Optional,
            TaskSupport::Required => ToolTaskSupportMode::Required,
        };
    }

    let lowered_name = name.to_ascii_lowercase();
    let description = tool
        .description
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let long_running_signals = [
        "watch",
        "stream",
        "tail",
        "deep_researcher_start",
        "takes 15 seconds",
        "takes 15 seconds to 2 minutes",
        "long-running",
        "background",
        "while user tests manually",
    ];

    if long_running_signals
        .iter()
        .any(|signal| lowered_name.contains(signal) || description.contains(signal))
    {
        return match semantics {
            ToolSemantics::ReadOnly => ToolTaskSupportMode::Optional,
            ToolSemantics::Mutating | ToolSemantics::Destructive => ToolTaskSupportMode::Required,
            ToolSemantics::Unknown => ToolTaskSupportMode::Optional,
        };
    }

    ToolTaskSupportMode::Unknown
}

/// Normalize annotation hints from a tool name.
///
/// For high-confidence semantic prefixes, `plug` prefers the tool-name-derived
/// contract over upstream-provided hints so clients don't misclassify obviously
/// read-only or destructive tools.
pub fn normalize_annotations(tool: &mut Tool, name: &str) {
    let profile = ToolProfile::infer(tool, name);

    let annotations = tool.annotations.get_or_insert_with(Default::default);
    match profile.semantics {
        ToolSemantics::ReadOnly => {
            annotations.read_only_hint = Some(true);
            annotations.destructive_hint = Some(false);
        }
        ToolSemantics::Mutating => {
            annotations.read_only_hint = Some(false);
            annotations.destructive_hint = Some(false);
        }
        ToolSemantics::Destructive => {
            annotations.read_only_hint = Some(false);
            annotations.destructive_hint = Some(true);
        }
        ToolSemantics::Unknown => {}
    }

    match profile.repeatability {
        ToolRepeatability::Idempotent => annotations.idempotent_hint = Some(true),
        ToolRepeatability::NonIdempotent => annotations.idempotent_hint = Some(false),
        ToolRepeatability::Unknown => {}
    }

    match profile.world_scope {
        ToolWorldScope::Closed => annotations.open_world_hint = Some(false),
        ToolWorldScope::Open => annotations.open_world_hint = Some(true),
        ToolWorldScope::Unknown => {}
    }

    match profile.task_support {
        ToolTaskSupportMode::Forbidden => {
            tool.execution
                .get_or_insert_with(ToolExecution::default)
                .task_support = Some(TaskSupport::Forbidden);
        }
        ToolTaskSupportMode::Optional => {
            tool.execution
                .get_or_insert_with(ToolExecution::default)
                .task_support = Some(TaskSupport::Optional);
        }
        ToolTaskSupportMode::Required => {
            tool.execution
                .get_or_insert_with(ToolExecution::default)
                .task_support = Some(TaskSupport::Required);
        }
        ToolTaskSupportMode::Unknown => {}
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
    use rmcp::model::Tool;
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
        for name in [
            "get_items",
            "list_items",
            "search_items",
            "read_items",
            "fetch_items",
            "watch_items",
            "reload_plugin",
            "query_items",
            "figjam_get_board_contents",
            "channels_list",
            "conversations_history",
        ] {
            let mut tool = make_tool(name);
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
        for name in [
            "create_items",
            "add_items",
            "insert_items",
            "set_items",
            "update_items",
            "write_items",
            "modify_items",
            "send_items",
            "manage_items",
            "conversations_add_message",
            "batch_update_variables",
        ] {
            let mut tool = make_tool(name);
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
        assert_eq!(ann.read_only_hint, Some(true)); // normalized from get_ semantics
        assert_eq!(ann.title.as_deref(), Some("Custom Title")); // not overridden
    }

    #[test]
    fn wrong_upstream_destructive_hint_is_overridden_for_read_tool() {
        let mut tool: Tool = serde_json::from_value(json!({
            "name": "search_messages",
            "inputSchema": { "type": "object" },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true
            }
        }))
        .unwrap();

        normalize_annotations(&mut tool, "search_messages");
        let ann = tool.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(true));
        assert_eq!(ann.destructive_hint, Some(false));
    }

    #[test]
    fn wrong_upstream_read_only_hint_is_overridden_for_delete_tool() {
        let mut tool: Tool = serde_json::from_value(json!({
            "name": "delete_item",
            "inputSchema": { "type": "object" },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }))
        .unwrap();

        normalize_annotations(&mut tool, "delete_item");
        let ann = tool.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.destructive_hint, Some(true));
    }

    #[test]
    fn wrong_upstream_destructive_hint_is_overridden_for_non_destructive_mutating_tool() {
        let mut tool: Tool = serde_json::from_value(json!({
            "name": "conversations_add_message",
            "inputSchema": { "type": "object" },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true
            }
        }))
        .unwrap();

        normalize_annotations(&mut tool, "conversations_add_message");
        let ann = tool.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.destructive_hint, Some(false));
    }

    #[test]
    fn ambiguous_tools_are_classified_from_description_and_schema() {
        let cases = [
            (
                "company_research",
                "Exa: Company Research",
                "[Deprecated] Research any company to get business information. Returns company information from trusted business sources.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "crawling",
                "Exa: Crawling",
                "Get the full content of a specific webpage. Returns full text content and metadata from the page.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "deep_researcher_start",
                "Exa: Deep Researcher Start",
                "Start an AI research agent that searches, reads, and writes a detailed report. Returns: Research ID.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "clear_console",
                "Figma: Clear Console",
                "Clear the console log buffer. Safely clears the buffer without disrupting the connection.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "ds_dashboard_refresh",
                "Figma: Ds Dashboard Refresh",
                "Refresh dashboard data.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "duplicate_slide",
                "Figma: Duplicate Slide",
                "Duplicate an existing slide. The clone is placed adjacent to the original.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "take_screenshot",
                "Figma: Take Screenshot",
                "Export an image of the current Figma page or specific node via REST API. Returns an image URL.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "execute",
                "Figma: Execute",
                "Execute arbitrary JavaScript in Figma's plugin context. CAUTION: Can modify your document.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "focus_slide",
                "Figma: Focus Slide",
                "Navigate to and focus a specific slide in single-slide view.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "navigate",
                "Figma: Navigate",
                "Navigate browser to a Figma URL and start console monitoring.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "reconnect",
                "Figma: Reconnect",
                "Force a complete reconnection to Figma Desktop.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "reorder_slides",
                "Figma: Reorder Slides",
                "Reorder slides by providing a new 2D array of slide IDs. WARNING: This is a destructive operation.",
                None,
                ToolSemantics::Destructive,
            ),
            (
                "resize_node",
                "Figma: Resize Node",
                "Resize a node to specific dimensions.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "setup_design_tokens",
                "Figma: Setup Design Tokens",
                "Create a complete design token structure in one operation.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "skip_slide",
                "Figma: Skip Slide",
                "Toggle whether a slide is skipped during presentation mode.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "token_browser_refresh",
                "Figma: Token Browser Refresh",
                "Refresh token data.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "run_function",
                "GoogleAppsScript: Run Function",
                "Executes a function in a deployed script.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "start",
                "GoogleAuth: Start",
                "Manually initiate Google OAuth authentication flow.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "import_to_google",
                "GoogleDocs: Import To Google",
                "Imports a file into Google Docs format with automatic conversion.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "debug_table_structure",
                "GoogleDocs: Debug Table Structure",
                "ESSENTIAL DEBUGGING TOOL. Shows you exact table dimensions and current content in each cell.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "copy_file",
                "GoogleDrive: Copy File",
                "Creates a copy of an existing Google Drive file.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "format_range",
                "GoogleSheets: Format Range",
                "Applies formatting to a range: colors, number formats, text wrapping, alignment, and text styling.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "date_time",
                "Krisp: Date Time",
                "Get current date/time or enumerate dates in a range.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "conversations_mark",
                "Slack: Conversations Mark",
                "Mark a channel or DM as read.",
                None,
                ToolSemantics::Mutating,
            ),
            (
                "conversations_replies",
                "Slack: Conversations Replies",
                "Get a thread of messages posted to a conversation.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "autofixer",
                "Svelte: Autofixer",
                "Given a svelte component or module returns a list of suggestions to fix any issues it has.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "playground_link",
                "Svelte: Playground Link",
                "Generates a Playground link given a Svelte code snippet.",
                None,
                ToolSemantics::ReadOnly,
            ),
            (
                "usergroups_me",
                "Slack: Usergroups Me",
                "Manage your own user group membership.",
                Some(vec![
                    "list".to_string(),
                    "join".to_string(),
                    "leave".to_string(),
                ]),
                ToolSemantics::Mutating,
            ),
        ];

        for (name, title, description, actions, expected) in cases {
            assert_eq!(
                classify_tool_semantics_with_metadata(
                    name,
                    Some(title),
                    Some(description),
                    actions.as_deref(),
                ),
                expected,
                "expected {name} to classify as {expected:?}"
            );
        }
    }

    #[test]
    fn repeatability_hints_are_inferred() {
        let mut read_tool = make_tool("search_messages");
        normalize_annotations(&mut read_tool, "search_messages");
        assert_eq!(
            read_tool.annotations.as_ref().unwrap().idempotent_hint,
            Some(true)
        );

        let mut create_tool = make_tool("create_item");
        normalize_annotations(&mut create_tool, "create_item");
        assert_eq!(
            create_tool.annotations.as_ref().unwrap().idempotent_hint,
            Some(false)
        );

        let mut set_tool = make_tool("set_status");
        normalize_annotations(&mut set_tool, "set_status");
        assert_eq!(
            set_tool.annotations.as_ref().unwrap().idempotent_hint,
            Some(true)
        );
    }

    #[test]
    fn world_scope_hints_are_inferred() {
        let mut remote_tool: Tool = serde_json::from_value(json!({
            "name": "search_messages",
            "description": "Search the web for any topic.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                }
            }
        }))
        .unwrap();
        normalize_annotations(&mut remote_tool, "search_messages");
        assert_eq!(
            remote_tool.annotations.as_ref().unwrap().open_world_hint,
            Some(true)
        );

        let mut local_tool: Tool = serde_json::from_value(json!({
            "name": "focus_slide",
            "description": "Navigate to and focus a specific slide in single-slide view.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slideId": {"type": "string"}
                }
            }
        }))
        .unwrap();
        normalize_annotations(&mut local_tool, "focus_slide");
        assert_eq!(
            local_tool.annotations.as_ref().unwrap().open_world_hint,
            Some(false)
        );

        let mut collaborative_tool: Tool = serde_json::from_value(json!({
            "name": "get_thread_content",
            "description": "Retrieves the complete content of a Gmail conversation thread, including all messages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread_id": {"type": "string"}
                }
            }
        }))
        .unwrap();
        normalize_annotations(&mut collaborative_tool, "get_thread_content");
        assert_eq!(
            collaborative_tool
                .annotations
                .as_ref()
                .unwrap()
                .open_world_hint,
            Some(true)
        );

        let mut comments_tool: Tool = serde_json::from_value(json!({
            "name": "get_comments",
            "description": "Get comments on a Figma file. Returns comment threads with author, message, timestamps, and pinned node locations.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "fileUrl": {"type": "string"}
                }
            }
        }))
        .unwrap();
        normalize_annotations(&mut comments_tool, "get_comments");
        assert_eq!(
            comments_tool.annotations.as_ref().unwrap().open_world_hint,
            Some(true)
        );
    }

    #[test]
    fn task_support_is_inferred_for_long_running_tools_without_upstream_execution() {
        let mut tool: Tool = serde_json::from_value(json!({
            "name": "deep_researcher_start",
            "description": "Start an AI research agent that searches, reads, and writes a detailed report. Takes 15 seconds to 2 minutes.",
            "inputSchema": { "type": "object" }
        }))
        .unwrap();

        normalize_annotations(&mut tool, "deep_researcher_start");
        assert_eq!(
            tool.execution
                .as_ref()
                .and_then(|execution| execution.task_support),
            Some(TaskSupport::Required)
        );
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
