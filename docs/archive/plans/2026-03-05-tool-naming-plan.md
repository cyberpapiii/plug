# Tool Naming & Display System — Implementation Plan

> Historical implementation artifact. Current naming truth lives in `README.md`, `docs/MCP-SPEC.md`, and the code on `main`. Keep this file for archaeology, not as the current source of truth.

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve tool wire names for LLM accuracy and human readability — sanitize casing, add semantic sub-service prefixes for workspace tools, generate `title` fields, and add case-insensitive routing fallback.

**Architecture:** New `tool_naming` module handles all name transformations (sanitize, classify, title-generate). Called from `refresh_tools()` in proxy/mod.rs. Routing table maps transformed wire names back to original upstream names. Enrichment runs on sanitized names.

**Tech Stack:** Rust, rmcp model types, regex-free string manipulation

---

### Task 1: Create `tool_naming` module with sanitization

**Files:**
- Create: `plug-core/src/tool_naming.rs`
- Modify: `plug-core/src/lib.rs` (add `pub mod tool_naming;`)

**Step 1: Write the failing tests**

```rust
// plug-core/src/tool_naming.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_hyphens_to_underscores() {
        assert_eq!(sanitize_tool_name("create-comment"), "create_comment");
        assert_eq!(sanitize_tool_name("query-database-view"), "query_database_view");
    }

    #[test]
    fn sanitize_camel_case_to_snake() {
        assert_eq!(sanitize_tool_name("listProjects"), "list_projects");
        assert_eq!(sanitize_tool_name("whoAmI"), "who_am_i");
        assert_eq!(sanitize_tool_name("getHTTPResponse"), "get_http_response");
    }

    #[test]
    fn sanitize_dots_to_underscores() {
        assert_eq!(sanitize_tool_name("admin.tools.list"), "admin_tools_list");
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
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p plug-core tool_naming -- --nocapture`
Expected: FAIL — module doesn't exist

**Step 3: Write the sanitization function**

```rust
// plug-core/src/tool_naming.rs

/// Sanitize a tool name to snake_case.
///
/// - Hyphens → underscores
/// - Dots → underscores
/// - camelCase → snake_case
/// - Collapse multiple underscores
/// - Lowercase everything
pub fn sanitize_tool_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 8);

    for (i, ch) in name.chars().enumerate() {
        if ch == '-' || ch == '.' {
            result.push('_');
        } else if ch.is_uppercase() {
            // Insert underscore before uppercase if preceded by lowercase or digit
            if i > 0 {
                let prev = name.as_bytes()[i - 1];
                if prev.is_ascii_lowercase() || prev.is_ascii_digit() {
                    result.push('_');
                }
                // Handle sequences like "HTTP" -> don't insert _ between H-T-T-P
                // but do insert before the lowercase after: "HTTPResponse" -> "http_response"
                if prev.is_ascii_uppercase() {
                    // Check if next char is lowercase (end of acronym)
                    if let Some(next) = name.chars().nth(i + 1) {
                        if next.is_ascii_lowercase() {
                            result.push('_');
                        }
                    }
                }
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }

    // Collapse multiple underscores and trim leading/trailing
    let collapsed: String = result
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    collapsed
}
```

Also add to `plug-core/src/lib.rs`:
```rust
pub mod tool_naming;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p plug-core tool_naming -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/tool_naming.rs plug-core/src/lib.rs
git commit -m "feat: add tool_naming module with snake_case sanitization"
```

---

### Task 2: Add workspace sub-service classification

**Files:**
- Modify: `plug-core/src/tool_naming.rs`

**Step 1: Write the failing tests**

```rust
#[test]
fn classify_gmail_tools() {
    assert_eq!(
        classify_workspace_tool("get_gmail_messages_content_batch"),
        ("Gmail", "get_messages_content_batch")
    );
    assert_eq!(
        classify_workspace_tool("search_gmail_messages"),
        ("Gmail", "search_messages")
    );
    assert_eq!(
        classify_workspace_tool("draft_gmail_message"),
        ("Gmail", "draft_message")
    );
}

#[test]
fn classify_drive_tools() {
    assert_eq!(
        classify_workspace_tool("search_drive_files"),
        ("Drive", "search_files")
    );
    assert_eq!(
        classify_workspace_tool("get_drive_file_content"),
        ("Drive", "get_file_content")
    );
}

#[test]
fn classify_docs_tools() {
    assert_eq!(
        classify_workspace_tool("get_doc_content"),
        ("Docs", "get_content")
    );
    assert_eq!(
        classify_workspace_tool("create_doc"),
        ("Docs", "create")
    );
    assert_eq!(
        classify_workspace_tool("list_document_comments"),
        ("Docs", "list_comments")
    );
    assert_eq!(
        classify_workspace_tool("import_to_google_doc"),
        ("Docs", "import")
    );
    assert_eq!(
        classify_workspace_tool("create_table_with_data"),
        ("Docs", "create_table_with_data")
    );
    assert_eq!(
        classify_workspace_tool("batch_update_doc"),
        ("Docs", "batch_update")
    );
}

#[test]
fn classify_sheets_tools() {
    assert_eq!(
        classify_workspace_tool("read_sheet_values"),
        ("Sheets", "read_values")
    );
    assert_eq!(
        classify_workspace_tool("create_spreadsheet"),
        ("Sheets", "create")
    );
    assert_eq!(
        classify_workspace_tool("manage_conditional_formatting"),
        ("Sheets", "manage_conditional_formatting")
    );
}

#[test]
fn classify_slides_tools() {
    assert_eq!(
        classify_workspace_tool("get_presentation"),
        ("Slides", "get")
    );
    assert_eq!(
        classify_workspace_tool("get_page"),
        ("Slides", "get_page")
    );
    assert_eq!(
        classify_workspace_tool("get_page_thumbnail"),
        ("Slides", "get_thumbnail")
    );
}

#[test]
fn classify_calendar_tools() {
    assert_eq!(
        classify_workspace_tool("get_events"),
        ("Calendar", "get_events")
    );
    assert_eq!(
        classify_workspace_tool("manage_event"),
        ("Calendar", "manage")
    );
    assert_eq!(
        classify_workspace_tool("list_calendars"),
        ("Calendar", "list")
    );
    assert_eq!(
        classify_workspace_tool("query_freebusy"),
        ("Calendar", "query_freebusy")
    );
}

#[test]
fn classify_contacts_tools() {
    assert_eq!(
        classify_workspace_tool("search_contacts"),
        ("Contacts", "search")
    );
    assert_eq!(
        classify_workspace_tool("manage_contact_group"),
        ("Contacts", "manage_group")
    );
}

#[test]
fn classify_tasks_tools() {
    assert_eq!(
        classify_workspace_tool("manage_task"),
        ("Tasks", "manage")
    );
    assert_eq!(
        classify_workspace_tool("list_task_lists"),
        ("Tasks", "list_lists")
    );
}

#[test]
fn classify_chat_tools() {
    assert_eq!(
        classify_workspace_tool("send_message"),
        ("Chat", "send")
    );
    assert_eq!(
        classify_workspace_tool("list_spaces"),
        ("Chat", "list_spaces")
    );
    assert_eq!(
        classify_workspace_tool("create_reaction"),
        ("Chat", "create_reaction")
    );
    assert_eq!(
        classify_workspace_tool("download_chat_attachment"),
        ("Chat", "download_attachment")
    );
}

#[test]
fn classify_forms_tools() {
    assert_eq!(
        classify_workspace_tool("create_form"),
        ("Forms", "create")
    );
    assert_eq!(
        classify_workspace_tool("list_form_responses"),
        ("Forms", "list_responses")
    );
}

#[test]
fn classify_scripts_tools() {
    assert_eq!(
        classify_workspace_tool("run_script_function"),
        ("Scripts", "run_function")
    );
    assert_eq!(
        classify_workspace_tool("list_deployments"),
        ("Scripts", "list_deployments")
    );
    assert_eq!(
        classify_workspace_tool("create_version"),
        ("Scripts", "create_version")
    );
    assert_eq!(
        classify_workspace_tool("set_publish_settings"),
        ("Scripts", "set_publish_settings")
    );
}

#[test]
fn classify_auth_tools() {
    assert_eq!(
        classify_workspace_tool("start_google_auth"),
        ("Auth", "start")
    );
}

#[test]
fn classify_unknown_falls_back_to_workspace() {
    assert_eq!(
        classify_workspace_tool("some_new_unknown_tool"),
        ("Workspace", "some_new_unknown_tool")
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p plug-core classify_workspace -- --nocapture`
Expected: FAIL — function doesn't exist

**Step 3: Write the classification function**

```rust
/// Classify a workspace tool into a sub-service and strip the service keyword.
///
/// Returns (sub_service_name, cleaned_tool_name).
/// The sub_service_name is PascalCase (e.g., "Gmail", "Drive").
/// The cleaned_tool_name has the service keyword stripped to avoid redundancy.
pub fn classify_workspace_tool(tool_name: &str) -> (&'static str, String) {
    // Order matters: more specific patterns first to avoid false matches.
    // Each entry: (service_name, keywords_to_match, keywords_to_strip)
    let rules: &[(&str, &[&str], &[&str])] = &[
        ("Gmail", &["gmail"], &["gmail_"]),
        ("Drive", &["drive"], &["drive_", "drive_file_"]),
        ("Sheets", &["sheet", "spreadsheet", "conditional_formatting"], &["sheet_", "spreadsheet_"]),
        ("Slides", &["presentation", "page_thumbnail"], &["presentation_", "page_"]),
        ("Docs", &[
            "doc_", "doc\0",  // doc_ or end-of-word "doc"
            "document", "paragraph_style",
            "import_to_google", "create_table_with_data",
            "debug_table_structure", "batch_update_doc",
            "find_and_replace", "search_docs",
        ], &["doc_", "document_", "google_doc"]),
        ("Calendar", &["event", "calendar", "freebusy"], &["calendar"]),
        ("Contacts", &["contact"], &["contact_", "contacts_"]),
        ("Tasks", &["task"], &["task_", "task_list"]),
        ("Chat", &["message", "spaces", "reaction", "chat_"], &["chat_", "message"]),
        ("Forms", &["form"], &["form_"]),
        ("Scripts", &[
            "script", "deployment", "version",
            "trigger_code", "publish_settings",
        ], &["script_"]),
        ("Auth", &["google_auth"], &["google_auth"]),
    ];

    for (service, match_keywords, strip_keywords) in rules {
        let matched = match_keywords.iter().any(|kw| {
            if *kw == "doc\0" {
                // Special: match "doc" as complete word at end or followed by nothing
                tool_name == "create_doc" || tool_name == "batch_update_doc"
                    || tool_name == "search_docs" || tool_name == "import_to_google_doc"
            } else {
                tool_name.contains(kw)
            }
        });

        if matched {
            let mut cleaned = tool_name.to_string();

            // Strip service keywords from the tool name
            for strip in strip_keywords.iter() {
                cleaned = cleaned.replace(strip, "");
            }

            // Clean up: remove leading/trailing underscores, collapse doubles
            let cleaned = cleaned
                .trim_matches('_')
                .split('_')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("_");

            // If stripping left us with nothing, use the original
            let cleaned = if cleaned.is_empty() {
                tool_name.to_string()
            } else {
                cleaned
            };

            return (service, cleaned);
        }
    }

    ("Workspace", tool_name.to_string())
}
```

Note: This function will need iteration to pass all tests. The test cases define the exact expected behavior — implement until all pass. The key patterns:
- "Slides" matches `presentation` and `page_thumbnail` (but NOT bare `get_page` — that needs special handling or Slides should also match `get_page`)
- "Docs" needs to match both `doc_` prefix patterns AND standalone words like `create_doc`, `batch_update_doc`
- "Chat" needs to catch `send_message` (no chat keyword) and `create_reaction`
- Stripping should remove the service keyword but preserve the verb and remaining nouns

**Step 4: Run tests, iterate until all pass**

Run: `cargo test -p plug-core classify_workspace -- --nocapture`
Expected: PASS (may need 2-3 iterations on edge cases)

**Step 5: Commit**

```bash
git add plug-core/src/tool_naming.rs
git commit -m "feat: add workspace sub-service classification"
```

---

### Task 3: Add title generation and server prefix formatting

**Files:**
- Modify: `plug-core/src/tool_naming.rs`

**Step 1: Write the failing tests**

```rust
#[test]
fn capitalize_server_name() {
    assert_eq!(format_server_prefix("slack"), "Slack");
    assert_eq!(format_server_prefix("imessage"), "IMessage");
    assert_eq!(format_server_prefix("context7"), "Context7");
    assert_eq!(format_server_prefix("supabase"), "Supabase");
    assert_eq!(format_server_prefix("supermemory"), "Supermemory");
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
    assert_eq!(
        generate_title("Notion", "search"),
        "Notion: Search"
    );
}

#[test]
fn build_wire_name() {
    assert_eq!(
        build_wire_name("Slack", "search_messages", "__"),
        "Slack__search_messages"
    );
    assert_eq!(
        build_wire_name("Gmail", "get_messages_content_batch", "__"),
        "Gmail__get_messages_content_batch"
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p plug-core generate_title -- --nocapture`
Expected: FAIL

**Step 3: Write the functions**

```rust
/// Capitalize the first letter of a server name for use as a wire name prefix.
pub fn format_server_prefix(server_name: &str) -> String {
    let mut chars = server_name.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let mut s = c.to_uppercase().to_string();
            s.extend(chars);
            s
        }
    }
}

/// Generate a human-readable title from a server prefix and tool name.
/// "Slack", "search_messages" -> "Slack: Search Messages"
pub fn generate_title(server_prefix: &str, tool_name: &str) -> String {
    let tool_part = tool_name
        .split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let mut word = c.to_uppercase().to_string();
                    word.extend(chars);
                    word
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    format!("{}: {}", server_prefix, tool_part)
}

/// Build the wire name: "{ServerPrefix}__{tool_name}"
pub fn build_wire_name(server_prefix: &str, tool_name: &str, delimiter: &str) -> String {
    format!("{}{}{}", server_prefix, delimiter, tool_name)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p plug-core tool_naming -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/tool_naming.rs
git commit -m "feat: add title generation and server prefix formatting"
```

---

### Task 4: Integrate into `refresh_tools()`

**Files:**
- Modify: `plug-core/src/proxy/mod.rs:113-169` (the `refresh_tools` method)

**Step 1: Write an integration test**

Add to `plug-core/src/proxy/mod.rs` tests section:

```rust
#[test]
fn tool_naming_transforms_applied() {
    // Simulate what refresh_tools does: sanitize + prefix + title
    use crate::tool_naming::*;

    let server_name = "notion";
    let original_name = "create-comment";

    let sanitized = sanitize_tool_name(original_name);
    assert_eq!(sanitized, "create_comment");

    let prefix = format_server_prefix(server_name);
    assert_eq!(prefix, "Notion");

    let wire = build_wire_name(&prefix, &sanitized, "__");
    assert_eq!(wire, "Notion__create_comment");

    let title = generate_title(&prefix, &sanitized);
    assert_eq!(title, "Notion: Create Comment");
}

#[test]
fn workspace_tool_naming_transforms() {
    use crate::tool_naming::*;

    let server_name = "workspace";
    let original_name = "get_gmail_messages_content_batch";

    let sanitized = sanitize_tool_name(original_name);
    assert_eq!(sanitized, "get_gmail_messages_content_batch");

    // Workspace gets sub-service classification
    let (sub_service, cleaned) = classify_workspace_tool(&sanitized);
    assert_eq!(sub_service, "Gmail");
    assert_eq!(cleaned, "get_messages_content_batch");

    let wire = build_wire_name(sub_service, &cleaned, "__");
    assert_eq!(wire, "Gmail__get_messages_content_batch");

    let title = generate_title(sub_service, &cleaned);
    assert_eq!(title, "Gmail: Get Messages Content Batch");
}
```

**Step 2: Run tests to verify they pass** (these should pass since they use already-implemented functions)

Run: `cargo test -p plug-core tool_naming_transforms -- --nocapture`
Expected: PASS

**Step 3: Modify `refresh_tools()` in `proxy/mod.rs`**

Replace the current tool naming logic (lines 119-168) with calls to `tool_naming` functions. The key changes:

1. After line 126 (manual renames), add sanitization:
```rust
// 2. Sanitize to snake_case
exposed_name = crate::tool_naming::sanitize_tool_name(&exposed_name);
```

2. Replace the prefix logic (lines 146-149) with:
```rust
// 3. Determine the display prefix
let (prefix, cleaned_name) = if server_name == "workspace" {
    let (sub_service, cleaned) = crate::tool_naming::classify_workspace_tool(&exposed_name);
    (sub_service.to_string(), cleaned)
} else {
    (crate::tool_naming::format_server_prefix(&server_name), exposed_name)
};

let prefixed_name = crate::tool_naming::build_wire_name(
    &prefix, &cleaned_name, &self.config.prefix_delimiter,
);
```

3. After creating `prefixed_tool`, set the title:
```rust
// Set title for human-friendly display in clients that support it
prefixed_tool.title = Some(crate::tool_naming::generate_title(&prefix, &cleaned_name));
```

4. Update `strip_optional_fields` to NOT strip `title` (line 603):
```rust
// Previously: tool.title = None;
// Now: preserve title — it's our human-friendly display name
```

**Step 4: Run full test suite**

Run: `cargo test -p plug-core -- --nocapture`
Expected: PASS (some existing tests may need wire name updates to match new format)

**Step 5: Commit**

```bash
git add plug-core/src/proxy/mod.rs
git commit -m "feat: integrate tool naming transforms into refresh_tools"
```

---

### Task 5: Add case-insensitive routing fallback

**Files:**
- Modify: `plug-core/src/proxy/mod.rs:269-298` (the `call_tool_inner` method)

**Step 1: Write the failing test**

```rust
#[test]
fn case_insensitive_route_lookup() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Set up routes with PascalCase prefix
    let mut routes = HashMap::new();
    routes.insert(
        "Slack__search_messages".to_string(),
        ("slack".to_string(), "conversations_search_messages".to_string()),
    );

    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(Vec::new()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
    }));

    let snapshot = router.cache.load();
    // Exact match works
    assert!(snapshot.routes.get("Slack__search_messages").is_some());
    // Lowercase should also resolve via fallback
    let lower = "slack__search_messages";
    let found = snapshot.routes.get(lower).or_else(|| {
        snapshot.routes.iter().find(|(k, _)| k.eq_ignore_ascii_case(lower)).map(|(_, v)| v)
    });
    assert!(found.is_some());
}
```

**Step 2: Run test to verify it passes** (this tests the approach, not the integration)

Run: `cargo test -p plug-core case_insensitive -- --nocapture`
Expected: PASS

**Step 3: Modify `call_tool_inner` route lookup**

In `proxy/mod.rs`, change the route lookup (around line 294):

```rust
// Look up the server and original name — with case-insensitive fallback
let cache = self.cache.load();
let (server_id, original_name) = cache
    .routes
    .get(tool_name)
    .or_else(|| {
        // Case-insensitive fallback for LLM casing drift
        cache.routes.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(tool_name))
            .map(|(_, v)| v)
    })
    .ok_or_else(|| {
        McpError::from(ProtocolError::ToolNotFound {
            tool_name: tool_name.to_string(),
        })
    })?;
```

Also update the `handle_search_tools` check (line 288) to be case-insensitive:
```rust
if tool_name.eq_ignore_ascii_case("plug__search_tools") {
```

**Step 4: Run tests**

Run: `cargo test -p plug-core -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add plug-core/src/proxy/mod.rs
git commit -m "feat: add case-insensitive tool routing fallback"
```

---

### Task 6: Update `list_all_tools()` and CLI display

**Files:**
- Modify: `plug-core/src/proxy/mod.rs:221-245` (`list_all_tools` method)
- Modify: `plug-core/src/ipc.rs` (add `title` field to `IpcToolInfo`)
- Modify: `plug/src/daemon.rs:665-677` (pass title through IPC)
- Modify: `plug/src/main.rs:423-464` (`cmd_tool_list` display)

**Step 1: Add `title` to IpcToolInfo**

In `plug-core/src/ipc.rs`, add a `title` field:
```rust
pub struct IpcToolInfo {
    pub name: String,
    pub server_id: String,
    pub description: Option<String>,
    pub title: Option<String>,  // NEW
}
```

**Step 2: Update daemon to pass title**

In `plug/src/daemon.rs:670`, add title:
```rust
plug_core::ipc::IpcToolInfo {
    name: tool.name.to_string(),
    server_id,
    description: tool.description.map(|d| d.to_string()),
    title: tool.title.clone(),  // NEW
}
```

**Step 3: Update `list_all_tools()` to use config server ID for grouping**

The current method strips the prefix to show original names. Now we want to show the wire name but group by the config server (or sub-service for workspace). Update to return the wire name and the upstream server ID:

In `proxy/mod.rs`, modify `list_all_tools()` to return `(config_server_id, tool)` where the tool keeps its wire name (don't strip prefix). The CLI will handle display grouping.

**Step 4: Update `cmd_tool_list` in `main.rs`**

Update the display to group by sub-service for workspace tools and show title:

```rust
// Group by the prefix part of the wire name (before __) instead of server_id
for t in tools {
    let display_group = t.name.split("__").next().unwrap_or(&t.server_id);
    tools_by_group.entry(display_group.to_string())
        .or_insert_with(Vec::new)
        .push((t.name, t.title, t.description, t.server_id));
}
```

Show the group with upstream server annotation for workspace sub-services:
```
  Gmail (14 tools)                               [workspace]
    get_messages_content_batch    Gmail: Get Messages Content Batch
```

**Step 5: Run full build and manual test**

Run: `cargo build --bin plug && cargo run --bin plug -- tools`
Expected: Tools grouped by sub-service with titles

**Step 6: Commit**

```bash
git add plug-core/src/ipc.rs plug-core/src/proxy/mod.rs plug/src/daemon.rs plug/src/main.rs
git commit -m "feat: update CLI display with titles and sub-service grouping"
```

---

### Task 7: Update enrichment to run on sanitized names

**Files:**
- Modify: `plug-core/src/proxy/mod.rs` (move enrichment call after sanitization)

**Step 1: Verify enrichment ordering**

Currently enrichment runs on the original upstream tool name (line 159-161). It needs to run on the sanitized+cleaned name so patterns like `get_*` match correctly on names that were originally `get-something`.

Move the enrichment call to AFTER sanitization and classification:

```rust
// Apply enrichment on the sanitized name (so get-* → get_* matches patterns)
if self.config.enrichment_servers.contains(&server_name) {
    crate::enrichment::enrich_tool(&mut prefixed_tool);
}
```

The enrichment `infer_annotations` checks `tool.name` for prefixes like `get_`, `list_`, etc. Since the tool name is now `Gmail__get_messages_content_batch`, the patterns won't match (they start with `Gmail__`).

Fix: enrichment should check against the cleaned tool name, not the full wire name. Either:
- Pass the cleaned name to enrichment, or
- Run enrichment BEFORE setting the wire name

Best approach: run enrichment before setting `prefixed_tool.name`:
```rust
// Enrich based on the cleaned tool name (before prefixing)
if self.config.enrichment_servers.contains(&server_name) {
    // Temporarily set the sanitized name for pattern matching
    prefixed_tool.name = Cow::Owned(cleaned_name.clone());
    crate::enrichment::enrich_tool(&mut prefixed_tool);
}
// Then set the final wire name
prefixed_tool.name = Cow::Owned(prefixed_name);
```

**Step 2: Write a test**

```rust
#[test]
fn enrichment_works_on_sanitized_names() {
    use crate::enrichment::enrich_tool;

    // Original name had hyphens: "get-comments"
    // After sanitization: "get_comments"
    let mut tool = Tool::new(
        Cow::Borrowed("get_comments"),
        Cow::Borrowed("Get comments"),
        Arc::new(serde_json::Map::new()),
    );
    enrich_tool(&mut tool);
    assert_eq!(
        tool.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );
}
```

**Step 3: Run tests**

Run: `cargo test -p plug-core -- --nocapture`
Expected: PASS

**Step 4: Commit**

```bash
git add plug-core/src/proxy/mod.rs
git commit -m "fix: run enrichment on sanitized names before prefixing"
```

---

### Task 8: Update existing tests for new wire name format

**Files:**
- Modify: `plug-core/src/proxy/mod.rs` (test section)
- Modify: `plug-core/tests/integration_tests.rs`

**Step 1: Update test expectations**

Existing tests reference `server__tool_name` format. Update to expect `Server__tool_name` format:
- `"git__commit"` → `"Git__commit"`
- `"git__push"` → `"Git__push"`
- `"slack__send"` → `"Slack__send"`
- `"server__important_tool"` → `"Server__important_tool"`

Also update the search_tools test to expect PascalCase prefixes.

**Step 2: Run full test suite**

Run: `cargo test -- --nocapture`
Expected: PASS

**Step 3: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings

**Step 4: Commit**

```bash
git add plug-core/src/proxy/mod.rs plug-core/tests/integration_tests.rs
git commit -m "test: update test expectations for new tool naming format"
```

---

### Task 9: Manual verification with live servers

**Step 1: Build and run**

```bash
cargo build --bin plug
cargo run --bin plug -- tools
```

Expected output should show:
- Workspace tools split into Gmail, Drive, Docs, Sheets, etc.
- All servers with PascalCase prefix (Slack, Notion, Krisp, etc.)
- Hyphens and camelCase normalized
- Titles in `Server: Tool Name` format

**Step 2: Verify routing works**

Start the daemon and verify tool calls still route correctly:
```bash
cargo run --bin plug -- daemon &
# Test a tool call through Claude Code or direct MCP client
```

**Step 3: Check for 64-char overflow**

```bash
cargo run --bin plug -- tools --output json | python3 -c "
import json, sys
data = json.loads(sys.stdin.read())
for server, tools in data.items():
    for t in tools:
        name = t[0] if isinstance(t, list) else t
        cc = f'mcp__plug__{name}'
        if len(cc) > 64:
            print(f'WARNING: {cc} ({len(cc)} chars)')
"
```

Expected: No warnings (all names under 64 chars)

**Step 4: Final commit**

If any fixes were needed, commit them:
```bash
git commit -m "fix: tool naming edge cases from live testing"
```
