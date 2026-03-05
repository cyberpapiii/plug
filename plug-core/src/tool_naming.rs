/// Sanitize a tool name into snake_case for LLM API compatibility.
///
/// Handles hyphens, dots, camelCase, PascalCase, and acronyms.
/// Examples:
/// - `"create-comment"` → `"create_comment"`
/// - `"listProjects"` → `"list_projects"`
/// - `"getHTTPResponse"` → `"get_http_response"`
/// - `"admin.tools.list"` → `"admin_tools_list"`
pub fn sanitize_tool_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);

    // Replace hyphens and dots with underscores, then handle camelCase
    let chars: Vec<char> = name.chars().collect();

    for i in 0..chars.len() {
        let c = chars[i];

        if c == '-' || c == '.' {
            result.push('_');
            continue;
        }

        if c.is_uppercase() {
            // Insert underscore before uppercase if:
            // 1. Not at the start
            // 2. Previous char is lowercase or digit, OR
            // 3. Previous char is uppercase and next char is lowercase (end of acronym)
            if i > 0 {
                let prev = chars[i - 1];
                let next = chars.get(i + 1);

                if prev.is_lowercase() || prev.is_ascii_digit() {
                    result.push('_');
                } else if prev.is_uppercase() {
                    if let Some(&n) = next {
                        if n.is_lowercase() {
                            result.push('_');
                        }
                    }
                }
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }

    // Collapse multiple underscores and trim leading/trailing
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_underscore = true; // treat start as underscore to trim leading
    for c in result.chars() {
        if c == '_' {
            if !prev_underscore {
                collapsed.push('_');
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }

    // Trim trailing underscore
    if collapsed.ends_with('_') {
        collapsed.pop();
    }

    collapsed
}

/// Capitalize the first letter of a server name for use as a wire name prefix.
/// "slack" -> "Slack", "imessage" -> "IMessage", "context7" -> "Context7"
pub fn format_server_prefix(server_name: &str) -> String {
    let mut chars = server_name.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut result = first.to_uppercase().to_string();
            result.extend(chars);
            result
        }
    }
}

/// Generate a human-readable title from a server prefix and tool name.
/// ("Slack", "search_messages") -> "Slack: Search Messages"
/// ("Gmail", "get_messages_content_batch") -> "Gmail: Get Messages Content Batch"
pub fn generate_title(server_prefix: &str, tool_name: &str) -> String {
    let words: Vec<String> = tool_name
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut result = first.to_uppercase().to_string();
                    result.extend(chars);
                    result
                }
            }
        })
        .collect();
    format!("{}: {}", server_prefix, words.join(" "))
}

/// Build the wire name: "{ServerPrefix}{delimiter}{tool_name}"
/// ("Slack", "search_messages", "__") -> "Slack__search_messages"
pub fn build_wire_name(server_prefix: &str, tool_name: &str, delimiter: &str) -> String {
    format!("{}{}{}", server_prefix, delimiter, tool_name)
}


/// Result of classifying a tool with rules.
pub struct ClassifyResult {
    /// The group prefix (e.g. "Gmail", "GoogleDrive").
    pub prefix: String,
    /// The original tool name (unmodified).
    pub name: String,
    /// Keywords to strip from the name to avoid redundancy.
    pub strip_keywords: Vec<String>,
}

/// Classify a tool using config-driven group rules.
/// Returns `Some(ClassifyResult)` if a rule matches, `None` otherwise.
pub fn classify_with_rules(
    tool_name: &str,
    rules: &[crate::config::ToolGroupRule],
) -> Option<ClassifyResult> {
    for rule in rules {
        if rule.contains.iter().any(|kw| tool_name.contains(kw.as_str())) {
            return Some(ClassifyResult {
                prefix: rule.prefix.clone(),
                name: tool_name.to_string(),
                strip_keywords: rule.strip.clone(),
            });
        }
    }
    None
}

/// Strip multiple keywords from a tool name, applying each in order.
pub fn strip_keywords(tool_name: &str, keywords: &[String]) -> String {
    let mut result = tool_name.to_string();
    for kw in keywords {
        let stripped = strip_keyword(&result, kw);
        if stripped != result {
            result = stripped;
            break; // Only strip one keyword to avoid over-stripping
        }
    }
    result
}

/// Strip a matched keyword from a tool name at word boundaries (underscore-delimited).
/// Returns the stripped name, or the original if stripping would produce an empty string.
///
/// The keyword must appear as a complete word segment bounded by underscores or
/// string boundaries. This prevents partial matches (e.g. "sheet" inside "spreadsheets").
///
/// Examples:
/// - ("get_gmail_message_content", "gmail") -> "get_message_content"
/// - ("search_drive_files", "drive") -> "search_files"
/// - ("create_sheet", "sheet") -> "create"
/// - ("get_page", "get_page") -> "get_page" (full match, keep original)
/// - ("list_spreadsheets", "spreadsheet") -> "list_spreadsheets" (no word boundary match)
pub fn strip_keyword(tool_name: &str, keyword: &str) -> String {
    // Don't strip if the keyword IS the entire name
    if tool_name == keyword {
        return tool_name.to_string();
    }

    // Split into underscore-delimited words, filtering out empty segments
    // (handles keywords like "doc_" or "_doc" that have leading/trailing underscores)
    let words: Vec<&str> = tool_name.split('_').filter(|w| !w.is_empty()).collect();
    let kw_words: Vec<&str> = keyword.split('_').filter(|w| !w.is_empty()).collect();
    let kw_len = kw_words.len();

    // Find a contiguous run of words matching the keyword words
    if words.len() >= kw_len {
        for start in 0..=(words.len() - kw_len) {
            if words[start..start + kw_len] == kw_words[..] {
                // Found a match — remove these words
                let mut remaining: Vec<&str> = Vec::new();
                remaining.extend_from_slice(&words[..start]);
                remaining.extend_from_slice(&words[start + kw_len..]);
                let result = remaining.join("_");
                if result.is_empty() {
                    return tool_name.to_string();
                }
                return result;
            }
        }
    }

    // No word-boundary match found — return original
    tool_name.to_string()
}

/// Strip keywords from a title string (display-only, collisions don't matter).
/// Removes the keyword and cleans up spacing.
pub fn strip_keyword_from_title(title_part: &str, keyword: &str) -> String {
    // Title words are space-separated and Title Cased
    // Convert keyword to title case for matching
    let kw_title: String = keyword
        .split('_')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let result = title_part.replace(&kw_title, "");
    // Clean up double spaces and trim
    let cleaned: String = result.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        title_part.to_string()
    } else {
        cleaned
    }
}

/// Returns the built-in tool group rules for the Google Workspace MCP server.
/// Used as defaults when no `tool_groups` config is specified for the `workspace` server.
pub fn default_workspace_rules() -> Vec<crate::config::ToolGroupRule> {
    use crate::config::ToolGroupRule;
    vec![
        ToolGroupRule { prefix: "Gmail".into(), contains: vec!["gmail".into()], strip: vec!["gmail".into()] },
        ToolGroupRule { prefix: "GoogleDrive".into(), contains: vec!["drive".into()], strip: vec!["drive".into()] },
        ToolGroupRule { prefix: "GoogleSheets".into(), contains: vec!["spreadsheet".into(), "sheet".into(), "conditional_formatting".into()], strip: vec!["spreadsheet".into(), "sheet".into()] },
        ToolGroupRule { prefix: "GoogleSlides".into(), contains: vec!["presentation".into(), "page_thumbnail".into(), "get_page".into()], strip: vec!["presentation".into()] },
        ToolGroupRule { prefix: "GoogleDocs".into(), contains: vec!["_doc".into(), "doc_".into(), "document".into(), "paragraph".into(), "table_with_data".into(), "table_structure".into(), "find_and_replace".into()], strip: vec!["doc".into(), "document".into()] },
        ToolGroupRule { prefix: "GoogleCalendar".into(), contains: vec!["event".into(), "calendar".into(), "freebusy".into()], strip: vec!["calendar".into()] },
        ToolGroupRule { prefix: "GoogleContacts".into(), contains: vec!["contact".into()], strip: vec!["contact".into(), "contacts".into()] },
        ToolGroupRule { prefix: "GoogleTasks".into(), contains: vec!["task".into()], strip: vec!["task".into(), "tasks".into()] },
        ToolGroupRule { prefix: "GoogleChat".into(), contains: vec!["spaces".into(), "reaction".into(), "chat_".into(), "send_message".into(), "get_messages".into(), "search_messages".into()], strip: vec!["chat".into()] },
        ToolGroupRule { prefix: "GoogleForms".into(), contains: vec!["form".into()], strip: vec!["form".into()] },
        ToolGroupRule { prefix: "GoogleAppsScript".into(), contains: vec!["script".into(), "deployment".into(), "version".into(), "trigger_code".into(), "publish_settings".into()], strip: vec!["script".into()] },
        ToolGroupRule { prefix: "GoogleAuth".into(), contains: vec!["google_auth".into()], strip: vec!["google_auth".into()] },
        ToolGroupRule { prefix: "GoogleSearch".into(), contains: vec!["search_engine".into(), "search_custom".into()], strip: vec![] },
    ]
}

/// Classify a workspace tool into a Google sub-service using built-in defaults.
/// Returns (&'static str, String) = (sub_service_prefix, original_tool_name)
///
/// The tool name is returned **unmodified** — no keyword stripping.
/// The prefix (e.g. "GoogleSheets") is what provides disambiguation.
pub fn classify_workspace_tool(tool_name: &str) -> (&'static str, String) {
    // Gmail (already a distinct brand, no "Google" prefix needed)
    if tool_name.contains("gmail") {
        return ("Gmail", tool_name.to_string());
    }

    // Google Drive
    if tool_name.contains("drive") {
        return ("GoogleDrive", tool_name.to_string());
    }

    // Google Sheets
    if tool_name.contains("sheet")
        || tool_name.contains("spreadsheet")
        || tool_name.contains("conditional_formatting")
    {
        return ("GoogleSheets", tool_name.to_string());
    }

    // Google Slides
    if tool_name.contains("presentation")
        || tool_name.contains("page_thumbnail")
        || tool_name == "get_page"
    {
        return ("GoogleSlides", tool_name.to_string());
    }

    // Google Docs
    if tool_name.contains("doc_")
        || tool_name.contains("docs_")
        || tool_name.ends_with("_doc")
        || tool_name.ends_with("_docs")
        || tool_name.contains("document")
        || tool_name.contains("paragraph")
        || tool_name == "import_to_google_doc"
        || tool_name.contains("table_with_data")
        || tool_name.contains("table_structure")
        || tool_name.contains("find_and_replace")
        || tool_name == "search_docs"
        || tool_name == "batch_update_doc"
        || tool_name == "create_doc"
    {
        return ("GoogleDocs", tool_name.to_string());
    }

    // Google Calendar
    if tool_name.contains("event")
        || tool_name.contains("calendar")
        || tool_name.contains("freebusy")
    {
        return ("GoogleCalendar", tool_name.to_string());
    }

    // Google Contacts
    if tool_name.contains("contact") {
        return ("GoogleContacts", tool_name.to_string());
    }

    // Google Tasks
    if tool_name.contains("task") {
        return ("GoogleTasks", tool_name.to_string());
    }

    // Google Chat
    if tool_name == "send_message"
        || tool_name == "get_messages"
        || tool_name == "search_messages"
        || tool_name.contains("spaces")
        || tool_name.contains("reaction")
        || tool_name.contains("chat_")
    {
        return ("GoogleChat", tool_name.to_string());
    }

    // Google Forms
    if tool_name.contains("form") {
        return ("GoogleForms", tool_name.to_string());
    }

    // Google Apps Script
    if tool_name.contains("script")
        || tool_name.contains("deployment")
        || tool_name.contains("version")
        || tool_name.contains("trigger_code")
        || tool_name.contains("publish_settings")
    {
        return ("GoogleAppsScript", tool_name.to_string());
    }

    // Google Auth
    if tool_name.contains("google_auth") {
        return ("GoogleAuth", tool_name.to_string());
    }

    // Google Search (Programmable Search Engine)
    if tool_name.contains("search_engine") || tool_name == "search_custom" {
        return ("GoogleSearch", tool_name.to_string());
    }

    // Fallback
    ("GoogleWorkspace", tool_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_hyphens_to_underscores() {
        assert_eq!(sanitize_tool_name("create-comment"), "create_comment");
        assert_eq!(
            sanitize_tool_name("query-database-view"),
            "query_database_view"
        );
    }

    #[test]
    fn sanitize_camel_case_to_snake() {
        assert_eq!(sanitize_tool_name("listProjects"), "list_projects");
        assert_eq!(sanitize_tool_name("whoAmI"), "who_am_i");
        assert_eq!(
            sanitize_tool_name("getHTTPResponse"),
            "get_http_response"
        );
    }

    #[test]
    fn sanitize_dots_to_underscores() {
        assert_eq!(
            sanitize_tool_name("admin.tools.list"),
            "admin_tools_list"
        );
    }

    #[test]
    fn sanitize_already_snake_case_unchanged() {
        assert_eq!(sanitize_tool_name("get_events"), "get_events");
        assert_eq!(sanitize_tool_name("search_messages"), "search_messages");
    }

    #[test]
    fn sanitize_preserves_numbers() {
        assert_eq!(sanitize_tool_name("DATA_EXPORT_v2"), "data_export_v2");
    }

    #[test]
    fn sanitize_mixed() {
        assert_eq!(
            sanitize_tool_name("fetch-graph-data"),
            "fetch_graph_data"
        );
        assert_eq!(
            sanitize_tool_name("get-documentation"),
            "get_documentation"
        );
    }

    #[test]
    fn capitalize_server_name() {
        assert_eq!(format_server_prefix("slack"), "Slack");
        assert_eq!(format_server_prefix("imessage"), "Imessage");
        assert_eq!(format_server_prefix("context7"), "Context7");
        assert_eq!(format_server_prefix("supabase"), "Supabase");
        assert_eq!(format_server_prefix("supermemory"), "Supermemory");
        assert_eq!(format_server_prefix("exa"), "Exa");
        assert_eq!(format_server_prefix("Gmail"), "Gmail"); // already capitalized
    }

    #[test]
    fn generate_title_basic() {
        assert_eq!(
            generate_title("Slack", "search_messages"),
            "Slack: Search Messages"
        );
        assert_eq!(
            generate_title("Gmail", "get_messages_content_batch"),
            "Gmail: Get Messages Content Batch"
        );
        assert_eq!(generate_title("Notion", "search"), "Notion: Search");
        assert_eq!(generate_title("Calendar", "list"), "Calendar: List");
    }

    #[test]
    fn build_wire_name_basic() {
        assert_eq!(
            build_wire_name("Slack", "search_messages", "__"),
            "Slack__search_messages"
        );
        assert_eq!(
            build_wire_name("Gmail", "get_messages_content_batch", "__"),
            "Gmail__get_messages_content_batch"
        );
    }

    // ---- classify_workspace_tool tests ----
    // Tool names are returned unmodified; only the prefix changes.

    #[test]
    fn classify_gmail_tools() {
        assert_eq!(
            classify_workspace_tool("search_gmail_messages"),
            ("Gmail", "search_gmail_messages".to_string())
        );
        assert_eq!(
            classify_workspace_tool("draft_gmail_message"),
            ("Gmail", "draft_gmail_message".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_messages_content_batch"),
            ("Gmail", "get_gmail_messages_content_batch".to_string())
        );
    }

    #[test]
    fn classify_drive_tools() {
        assert_eq!(
            classify_workspace_tool("search_drive_files"),
            ("GoogleDrive", "search_drive_files".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_drive_folder"),
            ("GoogleDrive", "create_drive_folder".to_string())
        );
    }

    #[test]
    fn classify_sheets_tools() {
        // No more collision — both keep their original names
        assert_eq!(
            classify_workspace_tool("create_sheet"),
            ("GoogleSheets", "create_sheet".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_spreadsheet"),
            ("GoogleSheets", "create_spreadsheet".to_string())
        );
        assert_eq!(
            classify_workspace_tool("format_sheet_range"),
            ("GoogleSheets", "format_sheet_range".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_conditional_formatting"),
            ("GoogleSheets", "manage_conditional_formatting".to_string())
        );
    }

    #[test]
    fn classify_slides_tools() {
        assert_eq!(
            classify_workspace_tool("batch_update_presentation"),
            ("GoogleSlides", "batch_update_presentation".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_presentation"),
            ("GoogleSlides", "get_presentation".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_page"),
            ("GoogleSlides", "get_page".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_page_thumbnail"),
            ("GoogleSlides", "get_page_thumbnail".to_string())
        );
    }

    #[test]
    fn classify_docs_tools() {
        assert_eq!(
            classify_workspace_tool("get_doc_content"),
            ("GoogleDocs", "get_doc_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_doc"),
            ("GoogleDocs", "create_doc".to_string())
        );
        assert_eq!(
            classify_workspace_tool("import_to_google_doc"),
            ("GoogleDocs", "import_to_google_doc".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_paragraph_style"),
            ("GoogleDocs", "update_paragraph_style".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_table_with_data"),
            ("GoogleDocs", "create_table_with_data".to_string())
        );
    }

    #[test]
    fn classify_calendar_tools() {
        assert_eq!(
            classify_workspace_tool("get_events"),
            ("GoogleCalendar", "get_events".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_calendars"),
            ("GoogleCalendar", "list_calendars".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_event"),
            ("GoogleCalendar", "manage_event".to_string())
        );
        assert_eq!(
            classify_workspace_tool("query_freebusy"),
            ("GoogleCalendar", "query_freebusy".to_string())
        );
    }

    #[test]
    fn classify_contacts_tools() {
        assert_eq!(
            classify_workspace_tool("get_contact"),
            ("GoogleContacts", "get_contact".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_contacts"),
            ("GoogleContacts", "search_contacts".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_contacts_batch"),
            ("GoogleContacts", "manage_contacts_batch".to_string())
        );
    }

    #[test]
    fn classify_tasks_tools() {
        assert_eq!(
            classify_workspace_tool("get_task"),
            ("GoogleTasks", "get_task".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_tasks"),
            ("GoogleTasks", "list_tasks".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_task_list"),
            ("GoogleTasks", "manage_task_list".to_string())
        );
    }

    #[test]
    fn classify_chat_tools() {
        assert_eq!(
            classify_workspace_tool("send_message"),
            ("GoogleChat", "send_message".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_messages"),
            ("GoogleChat", "get_messages".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_spaces"),
            ("GoogleChat", "list_spaces".to_string())
        );
        assert_eq!(
            classify_workspace_tool("download_chat_attachment"),
            ("GoogleChat", "download_chat_attachment".to_string())
        );
    }

    #[test]
    fn classify_forms_tools() {
        assert_eq!(
            classify_workspace_tool("create_form"),
            ("GoogleForms", "create_form".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_form_response"),
            ("GoogleForms", "get_form_response".to_string())
        );
        assert_eq!(
            classify_workspace_tool("batch_update_form"),
            ("GoogleForms", "batch_update_form".to_string())
        );
    }

    #[test]
    fn classify_scripts_tools() {
        assert_eq!(
            classify_workspace_tool("create_script_project"),
            ("GoogleAppsScript", "create_script_project".to_string())
        );
        assert_eq!(
            classify_workspace_tool("run_script_function"),
            ("GoogleAppsScript", "run_script_function".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_version"),
            ("GoogleAppsScript", "create_version".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_deployments"),
            ("GoogleAppsScript", "list_deployments".to_string())
        );
        assert_eq!(
            classify_workspace_tool("set_publish_settings"),
            ("GoogleAppsScript", "set_publish_settings".to_string())
        );
    }

    #[test]
    fn classify_auth_tool() {
        assert_eq!(
            classify_workspace_tool("start_google_auth"),
            ("GoogleAuth", "start_google_auth".to_string())
        );
    }

    #[test]
    fn classify_search_tools() {
        assert_eq!(
            classify_workspace_tool("get_search_engine_info"),
            ("GoogleSearch", "get_search_engine_info".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_custom"),
            ("GoogleSearch", "search_custom".to_string())
        );
    }

    #[test]
    fn classify_fallback_tools() {
        assert_eq!(
            classify_workspace_tool("some_new_tool"),
            ("GoogleWorkspace", "some_new_tool".to_string())
        );
    }

    #[test]
    fn classify_with_rules_matches_defaults() {
        let rules = default_workspace_rules();
        let test_cases = vec![
            ("search_gmail_messages", "Gmail"),
            ("create_drive_folder", "GoogleDrive"),
            ("create_sheet", "GoogleSheets"),
            ("create_spreadsheet", "GoogleSheets"),
            ("get_presentation", "GoogleSlides"),
            ("get_doc_content", "GoogleDocs"),
            ("get_events", "GoogleCalendar"),
            ("get_contact", "GoogleContacts"),
            ("list_tasks", "GoogleTasks"),
            ("send_message", "GoogleChat"),
            ("create_form", "GoogleForms"),
            ("run_script_function", "GoogleAppsScript"),
            ("start_google_auth", "GoogleAuth"),
            ("search_custom", "GoogleSearch"),
        ];
        for (tool, expected_prefix) in test_cases {
            let result = classify_with_rules(tool, &rules);
            assert!(result.is_some(), "rule should match: {tool}");
            let r = result.unwrap();
            assert_eq!(r.prefix, expected_prefix, "wrong prefix for {tool}");
            assert_eq!(r.name, tool, "tool name should be unmodified for {tool}");
        }
    }

    #[test]
    fn classify_with_rules_no_match_returns_none() {
        let rules = default_workspace_rules();
        assert!(classify_with_rules("some_new_tool", &rules).is_none());
    }

    #[test]
    fn classify_with_custom_rules() {
        use crate::config::ToolGroupRule;
        let rules = vec![
            ToolGroupRule { prefix: "Outlook".into(), contains: vec!["mail".into(), "outlook".into()], strip: vec!["mail".into()] },
            ToolGroupRule { prefix: "Teams".into(), contains: vec!["teams".into(), "channel".into()], strip: vec!["teams".into()] },
        ];
        let r = classify_with_rules("send_mail", &rules).unwrap();
        assert_eq!(r.prefix, "Outlook");
        assert_eq!(r.name, "send_mail");
        assert_eq!(r.strip_keywords, vec!["mail"]);

        let r = classify_with_rules("list_channels", &rules).unwrap();
        assert_eq!(r.prefix, "Teams");
        assert_eq!(r.name, "list_channels");
        assert_eq!(r.strip_keywords, vec!["teams"]); // strip is "teams", not "channel"

        assert!(classify_with_rules("unknown_tool", &rules).is_none());
    }

    #[test]
    fn strip_keywords_applies_strip_list() {
        // Gmail: strip "gmail"
        assert_eq!(strip_keywords("get_gmail_message_content", &["gmail".into()]), "get_message_content");
        // GoogleCalendar: strip "calendar" but NOT "event"
        assert_eq!(strip_keywords("list_calendars", &["calendar".into()]), "list_calendars"); // "calendars" != "calendar" at word boundary
        assert_eq!(strip_keywords("manage_event", &["calendar".into()]), "manage_event"); // no match
        // GoogleAppsScript: strip "script" but NOT "version"/"deployment"
        assert_eq!(strip_keywords("run_script_function", &["script".into()]), "run_function");
        assert_eq!(strip_keywords("create_version", &["script".into()]), "create_version"); // no match
    }

    // ---- strip_keyword tests ----

    #[test]
    fn strip_keyword_prefix() {
        assert_eq!(strip_keyword("gmail_message_content", "gmail"), "message_content");
        assert_eq!(strip_keyword("drive_file_content", "drive"), "file_content");
    }

    #[test]
    fn strip_keyword_suffix() {
        assert_eq!(strip_keyword("create_sheet", "sheet"), "create");
        assert_eq!(strip_keyword("batch_update_form", "form"), "batch_update");
        assert_eq!(strip_keyword("get_presentation", "presentation"), "get");
    }

    #[test]
    fn strip_keyword_infix() {
        assert_eq!(strip_keyword("get_gmail_message_content", "gmail"), "get_message_content");
        assert_eq!(strip_keyword("search_drive_files", "drive"), "search_files");
    }

    #[test]
    fn strip_keyword_full_match_keeps_original() {
        assert_eq!(strip_keyword("get_page", "get_page"), "get_page");
        assert_eq!(strip_keyword("search_custom", "search_custom"), "search_custom");
    }

    #[test]
    fn strip_keyword_no_word_boundary_keeps_original() {
        // "spreadsheet" doesn't match "spreadsheets" at word boundary
        assert_eq!(strip_keyword("list_spreadsheets", "spreadsheet"), "list_spreadsheets");
        // "script" doesn't match at word boundary inside "create_version"
        assert_eq!(strip_keyword("create_version", "script"), "create_version");
    }

    #[test]
    fn strip_keyword_multi_word() {
        // Multi-word keywords like "google_auth"
        assert_eq!(strip_keyword("start_google_auth", "google_auth"), "start");
        // "doc_" as keyword — only the "doc" word
        assert_eq!(strip_keyword("get_doc_content", "doc"), "get_content");
    }

    #[test]
    fn enrichment_works_after_sanitization() {
        // Simulate the full pipeline: original kebab-case -> sanitize -> enrich
        use crate::enrichment::enrich_tool;
        use rmcp::model::Tool;
        use std::borrow::Cow;
        use std::sync::Arc;

        // Original name from Notion: "get-comments" (hyphens)
        let original = "get-comments";
        let sanitized = sanitize_tool_name(original);
        assert_eq!(sanitized, "get_comments");

        // Create tool with sanitized name (as refresh_tools does before enrichment)
        let mut tool = Tool::new(
            Cow::Owned(sanitized),
            Cow::Borrowed("Get comments"),
            Arc::new(serde_json::Map::new()),
        );

        // Enrich should detect get_* pattern
        enrich_tool(&mut tool);
        assert_eq!(
            tool.annotations.as_ref().unwrap().read_only_hint,
            Some(true),
            "get_comments should be detected as read-only after sanitization"
        );
    }
}
