#![forbid(unsafe_code)]

use std::sync::Arc;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "plug", version, about = "MCP multiplexer — one config, every client connected")]
struct Cli {
    /// Path to config file
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Increase verbosity (-v for debug, -vv for trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Output format
    #[arg(long, global = true, default_value = "text")]
    output: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP stdio bridge (what clients invoke)
    Connect,
    /// Start the HTTP server for web-based MCP clients
    Serve {
        /// Also start stdio bridge on stdin/stdout
        #[arg(long)]
        stdio: bool,
    },
    /// Show server health status
    Status,
    /// Server management
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Tool management
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },
    /// Validate configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Subcommand)]
enum ServerCommands {
    /// List configured servers
    List,
}

#[derive(Subcommand)]
enum ToolCommands {
    /// List all available tools
    List,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate the config file
    Validate,
    /// Show the config file path
    Path,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Set up tracing — all output to stderr (stdout is MCP protocol only)
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("PLUG_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .compact()
        .init();

    match cli.command {
        Commands::Connect => {
            cmd_connect(cli.config.as_ref()).await?;
        }
        Commands::Serve { stdio } => {
            cmd_serve(cli.config.as_ref(), stdio).await?;
        }
        Commands::Status => {
            cmd_status(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Server { command } => match command {
            ServerCommands::List => {
                cmd_server_list(cli.config.as_ref(), &cli.output).await?;
            }
        },
        Commands::Tool { command } => match command {
            ToolCommands::List => {
                cmd_tool_list(cli.config.as_ref(), &cli.output).await?;
            }
        },
        Commands::Config { command } => match command {
            ConfigCommands::Validate => {
                let config = plug_core::config::load_config(cli.config.as_ref());
                match config {
                    Ok(cfg) => {
                        let errors = plug_core::config::validate_config(&cfg);
                        if errors.is_empty() {
                            eprintln!("config is valid");
                        } else {
                            for err in &errors {
                                eprintln!("error: {err}");
                            }
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("config error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            ConfigCommands::Path => {
                let path = cli
                    .config
                    .unwrap_or_else(plug_core::config::default_config_path);
                println!("{}", path.display());
            }
        },
    }

    Ok(())
}

/// Build a `RouterConfig` from the global `Config`.
fn router_config(config: &plug_core::config::Config) -> plug_core::proxy::RouterConfig {
    plug_core::proxy::RouterConfig {
        prefix_delimiter: config.prefix_delimiter.clone(),
        priority_tools: config.priority_tools.clone(),
        tool_description_max_chars: config.tool_description_max_chars,
        tool_search_threshold: config.tool_search_threshold,
        tool_filter_enabled: config.tool_filter_enabled,
    }
}

/// `plug connect` — the primary stdio bridge mode.
///
/// Creates an Engine, starts it, builds a ProxyHandler from the Engine's
/// ToolRouter, then serves MCP over stdin/stdout using rmcp's stdio transport.
/// Handles SIGINT/SIGTERM for graceful shutdown via Engine.
async fn cmd_connect(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    let errors = plug_core::config::validate_config(&config);
    if !errors.is_empty() {
        for err in &errors {
            tracing::error!("{err}");
        }
        anyhow::bail!("config validation failed with {} error(s)", errors.len());
    }

    let engine = plug_core::engine::Engine::new(config);
    engine.start().await?;

    // Build the proxy handler from Engine's ToolRouter
    let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());

    tracing::info!("starting stdio bridge");

    // Serve MCP over stdin/stdout
    use rmcp::ServiceExt as _;
    let transport = rmcp::transport::io::stdio();
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("failed to start MCP service: {e}"))?;

    // Wait for either the client to disconnect or a shutdown signal
    tokio::select! {
        result = service.waiting() => {
            tracing::info!("client disconnected");
            if let Err(e) = result {
                tracing::error!(error = %e, "service error");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received shutdown signal");
        }
    }

    tracing::info!("shutting down");
    engine.shutdown().await;

    Ok(())
}

/// `plug status` — show health of all upstream servers.
async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    let server_manager = Arc::new(plug_core::server::ServerManager::new());
    server_manager.start_all(&config).await?;

    let statuses = server_manager.server_statuses();

    match output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&statuses)?);
        }
        OutputFormat::Text => {
            if statuses.is_empty() {
                println!("no servers configured");
            } else {
                // Print table header
                println!("{:<20} {:<10} {:<6}", "NAME", "STATUS", "TOOLS");
                for status in &statuses {
                    let health = format!("{:?}", status.health);
                    println!(
                        "{:<20} {:<10} {:<6}",
                        status.server_id, health, status.tool_count
                    );
                }
            }
        }
    }

    server_manager.shutdown_all().await;
    Ok(())
}

/// `plug server list` — list configured servers (does not start them).
async fn cmd_server_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    match output {
        OutputFormat::Json => {
            let servers: Vec<serde_json::Value> = config
                .servers
                .iter()
                .map(|(name, srv)| {
                    serde_json::json!({
                        "name": name,
                        "transport": format!("{:?}", srv.transport).to_lowercase(),
                        "command": srv.command,
                        "url": srv.url,
                        "enabled": srv.enabled,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&servers)?);
        }
        OutputFormat::Text => {
            if config.servers.is_empty() {
                println!("no servers configured");
            } else {
                let header = "COMMAND/URL";
                println!(
                    "{:<20} {:<10} {:<10} {}",
                    "NAME", "TRANSPORT", "STATUS", header
                );
                let mut names: Vec<&String> = config.servers.keys().collect();
                names.sort();
                for name in names {
                    let srv = &config.servers[name];
                    let transport = format!("{:?}", srv.transport).to_lowercase();
                    let status = if srv.enabled { "enabled" } else { "disabled" };
                    let target = match srv.transport {
                        plug_core::config::TransportType::Stdio => {
                            srv.command.as_deref().unwrap_or("(no command)")
                        }
                        plug_core::config::TransportType::Http => {
                            srv.url.as_deref().unwrap_or("(no url)")
                        }
                    };
                    println!("{:<20} {:<10} {:<10} {}", name, transport, status, target);
                }
            }
        }
    }

    Ok(())
}

/// `plug serve` — start the HTTP server for web-based MCP clients.
///
/// Creates an Engine, starts it, builds HttpState from the Engine's
/// ToolRouter, and serves MCP over HTTP (Streamable HTTP transport).
/// Optionally also runs the stdio bridge via `--stdio`.
async fn cmd_serve(
    config_path: Option<&std::path::PathBuf>,
    with_stdio: bool,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    let errors = plug_core::config::validate_config(&config);
    if !errors.is_empty() {
        for err in &errors {
            tracing::error!("{err}");
        }
        anyhow::bail!("config validation failed with {} error(s)", errors.len());
    }

    // Warn on non-loopback bind address
    let bind_addr = &config.http.bind_address;
    if bind_addr != "127.0.0.1" && bind_addr != "::1" && bind_addr != "localhost" {
        tracing::warn!(
            bind_address = %bind_addr,
            "binding to non-loopback address — ensure this is intentional"
        );
    }

    let engine = plug_core::engine::Engine::new(config.clone());
    engine.start().await?;

    let cancel = engine.cancel_token().clone();

    // Build HTTP server state (SessionManager is transport-specific, stays outside Engine)
    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: engine.tool_router().clone(),
        sessions: plug_core::http::session::SessionManager::new(
            config.http.session_timeout_secs,
            config.http.max_sessions,
        ),
        cancel: cancel.clone(),
        sse_channel_capacity: config.http.sse_channel_capacity,
    });

    // Start session cleanup background task
    http_state.sessions.spawn_cleanup_task(cancel.clone());

    let axum_router = plug_core::http::server::build_router(http_state);
    let listen_addr = format!("{}:{}", config.http.bind_address, config.http.port);
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind {listen_addr}: {e}"))?;

    tracing::info!("HTTP server listening on http://{listen_addr}");

    if with_stdio {
        // Run HTTP server + stdio bridge simultaneously
        let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());

        use rmcp::ServiceExt as _;
        let transport = rmcp::transport::io::stdio();
        let service = proxy
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("failed to start stdio service: {e}"))?;

        tracing::info!("stdio bridge active");

        tokio::select! {
            result = axum::serve(listener, axum_router)
                .with_graceful_shutdown(cancel.clone().cancelled_owned()) =>
            {
                if let Err(e) = result {
                    tracing::error!(error = %e, "HTTP server error");
                }
            }
            result = service.waiting() => {
                tracing::info!("stdio client disconnected");
                if let Err(e) = result {
                    tracing::error!(error = %e, "stdio service error");
                }
                cancel.cancel();
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received shutdown signal");
                cancel.cancel();
            }
        }
    } else {
        // HTTP server only
        tokio::select! {
            result = axum::serve(listener, axum_router)
                .with_graceful_shutdown(cancel.clone().cancelled_owned()) =>
            {
                if let Err(e) = result {
                    tracing::error!(error = %e, "HTTP server error");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received shutdown signal");
                cancel.cancel();
            }
        }
    }

    tracing::info!("shutting down");
    engine.shutdown().await;

    Ok(())
}

/// `plug tool list` — list all available tools from upstream servers.
async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    let server_manager = Arc::new(plug_core::server::ServerManager::new());
    server_manager.start_all(&config).await?;

    // Use ProxyHandler to get the prefixed tool list
    let proxy = plug_core::proxy::ProxyHandler::new(
        server_manager.clone(),
        router_config(&config),
    );
    proxy.refresh_tools().await;

    let tools = server_manager.get_tools().await;

    match output {
        OutputFormat::Json => {
            let tool_list: Vec<serde_json::Value> = tools
                .iter()
                .map(|(server_name, tool)| {
                    let prefixed = format!(
                        "{}{}{}",
                        server_name, config.prefix_delimiter, tool.name
                    );
                    serde_json::json!({
                        "name": prefixed,
                        "server": server_name,
                        "description": tool.description,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&tool_list)?);
        }
        OutputFormat::Text => {
            if tools.is_empty() {
                println!("no tools available");
            } else {
                let desc_header = "DESCRIPTION";
                println!(
                    "{:<40} {:<20} {}",
                    "NAME", "SERVER", desc_header
                );
                for (server_name, tool) in &tools {
                    let prefixed = format!(
                        "{}{}{}",
                        server_name, config.prefix_delimiter, tool.name
                    );
                    let desc = tool
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    println!("{:<40} {:<20} {}", prefixed, server_name, desc);
                }
                println!("\ntotal: {} tools", tools.len());
            }
        }
    }

    server_manager.shutdown_all().await;
    Ok(())
}
