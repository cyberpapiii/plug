#![forbid(unsafe_code)]
// Exercise the complete MCP 2025-11-25 surface even where RMCP 2.2 marks
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
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            eprintln!("mock-mcp-server: call_tool {}", request.name);

            // Handle fail modes
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

            // Apply delay
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }

            if request.name == "structured" {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "tool": "structured",
                    "ok": true,
                    "count": 2
                })));
            }

            if request.name == "resource_link" {
                return Ok(CallToolResult::success(vec![ContentBlock::resource_link(
                    Resource::new("file:///tmp/mock-resource.txt", "mock-resource.txt")
                        .with_title("Mock Resource")
                        .with_description("Structured resource link test fixture")
                        .with_mime_type("text/plain")
                        .with_size(17),
                )]));
            }

            if request.name == "artifact_text" {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "A".repeat(18 * 1024 * 1024),
                )]));
            }

            if request.name == "chunked_text" {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "B".repeat(6 * 1024 * 1024),
                )]));
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
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    payload.to_string(),
                )]));
            }

            let args_str = match &request.arguments {
                Some(args) => serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string()),
                None => "{}".to_string(),
            };

            let mut response_text = format!("Called {} with {}", request.name, args_str);

            // Handle reverse requests
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

            Ok(CallToolResult::success(vec![ContentBlock::text(
                response_text,
            )]))
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
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
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
            )]))
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
    ) -> impl Future<Output = Result<GetPromptResult, McpError>> + Send + '_ {
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
            .with_description("Mock prompt fixture"))
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Set up tracing to stderr
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
