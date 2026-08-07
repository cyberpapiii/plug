#![forbid(unsafe_code)]
// Exercise the complete MCP 2025-11-25 surface even where RMCP 3.1 marks
// features deprecated toward future SEP-2577.
#![allow(deprecated)]

//! Mock MCP server for integration testing.
//!
//! A real MCP server binary that exposes configurable tools via stdio transport.
//! Each tool returns a text response echoing the arguments it was called with.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use clap::Parser;
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt as _;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};

#[derive(Parser)]
#[command(name = "mock-mcp-server")]
struct Args {
    /// Tools to expose (comma-separated names)
    #[arg(long, default_value = "echo,greet")]
    tools: String,

    /// Simulated response delay in milliseconds
    #[arg(long, default_value = "0")]
    delay_ms: u64,

    /// Fail mode: "none", "timeout" (hang forever), "crash" (exit immediately)
    #[arg(long, default_value = "none")]
    fail_mode: String,

    /// Reverse request mode: "none", "elicitation", "sampling"
    /// When set, call_tool will send a reverse request to the client before returning.
    #[arg(long, default_value = "none")]
    reverse_request: String,

    /// Expose one subscribable mock resource.
    #[arg(long, default_value_t = false)]
    resources: bool,

    /// Expose one mock resource template (requires `--resources` so the
    /// resources capability is advertised and plug lists templates).
    #[arg(long, default_value_t = false)]
    resource_templates: bool,

    /// Advertise the prompts capability and expose one mock prompt.
    #[arg(long, default_value_t = false)]
    prompts: bool,

    /// Advertise the completions capability and answer completion requests.
    #[arg(long, default_value_t = false)]
    completions: bool,

    /// When this file exists, `list_resources` returns an error — simulating a
    /// transient listing failure so tests can drive the degraded carry-forward
    /// path on demand (create the file, refresh; remove it, refresh to recover).
    #[arg(long)]
    list_fail_flag_file: Option<String>,

    /// When this file exists, `list_resources` returns an empty success —
    /// simulating a genuine resource removal so tests can exercise the
    /// fresh-empty prune path (distinct from the failure path above).
    #[arg(long)]
    list_empty_flag_file: Option<String>,

    /// Simulated delay before responding to list_resources,
    /// list_resource_templates, and list_prompts, in milliseconds. Default:
    /// no delay — existing users of the mock are unaffected.
    #[arg(long, default_value = "0")]
    list_delay_ms: u64,

    /// Speak the removed SEP-1686 task protocol as raw JSON-RPC. This exists
    /// solely to prove Plug's RMCP-3 compatibility adapter against a real
    /// child-process stdio boundary.
    #[arg(long, default_value_t = false)]
    legacy_tasks: bool,

    /// Lifecycle fixture: `rmcp` accepts both eras, `legacy-only` rejects
    /// server/discover, and `modern-only` rejects initialize.
    #[arg(long, default_value = "rmcp")]
    lifecycle: String,

    /// Optional newline-delimited request-method log for deterministic
    /// lifecycle sequence assertions.
    #[arg(long)]
    request_log_file: Option<String>,

    /// Speak the official `@modelcontextprotocol/conformance` content fixtures
    /// (`test_simple_text`, `test://static-text`, …). Opt-in evidence only —
    /// does not change the default mock catalog. Attach under Plug with
    /// `enable_prefix = false` so suite tool names stay unprefixed.
    #[arg(long, default_value_t = false)]
    official_modern_fixture: bool,
}

async fn append_request_log(path: Option<&str>, method: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let Some(path) = path else {
        return Ok(());
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(format!("{method}\n").as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

async fn serve_lifecycle_stdio(mode: &str, request_log_file: Option<&str>) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let request: serde_json::Value = serde_json::from_str(&line)?;
        let method = request["method"].as_str().unwrap_or_default();
        append_request_log(request_log_file, method).await?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let response = match (mode, method) {
            ("legacy-only", "server/discover") | ("modern-only", "initialize") => {
                serde_json::json!({
                    "jsonrpc":"2.0","id":id,
                    "error":{"code":-32601,"message":method}
                })
            }
            ("legacy-only", "initialize") => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":"2025-11-25",
                    "capabilities":{"tools":{"listChanged":false}},
                    "serverInfo":{"name":"mock-legacy-only","version":"0.1.0"}
                }
            }),
            ("modern-only", "server/discover") => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "resultType":"complete",
                    "supportedVersions":["2026-07-28"],
                    "capabilities":{"tools":{"listChanged":false}},
                    "ttlMs":0,
                    "cacheScope":"private",
                    "_meta":{"io.modelcontextprotocol/serverInfo":{
                        "name":"mock-modern-only","version":"0.1.0"
                    }}
                }
            }),
            (_, "tools/list") => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "resultType":"complete",
                    "ttlMs":0,
                    "cacheScope":"private",
                    "tools":[{
                        "name":"echo","description":"echo","inputSchema":{"type":"object"},
                        "outputSchema":{"type":"object","properties":{"echoed":{"type":"boolean"}}},
                        "_meta":{
                            "io.modelcontextprotocol/deferredFixture":true,
                            "io.modelcontextprotocol/ui":{"resourceUri":"ui://plug/fixture"},
                            "example.test/typed":{"boolean":true,"number":7,"array":[null,"value"]}
                        }
                    }]
                }
            }),
            (_, "tools/call") => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "resultType":"complete",
                    "content":[{"type":"text","text":"lifecycle fixture echo"}],
                    "isError":false,
                    "_meta":{"example.test/result":{"boolean":true,"number":7}}
                }
            }),
            _ => serde_json::json!({
                "jsonrpc":"2.0","id":id,
                "error":{"code":-32601,"message":method}
            }),
        };
        stdout
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

async fn serve_legacy_tasks_stdio() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut next_id = 0usize;
    let mut tasks = BTreeMap::<String, (serde_json::Value, serde_json::Value)>::new();
    while let Some(line) = lines.next_line().await? {
        let request: serde_json::Value = serde_json::from_str(&line)?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default();
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc":"2.0","id":id,"result":{
                    "protocolVersion":"2025-11-25",
                    "capabilities":{"tools":{"listChanged":false},"tasks":{"list":{},"cancel":{},"requests":{"tools":{"call":{}}}}},
                    "serverInfo":{"name":"mock-legacy-task-server","version":"0.1.0"}
                }
            }),
            "tools/list" => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}})
            }
            "tasks/list" => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tasks":tasks.values().map(|entry| entry.0.clone()).collect::<Vec<_>>()}})
            }
            "tools/call" if request["params"].get("task").is_some() => {
                next_id += 1;
                let task_id = format!("child-task-{next_id}");
                let now = rmcp::task_manager::current_timestamp();
                let task = serde_json::json!({"taskId":task_id,"status":"working","createdAt":now,"lastUpdatedAt":now,"ttl":60000,"pollInterval":25});
                let input = request["params"]["arguments"]["input"]
                    .as_str()
                    .unwrap_or_default();
                let payload = serde_json::json!({"content":[{"type":"text","text":format!("child-task {input}")}],"isError":false});
                tasks.insert(task_id, (task.clone(), payload));
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"task":task}})
            }
            "tasks/get" => {
                let task_id = request["params"]["taskId"].as_str().unwrap_or_default();
                let entry = tasks.get_mut(task_id).expect("known child task");
                entry.0["status"] = serde_json::json!("completed");
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":entry.0})
            }
            "tasks/result" => {
                let task_id = request["params"]["taskId"].as_str().unwrap_or_default();
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":tasks.get(task_id).expect("known child task").1})
            }
            "tasks/cancel" => {
                let task_id = request["params"]["taskId"].as_str().unwrap_or_default();
                let entry = tasks.get_mut(task_id).expect("known child task");
                entry.0["status"] = serde_json::json!("cancelled");
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":entry.0})
            }
            _ => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":method}})
            }
        };
        stdout
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

struct MockServer {
    tool_names: Vec<String>,
    delay: std::time::Duration,
    fail_mode: String,
    reverse_request: String,
    resources: bool,
    resource_templates: bool,
    prompts: bool,
    completions: bool,
    list_fail_flag_file: Option<String>,
    list_empty_flag_file: Option<String>,
    list_delay: std::time::Duration,
}

impl MockServer {
    fn build_tool(name: &str) -> Tool {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Input argument"
                }
            }
        });
        Tool::new(
            Cow::Owned(name.to_string()),
            Cow::Owned(format!("Mock tool: {name}")),
            Arc::new(rmcp::model::object(schema)),
        )
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for MockServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        capabilities.tools = Some(tools);
        if self.resources {
            let mut resources = ResourcesCapability::default();
            resources.subscribe = Some(true);
            resources.list_changed = Some(true);
            capabilities.resources = Some(resources);
        }
        if self.prompts {
            let mut prompts = PromptsCapability::default();
            prompts.list_changed = Some(false);
            capabilities.prompts = Some(prompts);
        }
        if self.completions {
            capabilities.completions = Some(serde_json::Map::new());
        }

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("mock-mcp-server", "0.1.0"))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let tools: Vec<Tool> = self
                .tool_names
                .iter()
                .map(|name| Self::build_tool(name))
                .collect();

            Ok(ListToolsResult::with_all_items(tools))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        async move {
            eprintln!("mock-mcp-server: call_tool {}", request.name);

            match self.fail_mode.as_str() {
                "crash" => {
                    eprintln!("mock-mcp-server: crash mode, exiting");
                    std::process::exit(1);
                }
                "timeout" => {
                    eprintln!("mock-mcp-server: timeout mode, hanging forever");
                    std::future::pending::<()>().await;
                }
                _ => {}
            }

            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }

            if request.name == "structured" {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "tool": "structured",
                    "ok": true,
                    "count": 2
                }))
                .into());
            }

            if request.name == "resource_link" {
                return Ok(CallToolResult::success(vec![ContentBlock::resource_link(
                    Resource::new("file:///tmp/mock-resource.txt", "mock-resource.txt")
                        .with_title("Mock Resource")
                        .with_description("Structured resource link test fixture")
                        .with_mime_type("text/plain")
                        .with_size(17),
                )])
                .into());
            }

            if request.name == "artifact_text" {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "A".repeat(18 * 1024 * 1024),
                )])
                .into());
            }

            if request.name == "chunked_text" {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "B".repeat(6 * 1024 * 1024),
                )])
                .into());
            }

            if request.name == "attachment_blob" {
                let raw = vec![0x5a_u8; 3_600_000];
                let content = base64::engine::general_purpose::STANDARD.encode(raw);
                let payload = serde_json::json!({
                    "file_id": "FTEST123",
                    "filename": "deck.pdf",
                    "mimetype": "application/pdf",
                    "size": 3_600_000,
                    "encoding": "base64",
                    "content": content,
                });
                return Ok(
                    CallToolResult::success(vec![ContentBlock::text(payload.to_string())]).into(),
                );
            }

            let args_str = match &request.arguments {
                Some(args) => serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                None => "{}".to_string(),
            };

            let mut response_text = format!("Called {} with {}", request.name, args_str);

            match self.reverse_request.as_str() {
                "elicitation" => {
                    eprintln!("mock-mcp-server: sending elicitation reverse request");
                    let schema = ElicitationSchema::new(BTreeMap::new());
                    let params = ElicitRequestParams::FormElicitationParams {
                        meta: None,
                        message: "mock elicitation request".to_string(),
                        requested_schema: schema,
                    };
                    match context.peer.create_elicitation(params).await {
                        Ok(result) => {
                            response_text
                                .push_str(&format!(" reverse=elicitation:{:?}", result.action));
                        }
                        Err(e) => {
                            response_text.push_str(&format!(" reverse=elicitation:error:{e}"));
                        }
                    }
                }
                "sampling" => {
                    eprintln!("mock-mcp-server: sending sampling reverse request");
                    let params = CreateMessageRequestParams::new(
                        vec![SamplingMessage::user_text("mock sampling request")],
                        100,
                    );
                    match context.peer.create_message(params).await {
                        Ok(result) => {
                            response_text
                                .push_str(&format!(" reverse=sampling:model={}", result.model));
                        }
                        Err(e) => {
                            response_text.push_str(&format!(" reverse=sampling:error:{e}"));
                        }
                    }
                }
                _ => {}
            }

            Ok(CallToolResult::success(vec![ContentBlock::text(response_text)]).into())
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move {
            // Hang on resource listing so tests can exercise the per-server
            // listing timeout in refresh_tools (a connected-but-stalled
            // upstream must not block the whole catalog refresh).
            if self.fail_mode == "timeout" {
                eprintln!("mock-mcp-server: timeout mode, hanging on list_resources");
                std::future::pending::<()>().await;
            }
            if !self.list_delay.is_zero() {
                tokio::time::sleep(self.list_delay).await;
            }
            // Test-driven transient failure: error while the flag file exists.
            if let Some(path) = &self.list_fail_flag_file
                && std::path::Path::new(path).exists()
            {
                eprintln!("mock-mcp-server: list_fail flag set, returning error");
                return Err(McpError::internal_error(
                    "mock list_resources transient failure (flag set)",
                    None,
                ));
            }
            // Test-driven genuine removal: empty success while the flag exists.
            if let Some(path) = &self.list_empty_flag_file
                && std::path::Path::new(path).exists()
            {
                eprintln!("mock-mcp-server: list_empty flag set, returning empty list");
                return Ok(ListResourcesResult::with_all_items(vec![]));
            }
            if !self.resources {
                return Ok(ListResourcesResult::with_all_items(vec![]));
            }
            Ok(ListResourcesResult::with_all_items(vec![
                Resource::new("file:///tmp/mock-resource.txt", "mock-resource.txt")
                    .with_title("Mock Resource")
                    .with_description("Subscribable mock resource")
                    .with_mime_type("text/plain"),
            ]))
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        async move {
            if !self.resources || request.uri != "file:///tmp/mock-resource.txt" {
                return Err(McpError::resource_not_found(
                    format!("resource not found: {}", request.uri),
                    None,
                ));
            }
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                "mock resource contents",
                request.uri,
            )])
            .into())
        }
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            if !self.resources || request.uri != "file:///tmp/mock-resource.txt" {
                return Err(McpError::resource_not_found(
                    format!("resource not found: {}", request.uri),
                    None,
                ));
            }
            let uri = request.uri;
            let peer = context.peer;
            tokio::spawn(async move {
                let _ = peer
                    .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                    .await;
            });
            Ok(())
        }
    }

    fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move { Ok(()) }
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        async move {
            if !self.list_delay.is_zero() {
                tokio::time::sleep(self.list_delay).await;
            }
            if !self.resource_templates {
                return Ok(ListResourceTemplatesResult::with_all_items(vec![]));
            }
            Ok(ListResourceTemplatesResult::with_all_items(vec![
                ResourceTemplate::new("file:///tmp/mock-templates/{id}.txt", "mock_template")
                    .with_title("Mock Template")
                    .with_description("Mock resource template")
                    .with_mime_type("text/plain"),
            ]))
        }
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move {
            if !self.list_delay.is_zero() {
                tokio::time::sleep(self.list_delay).await;
            }
            if !self.prompts {
                return Ok(ListPromptsResult::with_all_items(vec![]));
            }
            Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
                "mock_prompt",
                Some("Mock prompt fixture"),
                Some(vec![
                    PromptArgument::new("topic").with_description("Topic to expand"),
                ]),
            )]))
        }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        async move {
            if !self.prompts || request.name != "mock_prompt" {
                return Err(McpError::invalid_params(
                    format!("prompt not found: {}", request.name),
                    None,
                ));
            }
            Ok(GetPromptResult::new(vec![PromptMessage::new(
                Role::User,
                ContentBlock::text("mock prompt body"),
            )])
            .with_description("Mock prompt fixture")
            .into())
        }
    }

    fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move {
            // Gate on the capability flag, consistent with list_prompts /
            // get_prompt / list_resource_templates — so an upstream that did not
            // advertise completions does not silently answer completion requests.
            if !self.completions {
                return Err(McpError::invalid_request(
                    "completions capability not enabled",
                    None,
                ));
            }
            let completion = CompletionInfo::with_all_values(vec!["mock_completion".to_string()])
                .expect("single completion value is within the MCP max");
            Ok(CompleteResult::new(completion))
        }
    }
}

/// Minimal 1x1 red PNG for official suite image/binary rows.
const FIXTURE_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

struct OfficialModernFixture;

impl OfficialModernFixture {
    fn tool(name: &str, description: &str, schema: serde_json::Value) -> Tool {
        Tool::new(
            Cow::Owned(name.to_string()),
            Cow::Owned(description.to_string()),
            Arc::new(rmcp::model::object(schema)),
        )
    }

    fn empty_object_schema() -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for OfficialModernFixture {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        capabilities.tools = Some(tools);
        let mut resources = ResourcesCapability::default();
        resources.subscribe = Some(false);
        resources.list_changed = Some(false);
        capabilities.resources = Some(resources);
        let mut prompts = PromptsCapability::default();
        prompts.list_changed = Some(false);
        capabilities.prompts = Some(prompts);
        capabilities.completions = Some(serde_json::Map::new());
        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("official-modern-fixture", "0.1.0"))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let tools = vec![
                Self::tool(
                    "test_simple_text",
                    "Official suite simple text tool",
                    Self::empty_object_schema(),
                ),
                Self::tool(
                    "test_error_handling",
                    "Official suite intentional error tool",
                    Self::empty_object_schema(),
                ),
                Self::tool(
                    "test_image_content",
                    "Official suite image content tool",
                    Self::empty_object_schema(),
                ),
                Self::tool(
                    "test_audio_content",
                    "Official suite audio content tool",
                    Self::empty_object_schema(),
                ),
                Self::tool(
                    "test_embedded_resource",
                    "Official suite embedded resource tool",
                    Self::empty_object_schema(),
                ),
                Self::tool(
                    "test_multiple_content_types",
                    "Official suite mixed content tool",
                    Self::empty_object_schema(),
                ),
            ];
            Ok(ListToolsResult::with_all_items(tools))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        async move {
            match request.name.as_ref() {
                "test_simple_text" => Ok(CallToolResult::success(vec![ContentBlock::text(
                    "This is a simple text response for testing.",
                )])
                .into()),
                "test_error_handling" => Ok(CallToolResult::error(vec![ContentBlock::text(
                    "This tool intentionally returns an error for testing",
                )])
                .into()),
                "test_image_content" => Ok(CallToolResult::success(vec![ContentBlock::image(
                    FIXTURE_PNG_B64,
                    "image/png",
                )])
                .into()),
                "test_audio_content" => {
                    // Tiny valid-enough WAV header + silence for suite shape checks.
                    let wav = base64::engine::general_purpose::STANDARD.encode([
                        0x52, 0x49, 0x46, 0x46, 0x24, 0x00, 0x00, 0x00, 0x57, 0x41, 0x56, 0x45,
                        0x66, 0x6d, 0x74, 0x20, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
                        0x44, 0xac, 0x00, 0x00, 0x88, 0x58, 0x01, 0x00, 0x02, 0x00, 0x10, 0x00,
                        0x64, 0x61, 0x74, 0x61, 0x00, 0x00, 0x00, 0x00,
                    ]);
                    Ok(CallToolResult::success(vec![ContentBlock::audio(wav, "audio/wav")]).into())
                }
                "test_embedded_resource" => {
                    let resource = ResourceContents::text(
                        "This is an embedded resource content.",
                        "test://embedded-resource",
                    )
                    .with_mime_type("text/plain");
                    Ok(CallToolResult::success(vec![ContentBlock::resource(resource)]).into())
                }
                "test_multiple_content_types" => {
                    let resource = ResourceContents::text(
                        r#"{"test":"data","value":123}"#,
                        "test://mixed-content-resource",
                    )
                    .with_mime_type("application/json");
                    Ok(CallToolResult::success(vec![
                        ContentBlock::text("Multiple content types test:"),
                        ContentBlock::image(FIXTURE_PNG_B64, "image/png"),
                        ContentBlock::resource(resource),
                    ])
                    .into())
                }
                other => Err(McpError::invalid_params(
                    format!("unknown official fixture tool: {other}"),
                    None,
                )),
            }
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move {
            let resources = vec![
                Resource::new("test://static-text", "static-text").with_mime_type("text/plain"),
                Resource::new("test://static-binary", "static-binary").with_mime_type("image/png"),
            ];
            Ok(ListResourcesResult::with_all_items(resources))
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        async move {
            match request.uri.as_str() {
                "test://static-text" => Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(
                        "This is the content of the static text resource.",
                        "test://static-text",
                    )
                    .with_mime_type("text/plain"),
                ])
                .into()),
                "test://static-binary" => Ok(ReadResourceResult::new(vec![
                    ResourceContents::blob(FIXTURE_PNG_B64, "test://static-binary")
                        .with_mime_type("image/png"),
                ])
                .into()),
                other => Err(McpError::resource_not_found(
                    format!("unknown official fixture resource: {other}"),
                    Some(serde_json::json!({"uri": other})),
                )),
            }
        }
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move {
            let prompts = vec![
                Prompt::new(
                    "test_simple_prompt",
                    Some("Official suite simple prompt"),
                    None,
                ),
                Prompt::new(
                    "test_prompt_with_arguments",
                    Some("Official suite prompt with arguments"),
                    Some(vec![
                        PromptArgument::new("arg1")
                            .with_description("First test argument")
                            .with_required(true),
                        PromptArgument::new("arg2")
                            .with_description("Second test argument")
                            .with_required(true),
                    ]),
                ),
                Prompt::new(
                    "test_prompt_with_image",
                    Some("Official suite prompt with image"),
                    None,
                ),
                Prompt::new(
                    "test_prompt_with_embedded_resource",
                    Some("Official suite prompt with embedded resource"),
                    Some(vec![
                        PromptArgument::new("resourceUri")
                            .with_description("URI of the resource to embed")
                            .with_required(true),
                    ]),
                ),
            ];
            Ok(ListPromptsResult::with_all_items(prompts))
        }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        async move {
            match request.name.as_str() {
                "test_simple_prompt" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                    Role::User,
                    "This is a simple prompt for testing.",
                )])
                .into()),
                "test_prompt_with_arguments" => {
                    let args = request.arguments.unwrap_or_default();
                    let arg1 = args
                        .get("arg1")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let arg2 = args
                        .get("arg2")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                        Role::User,
                        format!("Prompt with arguments: arg1='{arg1}', arg2='{arg2}'"),
                    )])
                    .into())
                }
                "test_prompt_with_image" => Ok(GetPromptResult::new(vec![
                    PromptMessage::new(
                        Role::User,
                        ContentBlock::image(FIXTURE_PNG_B64, "image/png"),
                    ),
                    PromptMessage::new_text(Role::User, "Please analyze the image above."),
                ])
                .into()),
                "test_prompt_with_embedded_resource" => {
                    let args = request.arguments.unwrap_or_default();
                    let uri = args
                        .get("resourceUri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("test://static-text")
                        .to_string();
                    Ok(GetPromptResult::new(vec![
                        PromptMessage::new_resource(
                            Role::User,
                            uri,
                            Some("text/plain".to_string()),
                            Some("Embedded resource content for testing.".to_string()),
                            None,
                            None,
                            None,
                        ),
                        PromptMessage::new_text(
                            Role::User,
                            "Please process the embedded resource above.",
                        ),
                    ])
                    .into())
                }
                other => Err(McpError::invalid_params(
                    format!("unknown official fixture prompt: {other}"),
                    None,
                )),
            }
        }
    }

    fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move {
            let completion = CompletionInfo::with_all_values(vec![
                "paris".to_string(),
                "park".to_string(),
                "party".to_string(),
            ])
            .expect("three completion values within MCP max");
            Ok(CompleteResult::new(completion))
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.legacy_tasks {
        return serve_legacy_tasks_stdio().await;
    }
    if args.official_modern_fixture {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_env("MOCK_LOG")
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .compact()
            .init();
        tracing::info!("starting official modern conformance fixture server");
        let transport = rmcp::transport::io::stdio();
        let service = OfficialModernFixture.serve(transport).await?;
        service.waiting().await?;
        return Ok(());
    }
    if matches!(args.lifecycle.as_str(), "legacy-only" | "modern-only") {
        return serve_lifecycle_stdio(&args.lifecycle, args.request_log_file.as_deref()).await;
    }
    anyhow::ensure!(
        args.lifecycle == "rmcp",
        "--lifecycle must be rmcp, legacy-only, or modern-only"
    );

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MOCK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    let tool_names: Vec<String> = args
        .tools
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    tracing::info!(
        tools = ?tool_names,
        delay_ms = args.delay_ms,
        fail_mode = %args.fail_mode,
        "starting mock MCP server"
    );

    let server = MockServer {
        tool_names,
        delay: std::time::Duration::from_millis(args.delay_ms),
        fail_mode: args.fail_mode,
        reverse_request: args.reverse_request,
        resources: args.resources,
        resource_templates: args.resource_templates,
        prompts: args.prompts,
        completions: args.completions,
        list_fail_flag_file: args.list_fail_flag_file,
        list_empty_flag_file: args.list_empty_flag_file,
        list_delay: std::time::Duration::from_millis(args.list_delay_ms),
    };

    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}
