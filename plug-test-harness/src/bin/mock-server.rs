#![forbid(unsafe_code)]

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
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(false),
        });
        if self.resources {
            capabilities.resources = Some(ResourcesCapability {
                subscribe: Some(true),
                list_changed: Some(true),
            });
        }
        if self.prompts {
            capabilities.prompts = Some(PromptsCapability {
                list_changed: Some(false),
            });
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
                return Ok(CallToolResult::success(vec![Content::resource_link(
                    RawResource {
                        uri: "file:///tmp/mock-resource.txt".to_string(),
                        name: "mock-resource.txt".to_string(),
                        title: Some("Mock Resource".to_string()),
                        description: Some("Structured resource link test fixture".to_string()),
                        mime_type: Some("text/plain".to_string()),
                        size: Some(17),
                        icons: None,
                        meta: None,
                    },
                )]));
            }

            if request.name == "artifact_text" {
                return Ok(CallToolResult::success(vec![Content::text(
                    "A".repeat(18 * 1024 * 1024),
                )]));
            }

            if request.name == "chunked_text" {
                return Ok(CallToolResult::success(vec![Content::text(
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
                return Ok(CallToolResult::success(vec![Content::text(
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
                    let params = CreateElicitationRequestParams::FormElicitationParams {
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

            Ok(CallToolResult::success(vec![Content::text(response_text)]))
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
            // Test-driven transient failure: error while the flag file exists.
            if let Some(path) = &self.list_fail_flag_file {
                if std::path::Path::new(path).exists() {
                    eprintln!("mock-mcp-server: list_fail flag set, returning error");
                    return Err(McpError::internal_error(
                        "mock list_resources transient failure (flag set)",
                        None,
                    ));
                }
            }
            // Test-driven genuine removal: empty success while the flag exists.
            if let Some(path) = &self.list_empty_flag_file {
                if std::path::Path::new(path).exists() {
                    eprintln!("mock-mcp-server: list_empty flag set, returning empty list");
                    return Ok(ListResourcesResult::with_all_items(vec![]));
                }
            }
            if !self.resources {
                return Ok(ListResourcesResult::with_all_items(vec![]));
            }
            Ok(ListResourcesResult::with_all_items(vec![Resource::new(
                RawResource::new("file:///tmp/mock-resource.txt", "mock-resource.txt")
                    .with_title("Mock Resource")
                    .with_description("Subscribable mock resource")
                    .with_mime_type("text/plain"),
                None,
            )]))
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
            if !self.resource_templates {
                return Ok(ListResourceTemplatesResult::with_all_items(vec![]));
            }
            Ok(ListResourceTemplatesResult::with_all_items(vec![
                RawResourceTemplate::new("file:///tmp/mock-templates/{id}.txt", "mock_template")
                    .with_title("Mock Template")
                    .with_description("Mock resource template")
                    .with_mime_type("text/plain")
                    .no_annotation(),
            ]))
        }
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move {
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
            Ok(
                GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    "mock prompt body",
                )])
                .with_description("Mock prompt fixture"),
            )
        }
    }

    fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move {
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
    };

    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}
