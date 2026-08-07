use super::*;

/// Retained extension metadata from one upstream across one catalog refresh.
/// Descriptor schemas and ordinary fields are not counted or limited here.
pub(crate) const MAX_CATALOG_METADATA_BYTES_PER_UPSTREAM: usize = 256 * 1024;

/// One refresh-local budget shared by tools, resources, templates, and prompts.
/// Once an upstream exhausts its budget, all later descriptor metadata from
/// that upstream is dropped while the descriptors themselves remain intact.
#[derive(Default)]
pub(crate) struct CatalogMetadataBudget {
    retained_bytes: HashMap<String, usize>,
    exhausted_servers: HashSet<String>,
}

impl CatalogMetadataBudget {
    pub(crate) fn admit(
        &mut self,
        server_name: &str,
        meta: &mut Option<MetaObject>,
        surface: &'static str,
    ) {
        if self.exhausted_servers.contains(server_name) {
            *meta = None;
            return;
        }

        let envelope = match crate::types::ExtensionEnvelope::from_peer_catalog_meta(meta.as_ref())
        {
            Ok(envelope) => envelope,
            Err(error) => {
                *meta = None;
                tracing::warn!(
                    surface,
                    reason = %error,
                    "discarded unsafe peer extension metadata"
                );
                return;
            }
        };
        let encoded_bytes = envelope.encoded_bytes();
        let admitted = envelope.into_meta();
        if admitted.is_none() {
            *meta = None;
            return;
        }

        let retained = self
            .retained_bytes
            .entry(server_name.to_string())
            .or_default();
        if encoded_bytes > MAX_CATALOG_METADATA_BYTES_PER_UPSTREAM.saturating_sub(*retained) {
            *meta = None;
            if self.exhausted_servers.insert(server_name.to_string()) {
                tracing::warn!(
                    server = %server_name,
                    retained_metadata_limit_bytes = MAX_CATALOG_METADATA_BYTES_PER_UPSTREAM,
                    "catalog extension metadata budget exhausted; discarding later metadata"
                );
            }
            return;
        }

        *retained += encoded_bytes;
        *meta = admitted;
    }
}

/// Apply the shared peer-extension policy before a catalog descriptor is
/// cloned into Plug's cache. Invalid peer metadata is dropped as one unit;
/// neither keys nor values are included in diagnostics.
pub(crate) fn admit_catalog_meta(meta: &mut Option<MetaObject>, surface: &'static str) {
    match crate::types::ExtensionEnvelope::from_peer_catalog_meta(meta.as_ref()) {
        Ok(envelope) => *meta = envelope.into_meta(),
        Err(error) => {
            *meta = None;
            tracing::warn!(surface, reason = %error, "discarded unsafe peer extension metadata");
        }
    }
}

pub(crate) fn admit_resource_contents_meta(contents: &mut ResourceContents) {
    match contents {
        ResourceContents::TextResourceContents { meta, .. }
        | ResourceContents::BlobResourceContents { meta, .. } => {
            admit_catalog_meta(meta, "resource-content");
        }
        // RMCP marks this enum non-exhaustive; a future variant must add an
        // explicit metadata arm before that RMCP version is adopted.
        _ => {}
    }
}

pub(crate) fn admit_content_block_meta(content: &mut ContentBlock) {
    match content {
        ContentBlock::Text(content) => admit_catalog_meta(&mut content.meta, "text-content"),
        ContentBlock::Image(content) => admit_catalog_meta(&mut content.meta, "image-content"),
        ContentBlock::Audio(content) => admit_catalog_meta(&mut content.meta, "audio-content"),
        ContentBlock::Resource(content) => {
            admit_catalog_meta(&mut content.meta, "embedded-resource");
            admit_resource_contents_meta(&mut content.resource);
        }
        ContentBlock::ResourceLink(resource) => {
            admit_catalog_meta(&mut resource.meta, "resource-link");
        }
        // See the ResourceContents note above. Current RMCP variants are all
        // covered explicitly.
        _ => {}
    }
}

pub(crate) fn admit_tool_result_meta(result: &mut CallToolResult) {
    admit_catalog_meta(&mut result.meta, "tool-result");
    for content in &mut result.content {
        admit_content_block_meta(content);
    }
}

pub(crate) fn strip_optional_fields(tool: &mut Tool, max_desc_chars: Option<usize>) {
    if let Some(ref desc) = tool.description {
        let sanitized = sanitize_description(desc);
        let final_desc = if let Some(max) = max_desc_chars {
            sanitized.chars().take(max).collect()
        } else {
            sanitized
        };
        tool.description = Some(Cow::Owned(final_desc));
    }
}

pub(crate) fn apply_canonical_tool_title(tool: &mut Tool, title: String) {
    tool.title = Some(title.clone());
    let annotations = tool.annotations.get_or_insert_with(Default::default);
    annotations.title = Some(title);
}

pub(crate) fn normalized_icons_with_fallback(
    item_icons: Option<&[Icon]>,
    fallback_icons: Option<Vec<Icon>>,
) -> Option<Vec<Icon>> {
    match item_icons {
        Some([]) | None => fallback_icons
            .map(https_only_icons)
            .filter(|icons| !icons.is_empty()),
        Some(icons) => crate::icons::normalize_icons(Some(icons)),
    }
}

pub(crate) fn https_only_icons(icons: Vec<Icon>) -> Vec<Icon> {
    icons
        .into_iter()
        .filter(|icon| icon.src.to_ascii_lowercase().starts_with("https://"))
        .collect()
}

pub(crate) fn sanitize_description(desc: &str) -> String {
    desc.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .collect()
}

/// Sort comparator: priority tools first (by priority_tools index), then alphabetical.
pub(crate) fn priority_sort(a: &Tool, b: &Tool, priority_tools: &[String]) -> std::cmp::Ordering {
    let a_priority = priority_tools
        .iter()
        .position(|p| a.name.contains(p.as_str()));
    let b_priority = priority_tools
        .iter()
        .position(|p| b.name.contains(p.as_str()));

    match (a_priority, b_priority) {
        (Some(a_idx), Some(b_idx)) => a_idx
            .cmp(&b_idx)
            .then_with(|| a.name.as_ref().cmp(b.name.as_ref())),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.as_ref().cmp(b.name.as_ref()),
    }
}

pub(crate) fn is_disabled_tool(patterns: &[String], tool_name: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    // Prefer pre-lowercased patterns (`RouterConfig::from`); still tolerate mixed case.
    let tool_name = tool_name.to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        if pattern.bytes().all(|b| !b.is_ascii_uppercase()) {
            wildcard_match(pattern, &tool_name)
        } else {
            wildcard_match(&pattern.to_ascii_lowercase(), &tool_name)
        }
    })
}

pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
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

pub(crate) fn paginated_result<T: Clone, R>(
    items: &[T],
    request: Option<PaginatedRequestParams>,
    build: impl FnOnce(Vec<T>, Option<String>) -> R,
) -> R {
    const PAGE_SIZE: usize = 500;

    let start = request
        .as_ref()
        .and_then(|params| params.cursor.as_ref())
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .filter(|idx| *idx < items.len())
        .unwrap_or(0);
    let end = usize::min(start + PAGE_SIZE, items.len());
    let next_cursor = (end < items.len()).then(|| end.to_string());

    build(items[start..end].to_vec(), next_cursor)
}

pub(crate) fn detect_tool_definition_drift(
    previous: &HashMap<String, u64>,
    current: &HashMap<String, u64>,
) -> Vec<String> {
    let mut drifted = current
        .iter()
        .filter_map(|(tool_name, fingerprint)| {
            previous
                .get(tool_name)
                .filter(|previous_fingerprint| *previous_fingerprint != fingerprint)
                .map(|_| tool_name.clone())
        })
        .collect::<Vec<_>>();
    drifted.sort();
    drifted
}

pub(crate) fn fingerprint_tool_definition(tool: &Tool) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool.name.hash(&mut hasher);
    tool.description.as_deref().unwrap_or("").hash(&mut hasher);
    tool.title.as_deref().unwrap_or("").hash(&mut hasher);
    hasher.write(
        &serde_json::to_vec(&tool.input_schema).expect("tool input schema serializes"),
    );
    hasher.write(&serde_json::to_vec(&tool.annotations).expect("tool annotations serialize"));
    hasher.finish()
}

pub(crate) fn canonical_plug_meta_tool_name(tool_name: &str) -> Option<&'static str> {
    legacy_meta_tool_names()
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(tool_name))
}

pub(crate) fn legacy_meta_tool_names() -> &'static [&'static str] {
    &[
        "plug__list_servers",
        "plug__list_tools",
        "plug__search_tools",
        "plug__invoke_tool",
    ]
}

pub(crate) fn tokenize_search_query(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

pub(crate) fn normalize_search_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn score_tool_match(
    tool: &Tool,
    server_id: &str,
    query_phrase: &str,
    tokens: &[String],
) -> Option<i64> {
    let name = normalize_search_text(tool.name.as_ref());
    let title = normalize_search_text(tool.title.as_deref().unwrap_or(""));
    let server = normalize_search_text(server_id);
    let description = normalize_search_text(tool.description.as_deref().unwrap_or(""));
    let mut score = 0i64;
    let mut all_tokens_matched = true;

    if name.contains(query_phrase) {
        score += 120;
    }
    if title.contains(query_phrase) {
        score += 100;
    }
    if description.contains(query_phrase) {
        score += 60;
    }

    for token in tokens {
        let mut token_matched = false;
        if name.contains(token) {
            score += 40;
            token_matched = true;
        }
        if title.contains(token) {
            score += 30;
            token_matched = true;
        }
        if server.contains(token) {
            score += 25;
            token_matched = true;
        }
        if description.contains(token) {
            score += 10;
            token_matched = true;
        }
        all_tokens_matched &= token_matched;
    }

    if all_tokens_matched {
        score += 50;
    }

    (score > 0).then_some(score)
}

pub(crate) fn build_meta_tools() -> Vec<Tool> {
    vec![build_search_tools_meta_tool()]
}

pub(crate) fn build_legacy_meta_tools() -> Vec<Tool> {
    vec![
        build_list_servers_meta_tool(),
        build_list_tools_meta_tool(),
        build_search_tools_meta_tool(),
        build_invoke_tool_meta_tool(),
    ]
}

pub(crate) fn build_list_servers_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__list_servers"),
        Cow::Borrowed("List upstream server IDs, health, and current routed tool counts."),
        Arc::new(serde_json::Map::new()),
    )
}

pub(crate) fn build_list_tools_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__list_tools"),
        Cow::Borrowed(
            "List routed tools hidden behind legacy meta-tool mode, optionally filtered by server or query.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "Optional upstream server ID filter"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional substring filter on tool name or description"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum tools to return (default: 25)"
                    }
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
}

/// Build the search_tools meta-tool definition.
pub(crate) fn build_search_tools_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__search_tools"),
        Cow::Borrowed(
            "Search hidden routed tools by name or description, load the matches into this session, then call the chosen real tool directly.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for tool name or description"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Maximum tool definitions to load and return (default: 5)"
                    }
                },
                "required": ["query"]
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
}

pub(crate) fn build_invoke_tool_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__invoke_tool"),
        Cow::Borrowed(
            "Invoke a specific routed tool by prefixed name and return the raw upstream result. Legacy meta-tool mode only.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Exact prefixed tool name to invoke"
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Arguments object to forward to the target tool"
                    }
                },
                "required": ["tool_name"]
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
}

impl super::ToolRouter {
    /// Get the current list of tools (zero-copy via Arc). Returns all tools.
    pub fn list_tools(&self) -> Arc<Vec<Tool>> {
        let snapshot = self.cache.load();
        match self.config.lazy_surface_for_client(ClientType::Unknown) {
            LazyToolSurface::Bridge => Arc::new(self.filter_meta_tools(&snapshot.meta_tools_all)),
            LazyToolSurface::LegacyMeta => Arc::new(self.filtered_legacy_meta_tools()),
            LazyToolSurface::Standard | LazyToolSurface::Native => Arc::clone(&snapshot.tools_all),
        }
    }

    pub fn list_tools_page_for_client(
        &self,
        client_type: ClientType,
        request: Option<PaginatedRequestParams>,
    ) -> ListToolsResult {
        self.list_tools_page_for_client_session(client_type, None, request)
    }

    pub fn list_tools_page_for_client_session(
        &self,
        client_type: ClientType,
        session_key: Option<&str>,
        request: Option<PaginatedRequestParams>,
    ) -> ListToolsResult {
        let tools = self.list_tools_for_client_session(client_type, session_key);
        paginated_result(&tools, request, |tools, next_cursor| ListToolsResult {
            // This is a synthesized, shared multi-upstream view. RMCP's
            // list_all_tools discards per-page directive provenance, so Plug
            // must not copy a private, zero/negative, or short upstream TTL
            // onto this different response. Keep cache directives absent
            // until the catalog has principal-scoped directive sidecars and a
            // conservative aggregation rule (never lengthen upstream TTLs).
            meta: None,
            next_cursor,
            tools,
            ..Default::default()
        })
    }

    /// List all tools with their source server IDs.
    pub fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let snapshot = self.cache.load();
        let mut result = Vec::new();
        for tool in snapshot.tools_all.iter() {
            let server_id = snapshot
                .routes
                .get(tool.name.as_ref())
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| "unknown".to_string());

            result.push((server_id, tool.clone()));
        }
        result
    }

    /// List all tools with their source server IDs and operator risk metadata.
    pub fn list_all_tools_with_risk(&self) -> Vec<(String, Tool, crate::ipc::IpcToolRiskInfo)> {
        let snapshot = self.cache.load();
        let mut result = Vec::new();
        for tool in snapshot.tools_all.iter() {
            let server_id = snapshot
                .routes
                .get(tool.name.as_ref())
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let risk = snapshot
                .tool_risk_inventory
                .get(tool.name.as_ref())
                .cloned()
                .unwrap_or_else(|| {
                    crate::ipc::IpcToolRiskInfo::from_annotations(
                        None,
                        tool.annotations.as_ref(),
                        tool.annotations.as_ref(),
                    )
                });

            result.push((server_id, tool.clone(), risk));
        }
        result
    }

    /// Total number of tools in the unfiltered cache.
    pub fn tool_count(&self) -> usize {
        self.cache.load().tools_all.len()
    }

    pub fn get_tool_definition(&self, name: &str) -> Option<Tool> {
        self.cache.load().tool_by_name(name).cloned()
    }

    pub fn supports_tasks(&self) -> bool {
        self.supports_tasks_for_client(ClientType::Unknown)
    }

    pub fn supports_tasks_for_client(&self, client_type: ClientType) -> bool {
        matches!(
            self.config.lazy_surface_for_client(client_type),
            LazyToolSurface::Standard | LazyToolSurface::Native
        ) && !self.cache.load().tools_all.is_empty()
    }

    pub fn synthesized_capabilities(&self) -> ServerCapabilities {
        self.synthesized_capabilities_for_client(ClientType::Unknown)
    }

    pub fn synthesized_capabilities_for_client(
        &self,
        client_type: ClientType,
    ) -> ServerCapabilities {
        let upstream_caps = self.server_manager.healthy_capabilities();
        let mut capabilities = ServerCapabilities::default();

        if matches!(
            self.config.lazy_surface_for_client(client_type),
            LazyToolSurface::Bridge | LazyToolSurface::LegacyMeta
        ) || !self.list_tools_for_client(client_type).is_empty()
            || upstream_caps.iter().any(|caps| caps.tools.is_some())
        {
            let mut tools = ToolsCapability::default();
            tools.list_changed = Some(true);
            capabilities.tools = Some(tools);
        }
        if upstream_caps.iter().any(|caps| caps.resources.is_some()) {
            let any_subscribe = upstream_caps.iter().any(|caps| {
                caps.resources
                    .as_ref()
                    .and_then(|r| r.subscribe)
                    .unwrap_or(false)
            });
            let mut resources = ResourcesCapability::default();
            resources.subscribe = if any_subscribe { Some(true) } else { None };
            resources.list_changed = Some(true);
            capabilities.resources = Some(resources);
        }
        if upstream_caps.iter().any(|caps| caps.prompts.is_some()) {
            let mut prompts = PromptsCapability::default();
            prompts.list_changed = Some(true);
            capabilities.prompts = Some(prompts);
        }
        if upstream_caps.iter().any(|caps| caps.completions.is_some()) {
            capabilities.completions = Some(serde_json::Map::new());
        }
        if upstream_caps.iter().any(|caps| caps.logging.is_some()) {
            capabilities.logging = Some(serde_json::Map::new());
        }
        if self.supports_tasks_for_client(client_type) {
            capabilities
                .experimental
                .get_or_insert_with(Default::default)
                .insert(
                    crate::protocol::LEGACY_TASKS_CAPABILITY_KEY.to_string(),
                    serde_json::Map::new(),
                );
        }

        capabilities
    }

    /// Get tools filtered for a specific client type. O(1) — single Arc::clone.
    pub fn list_tools_for_client(&self, client_type: ClientType) -> Arc<Vec<Tool>> {
        self.list_tools_for_client_session(client_type, None)
    }

    pub fn list_tools_for_client_session(
        &self,
        client_type: ClientType,
        session_key: Option<&str>,
    ) -> Arc<Vec<Tool>> {
        if matches!(
            self.config.lazy_surface_for_client(client_type),
            LazyToolSurface::Bridge
        ) {
            return self.bridge_visible_tools(session_key);
        }
        if matches!(
            self.config.lazy_surface_for_client(client_type),
            LazyToolSurface::LegacyMeta
        ) {
            return Arc::new(self.filtered_legacy_meta_tools());
        }
        if !self.config.tool_filter_enabled {
            return self.list_tools();
        }
        let snapshot = self.cache.load();
        match client_type {
            ClientType::Windsurf => Arc::clone(&snapshot.tools_windsurf),
            ClientType::VSCodeCopilot => Arc::clone(&snapshot.tools_copilot),
            _ => Arc::clone(&snapshot.tools_all),
        }
    }

    fn bridge_visible_tools(&self, session_key: Option<&str>) -> Arc<Vec<Tool>> {
        let snapshot = self.cache.load();
        let mut tools = self.filter_meta_tools(&snapshot.meta_tools_all);
        let Some(session_key) = session_key else {
            return Arc::new(tools);
        };
        let Some(loaded) = self.lazy_working_sets.get(session_key) else {
            return Arc::new(tools);
        };
        let loaded_names = loaded.value().clone();
        drop(loaded);

        for loaded_name in loaded_names {
            if let Some(tool) = snapshot.tool_by_name(&loaded_name) {
                tools.push(tool.clone());
            }
        }
        Arc::new(tools)
    }

    fn filter_meta_tools(&self, tools: &[Tool]) -> Vec<Tool> {
        tools
            .iter()
            .filter(|tool| !is_disabled_tool(&self.config.disabled_tools, tool.name.as_ref()))
            .cloned()
            .collect()
    }

    fn filtered_legacy_meta_tools(&self) -> Vec<Tool> {
        build_legacy_meta_tools()
            .into_iter()
            .filter(|tool| !is_disabled_tool(&self.config.disabled_tools, tool.name.as_ref()))
            .collect()
    }

    pub fn list_resources(&self) -> Arc<Vec<Resource>> {
        Arc::clone(&self.cache.load().resources_all)
    }

    /// Number of resource URIs with at least one active downstream subscriber.
    /// Read-side observability; also lets tests assert that a degraded upstream's
    /// subscriptions survive a catalog refresh rather than being pruned.
    ///
    /// Counts tracked URIs regardless of transition state (Pending/Active/
    /// Draining), matching the historical `DashMap::len()` semantics this
    /// replaced.
    pub fn active_subscription_count(&self) -> usize {
        self.resource_subscriptions.len()
    }

    pub fn list_resources_page(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> ListResourcesResult {
        let resources = self.list_resources();
        paginated_result(&resources, request, |resources, next_cursor| {
            ListResourcesResult {
                meta: None,
                next_cursor,
                resources,
                ..Default::default()
            }
        })
    }

    pub fn list_resource_templates(&self) -> Arc<Vec<ResourceTemplate>> {
        Arc::clone(&self.cache.load().resource_templates_all)
    }

    pub fn list_resource_templates_page(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> ListResourceTemplatesResult {
        let resource_templates = self.list_resource_templates();
        paginated_result(
            &resource_templates,
            request,
            |resource_templates, next_cursor| ListResourceTemplatesResult {
                meta: None,
                next_cursor,
                resource_templates,
                ..Default::default()
            },
        )
    }

    pub fn list_prompts(&self) -> Arc<Vec<Prompt>> {
        Arc::clone(&self.cache.load().prompts_all)
    }

    pub fn list_prompts_page(&self, request: Option<PaginatedRequestParams>) -> ListPromptsResult {
        let prompts = self.list_prompts();
        paginated_result(&prompts, request, |prompts, next_cursor| {
            ListPromptsResult {
                meta: None,
                next_cursor,
                prompts,
                ..Default::default()
            }
        })
    }
}

#[cfg(test)]
mod metadata_budget_tests {
    use super::*;
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    #[derive(Clone, Default)]
    struct EventCapture(Arc<Mutex<String>>);

    struct EventVisitor<'a>(&'a mut String);

    impl Visit for EventVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }
    }

    impl Subscriber for EventCapture {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = String::new();
            event.record(&mut EventVisitor(&mut fields));
            let mut captured = self.0.lock().expect("event capture lock");
            captured.push_str(&fields);
            captured.push('\n');
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    fn large_valid_meta(key: &str, value_prefix: &str) -> MetaObject {
        let value = format!("{value_prefix}{}", "x".repeat(15 * 1024));
        MetaObject::from(
            serde_json::json!({(key): value})
                .as_object()
                .expect("metadata object")
                .clone(),
        )
    }

    #[test]
    fn aggregate_catalog_metadata_budget_keeps_descriptors_and_logs_no_peer_data() {
        const PEER_KEY: &str = "attacker.example/never-log-this-key";
        const PEER_VALUE: &str = "metadata-value-must-not-leak-";
        let capture = EventCapture::default();
        let captured = Arc::clone(&capture.0);

        let descriptors = tracing::subscriber::with_default(capture, || {
            let mut budget = CatalogMetadataBudget::default();
            let mut descriptors = (0..40)
                .map(|id| {
                    let mut meta = Some(large_valid_meta(PEER_KEY, PEER_VALUE));
                    let surface = match id % 4 {
                        0 => "tool",
                        1 => "resource",
                        2 => "resource-template",
                        _ => "prompt",
                    };
                    budget.admit("untrusted-upstream", &mut meta, surface);
                    (id, meta)
                })
                .collect::<Vec<_>>();
            let mut independent_meta = Some(large_valid_meta(PEER_KEY, PEER_VALUE));
            budget.admit("independent-upstream", &mut independent_meta, "tool");
            descriptors.push((40, independent_meta));
            descriptors
        });

        assert_eq!(
            descriptors.len(),
            41,
            "metadata must not remove descriptors"
        );
        assert!(
            descriptors.last().is_some_and(|(_, meta)| meta.is_some()),
            "one upstream exhausting its budget must not consume another upstream's budget"
        );
        assert!(descriptors.iter().any(|(_, meta)| meta.is_some()));
        assert!(descriptors.iter().any(|(_, meta)| meta.is_none()));

        let retained_bytes = descriptors
            .iter()
            .filter(|(id, _)| *id < 40)
            .filter_map(|(_, meta)| meta.as_ref())
            .map(|meta| {
                serde_json::to_vec(meta)
                    .expect("retained metadata serializes")
                    .len()
            })
            .sum::<usize>();
        assert!(retained_bytes <= MAX_CATALOG_METADATA_BYTES_PER_UPSTREAM);

        let diagnostics = captured.lock().expect("captured diagnostics").clone();
        assert_eq!(
            diagnostics
                .matches("catalog extension metadata budget exhausted")
                .count(),
            1,
            "budget warning must be emitted at most once per upstream and refresh"
        );
        assert!(!diagnostics.contains(PEER_KEY));
        assert!(!diagnostics.contains(PEER_VALUE));
    }
}
