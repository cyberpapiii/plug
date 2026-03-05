#![deny(unsafe_code)]

/// Load `.env` file vars into the process environment.
///
/// SAFETY: Called before tokio runtime starts (single-threaded at this point).
/// `set_var` is unsafe in Rust 2024 because it's not thread-safe, but we're
/// guaranteed single-threaded here since this runs before `#[tokio::main]`.
#[allow(unsafe_code)]
fn apply_dotenv() {
    for (key, value) in plug_core::dotenv::load_dotenv() {
        // SAFETY: single-threaded, before async runtime starts
        unsafe {
            std::env::set_var(&key, &value);
        }
    }
}

mod daemon;
mod ipc_proxy;

use std::sync::Arc;

use clap::{Parser, Subcommand};

const HELP_OVERVIEW: &str = "\
Get Started:
  plug setup              Discover servers and link clients
  plug link               Link plug to your AI clients

Inspect:
  plug status             Show runtime health and next actions
  plug servers            Show configured servers
  plug tools              Show available tools
  plug doctor             Diagnose setup problems

Maintain:
  plug repair             Refresh linked client configs
  plug config --path      Print config file path

Internal:
  plug connect            stdio adapter invoked by AI clients
  plug serve --daemon     Run the background service
";

#[derive(Parser)]
#[command(
    name = "plug",
    version,
    about = "MCP multiplexer — one config, every client connected",
    after_help = HELP_OVERVIEW
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
    command: Option<Commands>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    // ─── USER COMMANDS ───────────────────────────────────────────────
    #[command(display_order = 1)]
    /// Discover servers, import config, and link your AI clients
    Setup {
        /// Accept the default action for setup prompts
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 2)]
    /// Show runtime health and the next useful action
    Status,
    #[command(display_order = 3)]
    /// Diagnose problems with your plug setup
    Doctor,
    #[command(display_order = 4)]
    /// Refresh linked AI client configuration files
    Repair,
    #[command(display_order = 5)]
    /// Internal: reload service config from disk
    Reload,

    // ─── INSPECTION ──────────────────────────────────────────────────
    #[command(display_order = 6)]
    /// Show configured servers
    Servers,
    #[command(display_order = 7)]
    /// Show available tools from your servers
    Tools,
    #[command(display_order = 8)]
    /// Link plug to your AI clients
    Link {
        /// Link these clients without prompting (e.g. claude-code cursor)
        targets: Vec<String>,
        /// Link every detected client
        #[arg(long)]
        all: bool,
        /// Accept the default action for link prompts
        #[arg(long)]
        yes: bool,
    },

    // ─── SYSTEM / CLIENT COMMANDS ────────────────────────────────────
    #[command(display_order = 9)]
    /// Internal: start the stdio adapter AI clients invoke
    Connect,
    #[command(display_order = 10)]
    /// Internal: run plug as an HTTP/background service
    Serve {
        /// Also start stdio bridge on stdin/stdout
        #[arg(long)]
        stdio: bool,
        /// Run as headless daemon with IPC socket
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 11)]
    /// Internal: stop the background plug service
    Stop,

    // ─── ADVANCED CONFIG ─────────────────────────────────────────────
    #[command(display_order = 12)]
    /// Open the plug config file in your default editor
    Config {
        /// Just print the path instead of opening it
        #[arg(long)]
        path: bool,
    },
    #[command(display_order = 13)]
    /// Advanced: import MCP servers from existing AI client configs
    Import {
        /// Only scan specific clients (comma-separated: claude-desktop,cursor,vscode,...)
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        /// Explicitly scan every supported client source
        #[arg(long)]
        all: bool,
        /// Don't modify config — just show what would be imported
        #[arg(long)]
        dry_run: bool,
        /// Import every discovered server without prompting
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 14, hide = true)]
    /// Compatibility alias for `plug link`
    Export {
        /// Link these clients without prompting (e.g. claude-code cursor)
        targets: Vec<String>,
        /// Link every detected client
        #[arg(long)]
        all: bool,
        /// Accept the default action for link prompts
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file FIRST — before config loading or anything else.
    // This ensures secrets are available for $VAR expansion regardless
    // of how plug was launched (terminal, launchd, GUI app, etc.).
    apply_dotenv();

    let cli = Cli::parse();

    // Determine log level before doing ANYTHING else
    let log_level = if cli.verbose > 0 {
        match cli.verbose {
            1 => "debug",
            _ => "trace",
        }
    } else {
        match &cli.command {
            Some(Commands::Status) | Some(Commands::Servers) | Some(Commands::Tools) => "none",
            _ => "info",
        }
    };

    let daemon_mode = matches!(&cli.command, Some(Commands::Serve { daemon: true, .. }));

    let _log_guard = if daemon_mode {
        Some(daemon::setup_file_logging(&daemon::log_dir())?)
    } else {
        init_stderr_tracing(log_level);
        None
    };

    match cli.command {
        None => {
            cmd_overview(cli.config.as_ref(), &cli.output).await?;
        }
        Some(Commands::Connect) => {
            cmd_connect(cli.config.as_ref()).await?;
        }
        Some(Commands::Serve { stdio, daemon }) => {
            if daemon {
                cmd_daemon(cli.config.as_ref()).await?;
            } else {
                cmd_serve(cli.config.as_ref(), stdio).await?;
            }
        }
        Some(Commands::Status) => {
            cmd_status(cli.config.as_ref(), &cli.output).await?;
        }
        Some(Commands::Stop) => {
            cmd_daemon_stop().await?;
        }
        Some(Commands::Servers) => {
            cmd_server_list(cli.config.as_ref(), &cli.output).await?;
        }
        Some(Commands::Tools) => {
            cmd_tool_list(cli.config.as_ref(), &cli.output, cli.verbose).await?;
        }
        Some(Commands::Link { targets, all, yes }) => {
            cmd_link(targets, all, yes)?;
        }
        Some(Commands::Import {
            clients,
            all,
            dry_run,
            yes,
        }) => {
            cmd_import(cli.config.as_ref(), clients, all, dry_run, yes, &cli.output)?;
        }
        Some(Commands::Doctor) => {
            cmd_doctor(cli.config.as_ref(), &cli.output).await?;
        }
        Some(Commands::Repair) => {
            cmd_repair()?;
        }
        Some(Commands::Setup { yes }) => {
            cmd_setup(cli.config.as_ref(), yes)?;
        }
        Some(Commands::Reload) => {
            cmd_reload().await?;
        }
        Some(Commands::Config { path }) => {
            cmd_config(cli.config.as_ref(), path)?;
        }
        Some(Commands::Export { targets, all, yes }) => {
            cmd_link(targets, all, yes)?;
        }
    }

    Ok(())
}

fn init_stderr_tracing(level: &str) {
    if level == "none" {
        return;
    }

    let filter = tracing_subscriber::EnvFilter::try_from_env("PLUG_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .compact()
        .init();
}

async fn cmd_connect(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    match connect_via_daemon(config_path).await {
        Ok(()) => return Ok(()),
        Err(e) => {
            tracing::error!(error = %e, "daemon proxy failed — falling back to standalone mode");
        }
    }
    connect_standalone(config_path).await
}

async fn cmd_overview(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config_exists = config_path.exists();
    let linked_clients = linked_client_targets();

    if matches!(output, OutputFormat::Json) {
        let daemon_running = daemon::connect_to_daemon().await.is_some();
        let config = if config_exists {
            plug_core::config::load_config(Some(&config_path)).ok()
        } else {
            None
        };
        let server_count = config.as_ref().map(|c| c.servers.len()).unwrap_or(0);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "overview",
                "config_exists": config_exists,
                "config_path": config_path,
                "daemon_running": daemon_running,
                "server_count": server_count,
                "linked_clients": linked_clients,
                "next_actions": if !config_exists {
                    vec!["plug setup"]
                } else if linked_clients.is_empty() {
                    vec!["plug link", "plug status"]
                } else if daemon_running {
                    vec!["plug status", "plug doctor"]
                } else {
                    vec!["plug status", "plug doctor", "plug repair"]
                }
            }))?
        );
        return Ok(());
    }

    use dialoguer::console::{Emoji, style};

    println!("{} {}", Emoji("🔌", ""), style("plug").bold().cyan());
    println!();

    if !config_exists {
        println!(
            "No config found at {}.",
            style(config_path.display()).yellow()
        );
        println!();
        println!("Next:");
        println!("  1. {}", style("plug setup").cyan());
        println!("  2. {}", style("plug status").cyan());
        return Ok(());
    }

    let config = plug_core::config::load_config(Some(&config_path))?;
    let daemon_running = daemon::connect_to_daemon().await.is_some();

    println!(
        "Config: {}  Servers: {}  Linked clients: {}  Service: {}",
        style(config_path.display()).dim(),
        style(config.servers.len()).bold(),
        style(linked_clients.len()).bold(),
        if daemon_running {
            style("running").green().bold()
        } else {
            style("stopped").yellow().bold()
        }
    );

    if !linked_clients.is_empty() {
        println!("Linked: {}", linked_clients.join(", "));
    }

    println!();
    println!("Next:");
    if linked_clients.is_empty() {
        println!("  1. {}", style("plug link").cyan());
        println!("  2. {}", style("plug status").cyan());
    } else if daemon_running {
        println!("  1. {}", style("plug status").cyan());
        println!("  2. {}", style("plug doctor").cyan());
    } else {
        println!("  1. {}", style("plug status").cyan());
        println!("  2. {}", style("plug repair").cyan());
    }

    Ok(())
}

async fn connect_via_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            auto_start_daemon(config_path)?;
            wait_for_daemon_ready().await?
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let register_req = plug_core::ipc::IpcRequest::Register { client_info: None };
    let payload = serde_json::to_vec(&register_req)?;
    plug_core::ipc::write_frame(&mut writer, &payload).await?;

    let frame = plug_core::ipc::read_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed"))?;

    let response: plug_core::ipc::IpcResponse = serde_json::from_slice(&frame)?;
    let session_id = match response {
        plug_core::ipc::IpcResponse::Registered { session_id } => session_id,
        _ => anyhow::bail!("registration failed"),
    };

    let proxy = ipc_proxy::IpcProxyHandler::new(reader, writer, session_id);
    use rmcp::ServiceExt as _;
    let transport = rmcp::transport::io::stdio();
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    Ok(())
}

async fn connect_standalone(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());
    use rmcp::ServiceExt as _;
    let transport = rmcp::transport::io::stdio();
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    engine.shutdown().await;
    Ok(())
}

fn auto_start_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }

    // We don't env_clear here because we want to inherit the parent's PATH
    // and other essentials. The .env file will override these values
    // inside the daemon's own startup sequence anyway.

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn()?;
    Ok(())
}

async fn wait_for_daemon_ready() -> anyhow::Result<tokio::net::UnixStream> {
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await {
            return Ok(stream);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(500));
    }
    anyhow::bail!("daemon failed to start")
}

async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::console::{Emoji, style};

    // Ensure daemon is running so we get a live status
    let _ = ensure_daemon(config_path).await;

    if let Ok(plug_core::ipc::IpcResponse::Status {
        servers,
        clients,
        uptime_secs,
    }) = daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
    {
        if matches!(output, OutputFormat::Text) {
            println!(
                "{} {} (uptime: {}s) | {} {} client(s) connected",
                Emoji("🔌", ""),
                style("Plug Engine is running").green().bold(),
                uptime_secs,
                Emoji("👥", ""),
                style(clients.to_string()).bold()
            );
            println!();
            if servers.is_empty() {
                println!("  No servers configured.");
            } else {
                println!(
                    "  {:<20} {:<15} {:<6}",
                    style("SERVER").dim(),
                    style("STATUS").dim(),
                    style("TOOLS").dim()
                );
                for s in &servers {
                    if s.server_id == "__plug_internal__" {
                        continue;
                    }
                    let (icon, health) = match s.health {
                        plug_core::types::ServerHealth::Healthy => {
                            (Emoji("🟢", ""), style("Healthy").green())
                        }
                        plug_core::types::ServerHealth::Degraded => {
                            (Emoji("🟡", ""), style("Degraded").yellow())
                        }
                        plug_core::types::ServerHealth::Failed => {
                            (Emoji("🔴", ""), style("Failed").red())
                        }
                    };
                    println!(
                        "  {} {:<18} {:<23} {:<6}",
                        icon, s.server_id, health, s.tool_count
                    );
                }
            }
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({ "uptime": uptime_secs, "clients": clients, "servers": servers })
                )?
            );
        }
        return Ok(());
    }

    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        println!(
            "{} {}",
            Emoji("💤", ""),
            style("Plug Engine failed to start.").red().bold()
        );
        let mut names: Vec<_> = config.servers.keys().collect();
        names.sort();
        for n in names {
            println!(
                "  {} {:<18} {}",
                Emoji("⚪", ""),
                n,
                style("Not Running").dim()
            );
        }
    }
    Ok(())
}

async fn cmd_server_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::console::{Emoji, style};

    // Try to get live status from daemon first
    let _ = ensure_daemon(config_path).await;

    if let Ok(plug_core::ipc::IpcResponse::Status { servers, .. }) =
        daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
    {
        match output {
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&servers)?),
            OutputFormat::Text => {
                if servers.is_empty() {
                    println!("No servers configured.");
                } else {
                    println!(
                        "{} {} server(s) active:\n",
                        Emoji("📡", ""),
                        style(servers.len().saturating_sub(1)).bold()
                    );
                    for s in servers {
                        if s.server_id == "__plug_internal__" {
                            continue;
                        }
                        let (icon, health) = match s.health {
                            plug_core::types::ServerHealth::Healthy => {
                                (Emoji("🟢", ""), style("Healthy").green())
                            }
                            plug_core::types::ServerHealth::Degraded => {
                                (Emoji("🟡", ""), style("Degraded").yellow())
                            }
                            plug_core::types::ServerHealth::Failed => {
                                (Emoji("🔴", ""), style("Failed").red())
                            }
                        };
                        println!(
                            "  {} {:<18} {} ({} tools)",
                            icon,
                            style(&s.server_id).bold(),
                            health,
                            s.tool_count
                        );
                    }
                }
            }
        }
        return Ok(());
    }
    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        let mut names: Vec<_> = config.servers.keys().collect();
        names.sort();
        println!(
            "{} {} server(s) configured (daemon not running):\n",
            Emoji("⚙️", ""),
            style(names.len()).bold()
        );
        for n in names {
            println!("  {} {}", Emoji("⚪", ""), style(n).dim());
        }
    }
    Ok(())
}

/// Helper to ensure the background daemon is running.
async fn ensure_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    if daemon::connect_to_daemon().await.is_none() {
        auto_start_daemon(config_path)?;
        wait_for_daemon_ready().await?;
    }
    Ok(())
}

async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let cancel = engine.cancel_token().clone();
    plug_core::watcher::spawn_config_watcher(engine.clone(), cancel.clone(), engine.tracker());
    tokio::select! {
        _ = daemon::run_daemon(engine.clone(), 30) => {}
        _ = daemon::shutdown_signal(cancel) => {}
    }
    engine.shutdown().await;
    Ok(())
}

async fn cmd_daemon_stop() -> anyhow::Result<()> {
    let auth_token = daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Shutdown { auth_token };
    if let Ok(plug_core::ipc::IpcResponse::Ok) = daemon::ipc_request(&req).await {
        println!("stopped");
    }
    Ok(())
}

async fn cmd_serve(config_path: Option<&std::path::PathBuf>, _stdio: bool) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config.clone()));
    engine.start().await?;
    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: engine.tool_router().clone(),
        sessions: plug_core::http::session::SessionManager::new(3600, 100),
        cancel: engine.cancel_token().clone(),
        sse_channel_capacity: 100,
    });
    let router = plug_core::http::server::build_router(http_state);
    let addr = format!("{}:{}", config.http.bind_address, config.http.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("serving on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    _verbose: u8,
) -> anyhow::Result<()> {
    use dialoguer::console::{Emoji, style};
    use std::collections::BTreeMap;

    // Ensure daemon is running so we get a live tool list
    let _ = ensure_daemon(config_path).await;

    // Collect all tools from daemon
    let mut all_tools: Vec<plug_core::ipc::IpcToolInfo> = Vec::new();
    if let Ok(plug_core::ipc::IpcResponse::Tools { tools }) =
        daemon::ipc_request(&plug_core::ipc::IpcRequest::ListTools).await
    {
        for t in tools {
            if t.server_id == "__plug_internal__" {
                continue;
            }
            all_tools.push(t);
        }
    }

    // Group by prefix (the part before "__" in the wire name)
    #[allow(clippy::type_complexity)]
    let mut tools_by_prefix: BTreeMap<
        String,
        Vec<(String, String, Option<String>, Option<String>)>,
    > = BTreeMap::new();
    for t in &all_tools {
        let (prefix, tool_name) = if let Some(idx) = t.name.find("__") {
            (t.name[..idx].to_string(), t.name[idx + 2..].to_string())
        } else {
            (t.server_id.clone(), t.name.clone())
        };
        tools_by_prefix.entry(prefix).or_default().push((
            tool_name,
            t.server_id.clone(),
            t.title.clone(),
            t.description.clone(),
        ));
    }

    // Count unique server IDs
    let unique_servers: std::collections::BTreeSet<&str> =
        all_tools.iter().map(|t| t.server_id.as_str()).collect();

    match output {
        OutputFormat::Json => {
            // Include title in JSON output
            let json_groups: BTreeMap<String, Vec<serde_json::Value>> = tools_by_prefix
                .iter()
                .map(|(prefix, tools)| {
                    let entries: Vec<serde_json::Value> = tools
                        .iter()
                        .map(|(name, server_id, title, desc)| {
                            serde_json::json!({
                                "name": name,
                                "server_id": server_id,
                                "title": title,
                                "description": desc,
                            })
                        })
                        .collect();
                    (prefix.clone(), entries)
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_groups)?);
        }
        OutputFormat::Text => {
            if tools_by_prefix.is_empty() {
                println!(
                    "No tools found. Run {} to add servers.",
                    style("plug setup").cyan()
                );
                return Ok(());
            }
            let term_width = console::Term::stdout().size().1 as usize;
            let available_width = term_width.saturating_sub(40);
            println!(
                "{} {} tools across {} server(s)\n",
                Emoji("⚒️", ""),
                style(all_tools.len()).bold().green(),
                unique_servers.len()
            );
            for (prefix, mut tools) in tools_by_prefix {
                tools.sort_by(|a, b| a.0.cmp(&b.0));
                // Check if this group's server_id differs from the prefix (sub-service)
                let server_id = &tools[0].1;
                let annotation = if server_id != &prefix {
                    format!("  {}", style(format!("[{}]", server_id)).dim())
                } else {
                    String::new()
                };
                println!(
                    " {} ({}){}",
                    style(&prefix).bold().underlined(),
                    style(format!("{} tools", tools.len())).dim(),
                    annotation
                );
                for (name, _server_id, title, desc) in &tools {
                    let name_styled = style(format!("   {:<30}", name)).cyan();
                    // Prefer title, fall back to description
                    let display_text = title.as_deref().or(desc.as_deref());
                    if let Some(text) = display_text {
                        let cleaned = text.replace('\n', " ").replace('\r', "");
                        let short = if cleaned.len() > available_width {
                            format!("{}...", &cleaned[..available_width.max(0)])
                        } else {
                            cleaned
                        };
                        println!("{}  {}", name_styled, style(short).dim());
                    } else {
                        println!("{}", name_styled);
                    }
                }
                println!();
            }
        }
    }
    Ok(())
}

fn cmd_import(
    config_path: Option<&std::path::PathBuf>,
    clients: Option<Vec<String>>,
    _all: bool,
    dry_run: bool,
    yes: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::console::{Emoji, style};
    use dialoguer::{MultiSelect, theme::ColorfulTheme};
    use plug_core::import::{self, ClientSource};

    let sources = match clients {
        Some(names) => names
            .iter()
            .filter_map(|n| match n.as_str() {
                "claude-desktop" => Some(ClientSource::ClaudeDesktop),
                "claude-code" => Some(ClientSource::ClaudeCode),
                "cursor" => Some(ClientSource::Cursor),
                "windsurf" => Some(ClientSource::Windsurf),
                "vscode" => Some(ClientSource::VSCodeCopilot),
                "gemini-cli" => Some(ClientSource::GeminiCli),
                "codex-cli" => Some(ClientSource::CodexCli),
                "opencode" => Some(ClientSource::OpenCode),
                "zed" => Some(ClientSource::Zed),
                "cline" => Some(ClientSource::Cline),
                "cline-cli" => Some(ClientSource::ClineCli),
                "roocode" => Some(ClientSource::RooCode),
                "factory" => Some(ClientSource::Factory),
                "nanobot" => Some(ClientSource::Nanobot),
                "junie" => Some(ClientSource::Junie),
                "kilo" => Some(ClientSource::Kilo),
                "antigravity" => Some(ClientSource::Antigravity),
                "goose" => Some(ClientSource::Goose),
                _ => None,
            })
            .collect(),
        None => ClientSource::all().to_vec(),
    };

    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };

    if matches!(output, OutputFormat::Text) {
        println!(
            "{} {}",
            Emoji("🔍", ""),
            style("Scanning for MCP servers...").bold()
        );
    }
    let report = import::import(&existing, &sources);

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for res in &report.scanned {
                if let Some(ref e) = res.error {
                    eprintln!("  {} {} — {}", Emoji("⚠️", ""), res.source, style(e).red());
                }
            }
            if report.new_servers.is_empty() {
                println!("\n{} No new servers found.", Emoji("✅", ""));
                return Ok(());
            }
            if dry_run {
                return Ok(());
            }

            let selections = if yes {
                (0..report.new_servers.len()).collect::<Vec<_>>()
            } else {
                let labels: Vec<_> = report
                    .new_servers
                    .iter()
                    .map(|s| {
                        format!(
                            "{:<15} {}",
                            style(&s.name).bold(),
                            style(format!("(from {})", s.source)).dim()
                        )
                    })
                    .collect();
                MultiSelect::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select servers to import")
                    .items(&labels)
                    .defaults(&vec![true; labels.len()])
                    .interact()?
            };
            if selections.is_empty() {
                return Ok(());
            }

            let config_file = config_path
                .cloned()
                .unwrap_or_else(plug_core::config::default_config_path);
            let to_import: Vec<plug_core::import::DiscoveredServer> = selections
                .iter()
                .map(|&i| report.new_servers[i].clone())
                .collect();
            let existing_names: Vec<String> = existing.keys().cloned().collect();
            let toml = import::servers_to_toml(&to_import, &existing_names);

            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&config_file)?;
            file.write_all(toml.as_bytes())?;
            println!(
                "\n{} Imported {} server(s).",
                Emoji("✨", ""),
                to_import.len()
            );
        }
    }
    Ok(())
}

fn all_client_targets() -> &'static [(&'static str, &'static str)] {
    &[
        ("Claude Desktop", "claude-desktop"),
        ("Claude Code", "claude-code"),
        ("Cursor", "cursor"),
        ("VS Code Copilot", "vscode"),
        ("Windsurf", "windsurf"),
        ("Gemini CLI", "gemini-cli"),
        ("Codex CLI", "codex-cli"),
        ("OpenCode", "opencode"),
        ("Zed", "zed"),
        ("Cline (VS Code)", "cline"),
        ("Cline CLI", "cline-cli"),
        ("RooCode", "roocode"),
        ("Factory", "factory"),
        ("Nanobot", "nanobot"),
        ("JetBrains Junie", "junie"),
        ("Kilo Code", "kilo"),
        ("Google Antigravity", "antigravity"),
        ("Goose", "goose"),
    ]
}

fn linked_client_targets() -> Vec<String> {
    all_client_targets()
        .iter()
        .filter_map(|(_, target)| is_linked(target, false).then(|| (*target).to_string()))
        .collect()
}

fn detected_or_linked_clients() -> Vec<(&'static str, &'static str, bool)> {
    let mut items = Vec::new();
    for (display, target) in all_client_targets() {
        let linked = is_linked(target, false);
        let installed = if let Ok(t) = target.parse::<plug_core::export::ExportTarget>() {
            if let Some(path) = plug_core::export::default_config_path(t, false) {
                if path.exists() {
                    true
                } else if let Some(p) = path.parent() {
                    p.exists()
                        && !p.to_string_lossy().ends_with(".config")
                        && p != dirs::home_dir().unwrap_or_default()
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if linked || installed {
            items.push((*display, *target, linked));
        }
    }
    items
}

fn cmd_link(targets: Vec<String>, all: bool, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, MultiSelect, Select, theme::ColorfulTheme};

    if !targets.is_empty() {
        for target in &targets {
            execute_export(target, false, 3282, true, false)?;
        }
        return Ok(());
    }

    println!("✨ Let's link Plug to your AI clients ✨\n");

    if all {
        let detected = detected_or_linked_clients();
        if detected.is_empty() {
            anyhow::bail!(
                "no detected clients found; pass explicit targets or run `plug link` interactively"
            );
        }
        for target in detected.iter().map(|(_, target, _)| *target) {
            execute_export(target, false, 3282, true, false)?;
        }
        return Ok(());
    }

    let mut items = detected_or_linked_clients()
        .into_iter()
        .map(|(display, target, linked)| {
            let label = if linked {
                format!("{display} (Linked)")
            } else {
                format!("{display} (Detected)")
            };
            (label, target, display, linked)
        })
        .collect::<Vec<_>>();

    if items.is_empty() {
        println!("No clients detected.");
        if yes {
            println!(
                "Pass explicit targets like `plug link claude-code cursor` or run `plug link` interactively."
            );
            return Ok(());
        }
        if Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Show all supported clients?")
            .default(true)
            .interact()?
        {
            for (display, target) in all_client_targets() {
                items.push((
                    display.to_string(),
                    *target,
                    *display,
                    is_linked(target, false),
                ));
            }
        } else {
            return Ok(());
        }
    } else if !yes
        && Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Show all supported clients?")
            .default(false)
            .interact()?
    {
        items.clear();
        for (display, target) in all_client_targets() {
            let linked = is_linked(target, false);
            let label = if linked {
                format!("{display} (Linked)")
            } else {
                display.to_string()
            };
            items.push((label, *target, *display, linked));
        }
    }

    let selections = if yes {
        (0..items.len()).collect::<Vec<_>>()
    } else {
        let labels: Vec<_> = items.iter().map(|(l, ..)| l.clone()).collect();
        let defaults: Vec<_> = items.iter().map(|(.., linked)| *linked).collect();
        MultiSelect::with_theme(&ColorfulTheme::default())
            .with_prompt("Space to toggle [Linked], Enter to apply")
            .items(&labels)
            .defaults(&defaults)
            .interact()?
    };

    for (idx, (_, target, _display, was_linked)) in items.iter().enumerate() {
        let is_selected = selections.contains(&idx);
        if is_selected && !was_linked {
            execute_export(target, false, 3282, true, false)?;
        } else if !is_selected && *was_linked {
            execute_unlink(target, false)?;
        }
    }

    if yes {
        return Ok(());
    }

    println!();
    if Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Configure custom client?")
        .default(false)
        .interact()?
    {
        let path_str: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Config path")
            .interact_text()?;
        let path = if let Some(stripped) = path_str.strip_prefix("~/") {
            dirs::home_dir().unwrap().join(stripped)
        } else {
            std::path::PathBuf::from(path_str)
        };
        let format = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Format")
            .items(["JSON", "JSON (VS Code style)", "TOML", "YAML"])
            .default(0)
            .interact()?;
        let (snippet, is_toml, is_yaml) = match format {
            0 => (serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"command":"plug","args":["connect"]}}})).unwrap(), false, false),
            1 => (serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"command":"plug","args":["connect"]}}}})).unwrap(), false, false),
            2 => ("\n[mcp_servers.plug]\ncommand = \"plug\"\nargs = [\"connect\"]\n".to_string(), true, false),
            3 => ("\nextensions:\n  plug:\n    type: stdio\n    command: plug\n    args: [\"connect\"]\n    enabled: true\n".to_string(), false, true),
            _ => unreachable!(),
        };
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let existing = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let updated = if is_toml {
            let mut un = plug_core::import::unlink_toml(&existing);
            if !un.ends_with('\n') {
                un.push('\n');
            }
            un.push_str(&snippet);
            un
        } else if is_yaml {
            let mut un = unlink_yaml(&existing);
            if !un.ends_with('\n') {
                un.push('\n');
            }
            un.push_str(&snippet);
            un
        } else {
            merge_json_config(&existing, &snippet)?
        };
        std::fs::write(&path, updated)?;
    }
    Ok(())
}

fn execute_unlink(target: &str, project: bool) -> anyhow::Result<()> {
    let target_enum: plug_core::export::ExportTarget =
        target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let path = plug_core::export::default_config_path(target_enum, project)
        .ok_or_else(|| anyhow::anyhow!("no path"))?;
    if !path.exists() {
        return Ok(());
    }
    let existing = std::fs::read_to_string(&path)?;
    let ext = path.extension().and_then(|e| e.to_str());
    let unlinked = match ext {
        Some("toml") => plug_core::import::unlink_toml(&existing),
        Some("yaml") | Some("yml") => unlink_yaml(&existing),
        _ => unmerge_json_config(&existing)?,
    };
    std::fs::write(&path, unlinked)?;
    Ok(())
}

fn is_linked(target: &str, project: bool) -> bool {
    let target_enum: plug_core::export::ExportTarget = match target.parse() {
        Ok(t) => t,
        Err(_) => return false,
    };
    let path = match plug_core::export::default_config_path(target_enum, project) {
        Some(p) => p,
        None => return false,
    };
    if !path.exists() {
        return false;
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let ext = path.extension().and_then(|e| e.to_str());
    match ext {
        Some("toml") => content.contains("[mcp_servers.plug]"),
        Some("yaml") | Some("yml") => content.contains("plug:"),
        _ => {
            // For JSON, be more precise to avoid false positives in large configs
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                match target_enum {
                    plug_core::export::ExportTarget::Nanobot => json
                        .get("tools")
                        .and_then(|t| t.get("mcpServers"))
                        .and_then(|s| s.get("plug"))
                        .is_some(),
                    plug_core::export::ExportTarget::VSCodeCopilot => json
                        .get("mcp")
                        .and_then(|m| m.get("servers"))
                        .and_then(|s| s.get("plug"))
                        .is_some(),
                    _ => {
                        json.get("mcpServers").and_then(|s| s.get("plug")).is_some()
                            || json
                                .get("context_servers")
                                .and_then(|s| s.get("plug"))
                                .is_some()
                    }
                }
            } else {
                content.contains("\"plug\":")
            }
        }
    }
}

fn unmerge_json_config(existing: &str) -> anyhow::Result<String> {
    let mut json: serde_json::Value =
        serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        // Location 1: Top-level standard keys
        for key in ["mcpServers", "context_servers"] {
            if let Some(inner) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
                inner.remove("plug");
            }
        }
        // Location 2: VS Code style (mcp.servers)
        if let Some(mcp) = obj.get_mut("mcp").and_then(|v| v.as_object_mut()) {
            if let Some(srv) = mcp.get_mut("servers").and_then(|v| v.as_object_mut()) {
                srv.remove("plug");
            }
        }
        // Location 3: Nanobot / OpenCode style (tools.mcpServers)
        if let Some(tools) = obj.get_mut("tools").and_then(|v| v.as_object_mut()) {
            if let Some(srv) = tools.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
                srv.remove("plug");
            }
        }
    }
    Ok(serde_json::to_string_pretty(&json)?)
}

fn merge_json_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_json: serde_json::Value =
        serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    let snippet_json: serde_json::Value = serde_json::from_str(snippet)?;
    if let (Some(e_obj), Some(s_obj)) = (existing_json.as_object_mut(), snippet_json.as_object()) {
        for (k, v) in s_obj {
            // Specialized deep merge for nested keys: mcpServers, mcp.servers, tools.mcpServers
            if k == "mcp" || k == "tools" {
                if let (Some(e_inner), Some(s_inner)) = (
                    e_obj.get_mut(k).and_then(|v| v.as_object_mut()),
                    v.as_object(),
                ) {
                    for (ik, iv) in s_inner {
                        if let (Some(e_deep), Some(s_deep)) = (
                            e_inner.get_mut(ik).and_then(|v| v.as_object_mut()),
                            iv.as_object(),
                        ) {
                            for (dk, dv) in s_deep {
                                e_deep.insert(dk.clone(), dv.clone());
                            }
                        } else {
                            e_inner.insert(ik.clone(), iv.clone());
                        }
                    }
                } else {
                    e_obj.insert(k.clone(), v.clone());
                }
            } else if let (Some(e_inner), Some(s_inner)) = (
                e_obj.get_mut(k).and_then(|v| v.as_object_mut()),
                v.as_object(),
            ) {
                for (ik, iv) in s_inner {
                    e_inner.insert(ik.clone(), iv.clone());
                }
            } else {
                e_obj.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(serde_json::to_string_pretty(&existing_json)?)
}

fn merge_yaml_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_yml: serde_yml::Value = serde_yml::from_str(existing)
        .unwrap_or_else(|_| serde_yml::Value::Mapping(serde_yml::Mapping::new()));
    let snippet_yml: serde_yml::Value = serde_yml::from_str(snippet)?;

    if let (Some(e_map), Some(s_map)) = (existing_yml.as_mapping_mut(), snippet_yml.as_mapping()) {
        for (k, v) in s_map {
            if let (Some(e_inner), Some(s_inner)) = (
                e_map.get_mut(k).and_then(|v| v.as_mapping_mut()),
                v.as_mapping(),
            ) {
                for (ik, iv) in s_inner {
                    e_inner.insert(ik.clone(), iv.clone());
                }
            } else {
                e_map.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(serde_yml::to_string(&existing_yml)?)
}

fn unlink_yaml(existing: &str) -> String {
    let mut output = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == "plug:"
            || (skipping
                && (trimmed.starts_with("type:")
                    || trimmed.starts_with("command:")
                    || trimmed.starts_with("args:")
                    || trimmed.starts_with("enabled:")
                    || trimmed.starts_with("- ")))
        {
            skipping = true;
            continue;
        }
        if skipping && !line.starts_with(' ') && !trimmed.is_empty() {
            skipping = false;
        }
        if !skipping {
            output.push(line);
        }
    }
    output.join("\n")
}

fn execute_export(
    target: &str,
    http: bool,
    port: u16,
    write: bool,
    project: bool,
) -> anyhow::Result<()> {
    use plug_core::export::{ExportOptions, ExportTarget, ExportTransport};
    let target_enum: ExportTarget = target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let transport = if http {
        ExportTransport::Http
    } else {
        ExportTransport::Stdio
    };

    // Get absolute path to current binary for robust stdio execution
    let command = std::env::current_exe()?.to_string_lossy().to_string();

    let options = ExportOptions {
        target: target_enum,
        transport,
        port,
        command,
    };
    let snippet = plug_core::export::export_config(&options);
    if write {
        let path = plug_core::export::default_config_path(target_enum, project)
            .ok_or_else(|| anyhow::anyhow!("no path"))?;
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let existing = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let ext = path.extension().and_then(|e| e.to_str());
        let updated = match ext {
            Some("toml") => {
                let mut un = plug_core::import::unlink_toml(&existing);
                if !un.ends_with('\n') {
                    un.push('\n');
                }
                un.push_str(&snippet);
                un
            }
            Some("yaml") | Some("yml") => merge_yaml_config(&existing, &snippet)?,
            _ => merge_json_config(&existing, &snippet)?,
        };
        std::fs::write(&path, updated)?;
    } else {
        println!("{snippet}");
    }
    Ok(())
}

async fn cmd_doctor(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let resolved = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(config_path)?;
    let report = plug_core::doctor::run_doctor(&config, &resolved).await;
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for c in &report.checks {
                println!(
                    "[{:>4}] {}: {}",
                    format!("{:?}", c.status),
                    c.name,
                    c.message
                );
            }
        }
    }
    Ok(())
}

async fn cmd_reload() -> anyhow::Result<()> {
    let auth = daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Reload { auth_token: auth };
    daemon::ipc_request(&req).await?;
    Ok(())
}

fn cmd_setup(config_path: Option<&std::path::PathBuf>, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, theme::ColorfulTheme};
    println!("✨ Welcome to Plug Setup ✨\n");
    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };
    let report = plug_core::import::import(&existing, plug_core::import::ClientSource::all());
    if !report.new_servers.is_empty() {
        println!("Found {} servers:", report.new_servers.len());
        if yes
            || Confirm::with_theme(&ColorfulTheme::default())
                .with_prompt("Import them?")
                .default(true)
                .interact()?
        {
            let path = config_path
                .cloned()
                .unwrap_or_else(plug_core::config::default_config_path);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let existing_names: Vec<String> = existing.keys().cloned().collect();
            let toml = plug_core::import::servers_to_toml(&report.new_servers, &existing_names);
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(toml.as_bytes())?;
        }
    }
    cmd_link(Vec::new(), false, yes)?;
    Ok(())
}

fn cmd_config(config_path: Option<&std::path::PathBuf>, path_only: bool) -> anyhow::Result<()> {
    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    if path_only {
        println!("{}", path.display());
    } else if path.exists() {
        open::that(&path)?;
    } else {
        println!("Config missing at {}. Run setup.", path.display());
    }
    Ok(())
}

/// `plug repair` — clean up and refresh all client configurations.
fn cmd_repair() -> anyhow::Result<()> {
    use dialoguer::console::{Emoji, style};

    println!(
        "{} {}",
        Emoji("🛠️", ""),
        style("Repairing AI client configurations...").bold()
    );

    let all_clients = [
        "claude-desktop",
        "claude-code",
        "cursor",
        "vscode",
        "windsurf",
        "gemini-cli",
        "codex-cli",
        "opencode",
        "zed",
        "cline",
        "cline-cli",
        "roocode",
        "factory",
        "nanobot",
        "junie",
        "kilo",
        "antigravity",
        "goose",
    ];

    let mut repaired_count = 0;

    for target in all_clients {
        // Only repair if it's currently linked
        if is_linked(target, false) {
            print!("  {} Refreshing {}... ", Emoji("🔄", ""), target);
            if let Err(e) = execute_export(target, false, 3282, true, false) {
                println!("{}", style(format!("failed: {e}")).red());
            } else {
                println!("{}", style("done").green());
                repaired_count += 1;
            }
        }
    }

    if repaired_count == 0 {
        println!("\n{} No linked clients found to repair.", Emoji("✅", ""));
    } else {
        println!(
            "\n{} Successfully repaired {} client configuration(s).",
            Emoji("✨", ""),
            repaired_count
        );
    }

    Ok(())
}
