#![forbid(unsafe_code)]

//! Mock MCP server for integration testing.
//!
//! A real MCP server binary that exposes configurable tools via stdio transport.
//! Each tool returns a text response echoing the arguments it was called with.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

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
}

struct MockServer {
    tool_names: Vec<String>,
    delay: std::time::Duration,
    fail_mode: String,
    reverse_request: String,
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
    };

    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}
