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
}
