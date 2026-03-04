#![forbid(unsafe_code)]

mod daemon;
mod tui;

use std::sync::Arc;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "plug",
    version,
    about = "MCP multiplexer — one config, every client connected"
)]
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
        /// Run as headless daemon with IPC socket
        #[arg(long)]
        daemon: bool,
    },
    /// Launch the TUI dashboard
    Tui,
    /// Show server health status (queries daemon if running)
    Status,
    /// Daemon management
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
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
    /// Import MCP servers from AI client configs
    Import {
        /// Only scan specific clients (comma-separated: claude-desktop,cursor,vscode,...)
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        /// Don't modify config — just show what would be imported
        #[arg(long)]
        dry_run: bool,
    },
    /// Export plug config snippet for a target client
    Export {
        /// Target client (claude-desktop, claude-code, cursor, windsurf, vscode, gemini-cli, codex-cli, opencode, zed, cline, factory, nanobot)
        target: String,
        /// Use HTTP transport instead of stdio
        #[arg(long)]
        http: bool,
        /// HTTP port (default: from config or 3282)
        #[arg(long, default_value = "3282")]
        port: u16,
    },
    /// Run diagnostic checks on your plug setup
    Doctor,
    /// Reload config from disk (sends reload signal to daemon)
    Reload,
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Stop the running daemon
    Stop,
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

    // Initialize tracing based on command:
    // - daemon mode logs to file
    // - all other commands log to stderr (stdout is protocol output)
    let daemon_mode = matches!(&cli.command, Commands::Serve { daemon: true, .. });
    let _daemon_log_guard = if daemon_mode {
        Some(daemon::setup_file_logging(&daemon::log_dir())?)
    } else {
        init_stderr_tracing(cli.verbose);
        None
    };

    match cli.command {
        Commands::Connect => {
            cmd_connect(cli.config.as_ref()).await?;
        }
        Commands::Serve { stdio, daemon } => {
            if daemon {
                cmd_daemon(cli.config.as_ref()).await?;
            } else {
                cmd_serve(cli.config.as_ref(), stdio).await?;
            }
        }
        Commands::Tui => {
            cmd_tui(cli.config.as_ref()).await?;
        }
        Commands::Status => {
            cmd_status(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Daemon { command } => match command {
            DaemonCommands::Stop => {
                cmd_daemon_stop().await?;
            }
        },
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
        Commands::Import { clients, dry_run } => {
            cmd_import(cli.config.as_ref(), clients, dry_run, &cli.output)?;
        }
        Commands::Export { target, http, port } => {
            cmd_export(&target, http, port)?;
        }
        Commands::Doctor => {
            cmd_doctor(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Reload => {
            cmd_reload().await?;
        }
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

fn init_stderr_tracing(verbose: u8) {
    let level = match verbose {
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

/// `plug tui` — launch the TUI dashboard.
///
/// Creates an Engine, starts it, then runs the TUI event loop.
async fn cmd_tui(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
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

    tui::run(&engine).await?;

    tracing::info!("shutting down");
    engine.shutdown().await;

    Ok(())
}

/// `plug status` — show health of all upstream servers.
///
/// Tries to query a running daemon via IPC first. If no daemon is running,
/// falls back to starting servers directly and querying them.
async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    // Try daemon IPC first
    if let Ok(response) = daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await {
        match response {
            plug_core::ipc::IpcResponse::Status {
                servers,
                clients,
                uptime_secs,
            } => {
                match output {
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "source": "daemon",
                                "uptime_secs": uptime_secs,
                                "clients": clients,
                                "servers": servers,
                            }))?
                        );
                    }
                    OutputFormat::Text => {
                        println!(
                            "connected to daemon (uptime: {}s, clients: {})",
                            uptime_secs, clients
                        );
                        if servers.is_empty() {
                            println!("no servers configured");
                        } else {
                            println!("{:<20} {:<10} {:<6}", "NAME", "STATUS", "TOOLS");
                            for status in &servers {
                                let health = format!("{:?}", status.health);
                                println!(
                                    "{:<20} {:<10} {:<6}",
                                    status.server_id, health, status.tool_count
                                );
                            }
                        }
                    }
                }
                return Ok(());
            }
            plug_core::ipc::IpcResponse::Error { message, .. } => {
                tracing::debug!(error = %message, "daemon status query failed, falling back");
            }
            _ => {}
        }
    }

    // No daemon running — show configured servers from config only
    let config = plug_core::config::load_config(config_path)?;

    match output {
        OutputFormat::Json => {
            let servers: Vec<serde_json::Value> = config
                .servers
                .keys()
                .map(|name| serde_json::json!({"name": name, "status": "not_running"}))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "source": "config",
                    "daemon_running": false,
                    "servers": servers,
                }))?
            );
        }
        OutputFormat::Text => {
            eprintln!("no daemon running — showing configured servers");
            if config.servers.is_empty() {
                println!("no servers configured");
            } else {
                let mut names: Vec<&String> = config.servers.keys().collect();
                names.sort();
                println!("{:<20} {:<10}", "NAME", "STATUS");
                for name in names {
                    println!("{:<20} {:<10}", name, "not_running");
                }
            }
        }
    }

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

/// `plug serve --daemon` — start headless daemon with IPC socket.
///
/// Creates an Engine, starts it, sets up file logging, then runs the
/// Unix socket IPC listener until shutdown.
async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;

    let errors = plug_core::config::validate_config(&config);
    if !errors.is_empty() {
        for err in &errors {
            tracing::error!("{err}");
        }
        anyhow::bail!("config validation failed with {} error(s)", errors.len());
    }

    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;

    let cancel = engine.cancel_token().clone();

    // Run daemon IPC listener + signal handler
    tokio::select! {
        result = daemon::run_daemon(engine.clone()) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "daemon error");
            }
        }
        _ = daemon::shutdown_signal(cancel) => {}
    }

    tracing::info!("shutting down");
    engine.shutdown().await;

    Ok(())
}

/// `plug daemon stop` — tell the running daemon to shut down.
async fn cmd_daemon_stop() -> anyhow::Result<()> {
    let auth_token = match daemon::read_auth_token() {
        Ok(token) => token,
        Err(_) => {
            eprintln!("no plug daemon running");
            std::process::exit(1);
        }
    };

    let request = plug_core::ipc::IpcRequest::Shutdown { auth_token };
    match daemon::ipc_request(&request).await {
        Ok(plug_core::ipc::IpcResponse::Ok) => {
            eprintln!("daemon shutting down");
        }
        Ok(plug_core::ipc::IpcResponse::Error { message, .. }) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("unexpected response from daemon");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to connect to daemon: {e}");
            std::process::exit(1);
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
        plug_core::proxy::RouterConfig::from(&config),
    );
    proxy.refresh_tools().await;

    let tools = server_manager.get_tools().await;

    match output {
        OutputFormat::Json => {
            let tool_list: Vec<serde_json::Value> = tools
                .iter()
                .map(|(server_name, tool)| {
                    let prefixed =
                        format!("{}{}{}", server_name, config.prefix_delimiter, tool.name);
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
                println!("{:<40} {:<20} {}", "NAME", "SERVER", desc_header);
                for (server_name, tool) in &tools {
                    let prefixed =
                        format!("{}{}{}", server_name, config.prefix_delimiter, tool.name);
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

/// `plug import` — scan AI client configs and import MCP server definitions.
fn cmd_import(
    config_path: Option<&std::path::PathBuf>,
    clients: Option<Vec<String>>,
    dry_run: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use plug_core::import::{self, ClientSource};

    // Determine which clients to scan
    let sources: Vec<ClientSource> = match clients {
        Some(names) => {
            let mut sources = Vec::new();
            for name in &names {
                match name.as_str() {
                    "claude-desktop" => sources.push(ClientSource::ClaudeDesktop),
                    "claude-code" => sources.push(ClientSource::ClaudeCode),
                    "cursor" => sources.push(ClientSource::Cursor),
                    "windsurf" => sources.push(ClientSource::Windsurf),
                    "vscode" => sources.push(ClientSource::VSCodeCopilot),
                    "gemini" | "gemini-cli" => sources.push(ClientSource::GeminiCli),
                    "codex" | "codex-cli" => sources.push(ClientSource::CodexCli),
                    "opencode" => sources.push(ClientSource::OpenCode),
                    "zed" => sources.push(ClientSource::Zed),
                    "cline" => sources.push(ClientSource::Cline),
                    "factory" => sources.push(ClientSource::Factory),
                    "nanobot" => sources.push(ClientSource::Nanobot),
                    _ => {
                        eprintln!("unknown client: {name}");
                        eprintln!(
                            "valid clients: claude-desktop, claude-code, cursor, windsurf, vscode, gemini-cli, codex-cli, opencode, zed, cline, factory, nanobot"
                        );
                        std::process::exit(1);
                    }
                }
            }
            sources
        }
        None => ClientSource::all().to_vec(),
    };

    // Load existing config to detect duplicates
    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };

    let report = import::import(&existing, &sources);

    match output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Text => {
            // Show scan results
            for result in &report.scanned {
                if !result.servers.is_empty() {
                    eprintln!(
                        "  {} — found {} server(s)",
                        result.source,
                        result.servers.len()
                    );
                }
                if let Some(ref err) = result.error {
                    eprintln!("  {} — error: {err}", result.source);
                }
            }

            if report.duplicates_merged > 0 {
                eprintln!("  merged {} duplicate(s)", report.duplicates_merged);
            }
            if report.skipped > 0 {
                eprintln!("  skipped {} already-configured server(s)", report.skipped);
            }

            if report.new_servers.is_empty() {
                println!("no new servers to import");
            } else if dry_run {
                println!("would import {} new server(s):", report.new_servers.len());
                for s in &report.new_servers {
                    println!("  {} (from {})", s.name, s.source);
                }
                let existing_names: Vec<String> = existing.keys().cloned().collect();
                let toml = import::servers_to_toml(&report.new_servers, &existing_names);
                println!("\nconfig to append:\n{toml}");
            } else {
                // Write to config file
                let config_file = config_path
                    .cloned()
                    .unwrap_or_else(plug_core::config::default_config_path);
                let existing_names: Vec<String> = existing.keys().cloned().collect();
                let toml = import::servers_to_toml(&report.new_servers, &existing_names);

                use std::io::Write as _;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&config_file)?;
                file.write_all(toml.as_bytes())?;

                println!(
                    "imported {} server(s) into {}",
                    report.new_servers.len(),
                    config_file.display()
                );
                for s in &report.new_servers {
                    println!("  + {} (from {})", s.name, s.source);
                }
            }
        }
    }

    Ok(())
}

/// `plug export <target>` — generate a config snippet for a target client.
fn cmd_export(target: &str, http: bool, port: u16) -> anyhow::Result<()> {
    use plug_core::export::{ExportOptions, ExportTarget, ExportTransport};

    let target: ExportTarget = target.parse().map_err(|e: String| {
        anyhow::anyhow!(
            "{e}\nvalid targets: {}",
            ExportTarget::all_names().join(", ")
        )
    })?;

    let transport = if http {
        ExportTransport::Http
    } else {
        ExportTransport::Stdio
    };

    let options = ExportOptions {
        target,
        transport,
        port,
    };

    let config = plug_core::export::export_config(&options);
    println!("{config}");

    // Show where to put it
    let path = plug_core::export::default_config_path(target, false);
    if let Some(path) = path {
        eprintln!("\nadd this to: {}", path.display());
    }

    Ok(())
}

/// `plug doctor` — run diagnostic checks on the plug setup.
async fn cmd_doctor(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let resolved_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(config_path)?;

    let report = plug_core::doctor::run_doctor(&config, &resolved_path).await;

    match output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Text => {
            let mut pass = 0;
            let mut warn = 0;
            let mut fail = 0;

            for check in &report.checks {
                let icon = match check.status {
                    plug_core::doctor::CheckStatus::Pass => {
                        pass += 1;
                        "ok"
                    }
                    plug_core::doctor::CheckStatus::Warn => {
                        warn += 1;
                        "!!"
                    }
                    plug_core::doctor::CheckStatus::Fail => {
                        fail += 1;
                        "FAIL"
                    }
                };
                println!("[{icon:>4}] {}: {}", check.name, check.message);
                if let Some(ref fix) = check.fix_suggestion {
                    println!("       fix: {fix}");
                }
            }

            println!("\n{pass} passed, {warn} warnings, {fail} failures");
            if fail > 0 {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// `plug reload` — tell the running daemon to reload its config.
async fn cmd_reload() -> anyhow::Result<()> {
    let auth_token = match daemon::read_auth_token() {
        Ok(token) => token,
        Err(_) => {
            eprintln!("no plug daemon running");
            std::process::exit(1);
        }
    };

    let request = plug_core::ipc::IpcRequest::Reload { auth_token };
    match daemon::ipc_request(&request).await {
        Ok(plug_core::ipc::IpcResponse::Ok) => {
            eprintln!("config reload triggered");
        }
        Ok(plug_core::ipc::IpcResponse::Error { message, .. }) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        Ok(_) => {
            eprintln!("unexpected response from daemon");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to connect to daemon: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}
