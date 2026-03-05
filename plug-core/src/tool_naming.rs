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

/// Clean up a tool name after stripping: trim leading/trailing underscores,
/// collapse double underscores. If result is empty, return the original.
fn clean_stripped(name: &str, original: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_underscore = true; // trim leading
    for c in name.chars() {
        if c == '_' {
            if !prev_underscore {
                out.push('_');
            }
            prev_underscore = true;
        } else {
            out.push(c);
            prev_underscore = false;
        }
    }
    if out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        original.to_string()
    } else {
        out
    }
}

/// Strip a suffix word from the end of a name (including any preceding underscore).
fn strip_suffix_word(name: &str, suffix: &str) -> String {
    if let Some(before) = name.strip_suffix(suffix) {
        before.to_string()
    } else {
        name.to_string()
    }
}

/// Classify a workspace tool into a sub-service and strip the service keyword.
/// Returns (&'static str, String) = (sub_service_name, cleaned_tool_name)
pub fn classify_workspace_tool(tool_name: &str) -> (&'static str, String) {
    // Gmail
    if tool_name.contains("gmail") {
        let stripped = tool_name.replace("gmail_", "");
        return ("Gmail", clean_stripped(&stripped, tool_name));
    }

    // Drive
    if tool_name.contains("drive") {
        let stripped = tool_name.replace("drive_", "");
        return ("Drive", clean_stripped(&stripped, tool_name));
    }

    // Sheets
    if tool_name.contains("sheet")
        || tool_name.contains("spreadsheet")
        || tool_name.contains("conditional_formatting")
    {
        let stripped = tool_name
            .replace("spreadsheet_", "")
            .replace("sheet_", "");
        // Handle trailing _sheet (e.g. "create_sheet")
        let stripped = strip_suffix_word(&stripped, "_sheet");
        // Handle trailing _spreadsheet (e.g. "create_spreadsheet")
        let stripped = strip_suffix_word(&stripped, "_spreadsheet");
        return ("Sheets", clean_stripped(&stripped, tool_name));
    }

    // Slides
    if tool_name.contains("presentation")
        || tool_name.contains("page_thumbnail")
        || tool_name == "get_page"
    {
        let stripped = tool_name.replace("presentation_", "");
        // Handle trailing _presentation (e.g. "batch_update_presentation", "get_presentation")
        let stripped = strip_suffix_word(&stripped, "_presentation");
        return ("Slides", clean_stripped(&stripped, tool_name));
    }

    // Docs
    if tool_name.contains("doc_")
        || tool_name.contains("docs_")
        || tool_name.ends_with("_doc")
        || tool_name.ends_with("_docs")
        || tool_name.contains("document")
        || tool_name.contains("paragraph_style")
        || tool_name == "import_to_google_doc"
        || tool_name.contains("table_with_data")
        || tool_name.contains("table_structure")
        || tool_name.contains("find_and_replace")
        || tool_name == "search_docs"
        || tool_name == "batch_update_doc"
        || tool_name == "create_doc"
    {
        let stripped = tool_name
            .replace("document_", "")
            .replace("docs_", "")
            .replace("doc_", "");
        let stripped = strip_suffix_word(&stripped, "_doc");
        let stripped = strip_suffix_word(&stripped, "_docs");
        return ("Docs", clean_stripped(&stripped, tool_name));
    }

    // Calendar
    if tool_name.contains("event")
        || tool_name.contains("calendar")
        || tool_name.contains("freebusy")
    {
        let stripped = tool_name.replace("calendars", "").replace("calendar", "");
        return ("Calendar", clean_stripped(&stripped, tool_name));
    }

    // Contacts
    if tool_name.contains("contact") {
        let stripped = tool_name
            .replace("contacts_", "")
            .replace("contact_", "");
        let stripped = strip_suffix_word(&stripped, "_contact");
        let stripped = strip_suffix_word(&stripped, "_contacts");
        return ("Contacts", clean_stripped(&stripped, tool_name));
    }

    // Tasks
    if tool_name.contains("task") {
        let stripped = tool_name.replace("task_", "");
        let stripped = strip_suffix_word(&stripped, "_tasks");
        let stripped = strip_suffix_word(&stripped, "_task");
        return ("Tasks", clean_stripped(&stripped, tool_name));
    }

    // Chat
    if tool_name == "send_message"
        || tool_name == "get_messages"
        || tool_name == "search_messages"
        || tool_name.contains("spaces")
        || tool_name.contains("reaction")
        || tool_name.contains("chat_")
    {
        let stripped = tool_name.replace("chat_", "");
        return ("Chat", clean_stripped(&stripped, tool_name));
    }

    // Forms
    if tool_name.contains("form") {
        let stripped = tool_name.replace("form_", "");
        return ("Forms", clean_stripped(&stripped, tool_name));
    }

    // Scripts
    if tool_name.contains("script")
        || tool_name.contains("deployment")
        || tool_name.contains("version")
        || tool_name.contains("trigger_code")
        || tool_name.contains("publish_settings")
    {
        let stripped = tool_name.replace("script_", "");
        return ("Scripts", clean_stripped(&stripped, tool_name));
    }

    // Auth
    if tool_name.contains("google_auth") {
        let stripped = tool_name.replace("google_auth", "");
        return ("Auth", clean_stripped(&stripped, tool_name));
    }

    // Fallback
    ("Workspace", tool_name.to_string())
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

    #[test]
    fn classify_gmail_tools() {
        assert_eq!(
            classify_workspace_tool("search_gmail_messages"),
            ("Gmail", "search_messages".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_message_content"),
            ("Gmail", "get_message_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_messages_content_batch"),
            ("Gmail", "get_messages_content_batch".to_string())
        );
        assert_eq!(
            classify_workspace_tool("draft_gmail_message"),
            ("Gmail", "draft_message".to_string())
        );
        assert_eq!(
            classify_workspace_tool("send_gmail_message"),
            ("Gmail", "send_message".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_gmail_filters"),
            ("Gmail", "list_filters".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_gmail_labels"),
            ("Gmail", "list_labels".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_gmail_filter"),
            ("Gmail", "manage_filter".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_gmail_label"),
            ("Gmail", "manage_label".to_string())
        );
        assert_eq!(
            classify_workspace_tool("modify_gmail_message_labels"),
            ("Gmail", "modify_message_labels".to_string())
        );
        assert_eq!(
            classify_workspace_tool("batch_modify_gmail_message_labels"),
            ("Gmail", "batch_modify_message_labels".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_attachment_content"),
            ("Gmail", "get_attachment_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_thread_content"),
            ("Gmail", "get_thread_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_gmail_threads_content_batch"),
            ("Gmail", "get_threads_content_batch".to_string())
        );
    }

    #[test]
    fn classify_drive_tools() {
        assert_eq!(
            classify_workspace_tool("search_drive_files"),
            ("Drive", "search_files".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_drive_file_content"),
            ("Drive", "get_file_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_drive_folder"),
            ("Drive", "create_folder".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_drive_items"),
            ("Drive", "list_items".to_string())
        );
        assert_eq!(
            classify_workspace_tool("check_drive_file_public_access"),
            ("Drive", "check_file_public_access".to_string())
        );
        assert_eq!(
            classify_workspace_tool("copy_drive_file"),
            ("Drive", "copy_file".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_drive_file"),
            ("Drive", "create_file".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_drive_file_download_url"),
            ("Drive", "get_file_download_url".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_drive_file_permissions"),
            ("Drive", "get_file_permissions".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_drive_shareable_link"),
            ("Drive", "get_shareable_link".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_drive_access"),
            ("Drive", "manage_access".to_string())
        );
        assert_eq!(
            classify_workspace_tool("set_drive_file_permissions"),
            ("Drive", "set_file_permissions".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_drive_file"),
            ("Drive", "update_file".to_string())
        );
    }

    #[test]
    fn classify_sheets_tools() {
        assert_eq!(
            classify_workspace_tool("create_sheet"),
            ("Sheets", "create".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_spreadsheet"),
            ("Sheets", "create".to_string())
        );
        assert_eq!(
            classify_workspace_tool("format_sheet_range"),
            ("Sheets", "format_range".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_spreadsheet_info"),
            ("Sheets", "get_info".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_spreadsheets"),
            ("Sheets", "list_spreadsheets".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_spreadsheet_comments"),
            ("Sheets", "list_comments".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_conditional_formatting"),
            ("Sheets", "manage_conditional_formatting".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_spreadsheet_comment"),
            ("Sheets", "manage_comment".to_string())
        );
        assert_eq!(
            classify_workspace_tool("modify_sheet_values"),
            ("Sheets", "modify_values".to_string())
        );
        assert_eq!(
            classify_workspace_tool("read_sheet_values"),
            ("Sheets", "read_values".to_string())
        );
    }

    #[test]
    fn classify_slides_tools() {
        assert_eq!(
            classify_workspace_tool("batch_update_presentation"),
            ("Slides", "batch_update".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_presentation"),
            ("Slides", "create".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_presentation"),
            ("Slides", "get".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_page"),
            ("Slides", "get_page".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_page_thumbnail"),
            ("Slides", "get_page_thumbnail".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_presentation_comments"),
            ("Slides", "list_comments".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_presentation_comment"),
            ("Slides", "manage_comment".to_string())
        );
    }

    #[test]
    fn classify_docs_tools() {
        assert_eq!(
            classify_workspace_tool("get_doc_content"),
            ("Docs", "get_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_doc_as_markdown"),
            ("Docs", "get_as_markdown".to_string())
        );
        assert_eq!(
            classify_workspace_tool("batch_update_doc"),
            ("Docs", "batch_update".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_doc"),
            ("Docs", "create".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_document_comments"),
            ("Docs", "list_comments".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_document_comment"),
            ("Docs", "manage_comment".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_paragraph_style"),
            ("Docs", "update_paragraph_style".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_table_with_data"),
            ("Docs", "create_table_with_data".to_string())
        );
        assert_eq!(
            classify_workspace_tool("debug_table_structure"),
            ("Docs", "debug_table_structure".to_string())
        );
        assert_eq!(
            classify_workspace_tool("find_and_replace_doc"),
            ("Docs", "find_and_replace".to_string())
        );
        assert_eq!(
            classify_workspace_tool("import_to_google_doc"),
            ("Docs", "import_to_google".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_docs"),
            ("Docs", "search".to_string())
        );
        assert_eq!(
            classify_workspace_tool("delete_doc_tab"),
            ("Docs", "delete_tab".to_string())
        );
        assert_eq!(
            classify_workspace_tool("export_doc_to_pdf"),
            ("Docs", "export_to_pdf".to_string())
        );
        assert_eq!(
            classify_workspace_tool("insert_doc_elements"),
            ("Docs", "insert_elements".to_string())
        );
        assert_eq!(
            classify_workspace_tool("insert_doc_image"),
            ("Docs", "insert_image".to_string())
        );
        assert_eq!(
            classify_workspace_tool("insert_doc_tab"),
            ("Docs", "insert_tab".to_string())
        );
        assert_eq!(
            classify_workspace_tool("inspect_doc_structure"),
            ("Docs", "inspect_structure".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_docs_in_folder"),
            ("Docs", "list_in_folder".to_string())
        );
        assert_eq!(
            classify_workspace_tool("modify_doc_text"),
            ("Docs", "modify_text".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_doc_headers_footers"),
            ("Docs", "update_headers_footers".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_doc_tab"),
            ("Docs", "update_tab".to_string())
        );
    }

    #[test]
    fn classify_calendar_tools() {
        assert_eq!(
            classify_workspace_tool("get_events"),
            ("Calendar", "get_events".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_calendars"),
            ("Calendar", "list".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_event"),
            ("Calendar", "manage_event".to_string())
        );
        assert_eq!(
            classify_workspace_tool("query_freebusy"),
            ("Calendar", "query_freebusy".to_string())
        );
    }

    #[test]
    fn classify_contacts_tools() {
        assert_eq!(
            classify_workspace_tool("get_contact"),
            ("Contacts", "get".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_contact_group"),
            ("Contacts", "get_group".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_contact_groups"),
            ("Contacts", "list_groups".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_contacts"),
            ("Contacts", "list".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_contact"),
            ("Contacts", "manage".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_contact_group"),
            ("Contacts", "manage_group".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_contacts_batch"),
            ("Contacts", "manage_batch".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_contacts"),
            ("Contacts", "search".to_string())
        );
    }

    #[test]
    fn classify_tasks_tools() {
        assert_eq!(
            classify_workspace_tool("get_task"),
            ("Tasks", "get".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_task_list"),
            ("Tasks", "get_list".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_task_lists"),
            ("Tasks", "list_lists".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_tasks"),
            ("Tasks", "list".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_task"),
            ("Tasks", "manage".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_task_list"),
            ("Tasks", "manage_list".to_string())
        );
    }

    #[test]
    fn classify_chat_tools() {
        assert_eq!(
            classify_workspace_tool("send_message"),
            ("Chat", "send_message".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_messages"),
            ("Chat", "get_messages".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_messages"),
            ("Chat", "search_messages".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_spaces"),
            ("Chat", "list_spaces".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_reaction"),
            ("Chat", "create_reaction".to_string())
        );
        assert_eq!(
            classify_workspace_tool("download_chat_attachment"),
            ("Chat", "download_attachment".to_string())
        );
    }

    #[test]
    fn classify_forms_tools() {
        assert_eq!(
            classify_workspace_tool("get_form"),
            ("Forms", "get_form".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_form_response"),
            ("Forms", "get_response".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_form_responses"),
            ("Forms", "list_responses".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_form"),
            ("Forms", "create_form".to_string())
        );
        assert_eq!(
            classify_workspace_tool("batch_update_form"),
            ("Forms", "batch_update_form".to_string())
        );
    }

    #[test]
    fn classify_scripts_tools() {
        assert_eq!(
            classify_workspace_tool("create_script_project"),
            ("Scripts", "create_project".to_string())
        );
        assert_eq!(
            classify_workspace_tool("run_script_function"),
            ("Scripts", "run_function".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_script_content"),
            ("Scripts", "get_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_script_metrics"),
            ("Scripts", "get_metrics".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_script_project"),
            ("Scripts", "get_project".to_string())
        );
        assert_eq!(
            classify_workspace_tool("delete_script_project"),
            ("Scripts", "delete_project".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_script_processes"),
            ("Scripts", "list_processes".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_script_projects"),
            ("Scripts", "list_projects".to_string())
        );
        assert_eq!(
            classify_workspace_tool("update_script_content"),
            ("Scripts", "update_content".to_string())
        );
        assert_eq!(
            classify_workspace_tool("create_version"),
            ("Scripts", "create_version".to_string())
        );
        assert_eq!(
            classify_workspace_tool("get_version"),
            ("Scripts", "get_version".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_versions"),
            ("Scripts", "list_versions".to_string())
        );
        assert_eq!(
            classify_workspace_tool("list_deployments"),
            ("Scripts", "list_deployments".to_string())
        );
        assert_eq!(
            classify_workspace_tool("manage_deployment"),
            ("Scripts", "manage_deployment".to_string())
        );
        assert_eq!(
            classify_workspace_tool("generate_trigger_code"),
            ("Scripts", "generate_trigger_code".to_string())
        );
        assert_eq!(
            classify_workspace_tool("set_publish_settings"),
            ("Scripts", "set_publish_settings".to_string())
        );
    }

    #[test]
    fn classify_auth_tool() {
        assert_eq!(
            classify_workspace_tool("start_google_auth"),
            ("Auth", "start".to_string())
        );
    }

    #[test]
    fn classify_fallback_tools() {
        assert_eq!(
            classify_workspace_tool("get_search_engine_info"),
            ("Workspace", "get_search_engine_info".to_string())
        );
        assert_eq!(
            classify_workspace_tool("search_custom"),
            ("Workspace", "search_custom".to_string())
        );
    }

    #[test]
    fn classify_unknown_tool_falls_back() {
        assert_eq!(
            classify_workspace_tool("some_new_tool"),
            ("Workspace", "some_new_tool".to_string())
        );
    }
}
