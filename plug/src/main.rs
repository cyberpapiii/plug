#![forbid(unsafe_code)]

mod daemon;
mod ipc_proxy;

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
    // ─── USER COMMANDS ───────────────────────────────────────────────
    #[command(display_order = 1)]
    /// Interactive setup wizard for onboarding
    Setup,
    #[command(display_order = 2)]
    /// Show server health status (queries daemon if running)
    Status,
    #[command(display_order = 3)]
    /// Run diagnostic checks on your plug setup
    Doctor,
    #[command(display_order = 4)]
    /// Reload config from disk (sends reload signal to daemon)
    Reload,

    // ─── INSPECTION ──────────────────────────────────────────────────
    #[command(display_order = 5)]
    /// List configured servers
    Servers,
    #[command(display_order = 6)]
    /// List all available tools from your servers
    Tools,

    // ─── SYSTEM / CLIENT COMMANDS ────────────────────────────────────
    #[command(display_order = 7)]
    /// Start the MCP stdio bridge (what clients invoke)
    Connect,
    #[command(display_order = 8)]
    /// Start the HTTP server for web-based MCP clients
    Serve {
        /// Also start stdio bridge on stdin/stdout
        #[arg(long)]
        stdio: bool,
        /// Run as headless daemon with IPC socket
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 9)]
    /// Stop the background plug engine (daemon)
    Stop,

    // ─── ADVANCED CONFIG ─────────────────────────────────────────────
    #[command(display_order = 10)]
    /// Open the plug configuration file in your default editor
    Config {
        /// Just print the path instead of opening it
        #[arg(long)]
        path: bool,
    },
    #[command(display_order = 11)]
    /// Import MCP servers from AI client configs
    Import {
        /// Only scan specific clients (comma-separated: claude-desktop,cursor,vscode,...)
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        /// Don't modify config — just show what would be imported
        #[arg(long)]
        dry_run: bool,
    },
    #[command(display_order = 12)]
    /// Interactively link plug to your AI clients
    Export,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let log_to_file = matches!(
        &cli.command,
        Commands::Serve { daemon: true, .. }
    );
    
    let log_level = if cli.verbose > 0 {
        match cli.verbose {
            1 => "debug",
            _ => "trace",
        }
    } else {
        match &cli.command {
            Commands::Status | Commands::Servers | Commands::Tools => "error",
            _ => "info",
        }
    };

    let _log_guard = if log_to_file {
        Some(daemon::setup_file_logging(&daemon::log_dir())?)
    } else {
        init_stderr_tracing(log_level);
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
        Commands::Status => {
            cmd_status(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Stop => {
            cmd_daemon_stop().await?;
        }
        Commands::Servers => {
            cmd_server_list(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Tools => {
            cmd_tool_list(cli.config.as_ref(), &cli.output, cli.verbose).await?;
        }
        Commands::Import { clients, dry_run } => {
            cmd_import(cli.config.as_ref(), clients, dry_run, &cli.output)?;
        }
        Commands::Export => {
            cmd_export()?;
        }
        Commands::Doctor => {
            cmd_doctor(cli.config.as_ref(), &cli.output).await?;
        }
        Commands::Setup => {
            cmd_setup(cli.config.as_ref())?;
        }
        Commands::Reload => {
            cmd_reload().await?;
        }
        Commands::Config { path } => {
            cmd_config(cli.config.as_ref(), path)?;
        }
    }

    Ok(())
}

fn init_stderr_tracing(level: &str) {
    let filter = if level == "error" {
        // For clean commands, only show errors from our own code and be silent for dependencies
        tracing_subscriber::EnvFilter::new("plug=error,plug_core=error")
    } else {
        tracing_subscriber::EnvFilter::try_from_env("PLUG_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level))
    };

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
    let service = proxy.serve(transport).await.map_err(|e| anyhow::anyhow!(e))?;
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
    let service = proxy.serve(transport).await.map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    engine.shutdown().await;
    Ok(())
}

fn auto_start_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path { cmd.arg("--config").arg(path); }
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn()?;
    Ok(())
}

async fn wait_for_daemon_ready() -> anyhow::Result<tokio::net::UnixStream> {
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await { return Ok(stream); }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(500));
    }
    anyhow::bail!("daemon failed to start")
}

async fn cmd_status(config_path: Option<&std::path::PathBuf>, output: &OutputFormat) -> anyhow::Result<()> {
    use dialoguer::console::{style, Emoji};
    if let Ok(plug_core::ipc::IpcResponse::Status { servers, clients, uptime_secs }) = daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await {
        if matches!(output, OutputFormat::Text) {
            println!("{} {} (uptime: {}s) | {} {} client(s) connected", Emoji("🔌", ""), style("Plug Engine is running").green().bold(), uptime_secs, Emoji("👥", ""), style(clients.to_string()).bold());
            println!();
            if servers.is_empty() { println!("  No servers configured."); }
            else {
                println!("  {:<20} {:<15} {:<6}", style("SERVER").dim(), style("STATUS").dim(), style("TOOLS").dim());
                for s in &servers {
                    let (icon, health) = match s.health {
                        plug_core::types::ServerHealth::Healthy => (Emoji("🟢", ""), style("Healthy").green()),
                        plug_core::types::ServerHealth::Degraded => (Emoji("🟡", ""), style("Degraded").yellow()),
                        plug_core::types::ServerHealth::Failed => (Emoji("🔴", ""), style("Failed").red()),
                    };
                    println!("  {} {:<18} {:<23} {:<6}", icon, s.server_id, health, s.tool_count);
                }
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "uptime": uptime_secs, "clients": clients, "servers": servers }))?);
        }
        return Ok(());
    }
    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        println!("{} {}", Emoji("💤", ""), style("Plug Engine is not running.").yellow().bold());
        let mut names: Vec<_> = config.servers.keys().collect(); names.sort();
        for n in names { println!("  {} {:<18} {}", Emoji("⚪", ""), n, style("Not Running").dim()); }
    }
    Ok(())
}

/// `plug servers` — list all configured upstream servers.
async fn cmd_server_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::console::{style, Emoji};

    // 1. Try daemon first for live status
    if let Ok(response) = daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await {
        if let plug_core::ipc::IpcResponse::Status { servers, .. } = response {
            match output {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&servers)?);
                }
                OutputFormat::Text => {
                    if servers.is_empty() {
                        println!("No servers configured. Run {} to add some.", style("plug setup").cyan());
                        return Ok(());
                    }
                    println!("{} {} server(s) currently active:\n", Emoji("📡", ""), style(servers.len()).bold());
                    for s in servers {
                        let (icon, health) = match s.health {
                            plug_core::types::ServerHealth::Healthy => (Emoji("🟢", ""), style("Healthy").green()),
                            plug_core::types::ServerHealth::Degraded => (Emoji("🟡", ""), style("Degraded").yellow()),
                            plug_core::types::ServerHealth::Failed => (Emoji("🔴", ""), style("Failed").red()),
                        };
                        println!("  {} {:<18} {} ({} tools)", icon, style(&s.server_id).bold(), health, s.tool_count);
                    }
                }
            }
            return Ok(());
        }
    }

    // 2. Fallback to config only
    let config = plug_core::config::load_config(config_path)?;
    match output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&config.servers.keys().collect::<Vec<_>>())?);
        }
        OutputFormat::Text => {
            if config.servers.is_empty() {
                println!("No servers configured in config file. Run {} to add some.", style("plug setup").cyan());
            } else {
                let mut names: Vec<_> = config.servers.keys().collect();
                names.sort();
                println!("{} {} server(s) configured (daemon not running):\n", Emoji("⚙️", ""), style(names.len()).bold());
                for name in names {
                    println!("  {} {}", Emoji("⚪", ""), style(name).dim());
                }
            }
        }
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
    if let Ok(plug_core::ipc::IpcResponse::Ok) = daemon::ipc_request(&req).await { println!("stopped"); }
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

/// `plug tools` — list all available tools across all servers.
///
/// Tries to query a running daemon via IPC first for instant results.
/// Falls back to starting servers locally if daemon is not running.
async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    verbose: u8,
) -> anyhow::Result<()> {
    use dialoguer::console::{style, Emoji};
    use std::collections::BTreeMap;

    let mut tools_by_server = BTreeMap::new();

    // 1. Try daemon IPC first (instant)
    if let Ok(response) = daemon::ipc_request(&plug_core::ipc::IpcRequest::ListTools).await {
        if let plug_core::ipc::IpcResponse::Tools { tools } = response {
            for t in tools {
                tools_by_server
                    .entry(t.server_id)
                    .or_insert_with(Vec::new)
                    .push((t.name, t.description));
            }
        }
    }

    // 2. Fallback: start servers locally (heavy)
    if tools_by_server.is_empty() {
        let mut config = plug_core::config::load_config(config_path)?;
        
        // Anti-recursion shield: remove any server named "plug"
        config.servers.remove("plug");

        if verbose == 0 {
            eprintln!("{} Local discovery in progress (starting {} servers)...", Emoji("🔍", ""), config.servers.len());
            eprintln!("{}", style("   (Tip: Run 'plug serve --daemon' to make this instant and silent)").dim());
        }
        
        let mgr = Arc::new(plug_core::server::ServerManager::new());
        mgr.start_all(&config).await?;
        let tools = mgr.get_tools().await;
        for (srv, tool) in tools {
            tools_by_server
                .entry(srv)
                .or_insert_with(Vec::new)
                .push((tool.name.to_string(), tool.description.map(|d| d.to_string())));
        }
        mgr.shutdown_all().await;
    }

    // 3. Render
    match output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&tools_by_server)?);
        }
        OutputFormat::Text => {
            if tools_by_server.is_empty() {
                println!("No tools found. Run {} to add servers.", style("plug setup").cyan());
                return Ok(());
            }

            println!(
                "{} {} tools available across {} server(s)\n",
                Emoji("⚒️", ""),
                style(tools_by_server.values().map(|v| v.len()).sum::<usize>()).bold().green(),
                style(tools_by_server.len()).bold().cyan(),
            );

            for (server, mut tools) in tools_by_server {
                // Sort tools alphabetically by name
                tools.sort_by(|a, b| a.0.cmp(&b.0));

                println!(" {} {}", Emoji("📦", ""), style(server).bold().underlined());
                for (name, desc) in tools {
                    let name_styled = style(format!("{:<30}", name)).cyan();
                    if let Some(d) = desc {
                        let cleaned = d.replace('\n', " ").replace('\r', "");
                        let short_desc = if cleaned.len() > 70 {
                            format!("{}...", &cleaned[..67])
                        } else {
                            cleaned
                        };
                        println!("   • {} {}", name_styled, style(short_desc).dim());
                    } else {
                        println!("   • {}", name_styled);
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}

fn cmd_import(config_path: Option<&std::path::PathBuf>, clients: Option<Vec<String>>, dry_run: bool, output: &OutputFormat) -> anyhow::Result<()> {
    use dialoguer::console::{style, Emoji};
    use dialoguer::{MultiSelect, theme::ColorfulTheme};
    use plug_core::import::{self, ClientSource};

    let sources = match clients {
        Some(names) => names.iter().filter_map(|n| match n.as_str() {
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
            _ => None,
        }).collect(),
        None => ClientSource::all().to_vec(),
    };

    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };

    if matches!(output, OutputFormat::Text) {
        println!("{} {}", Emoji("🔍", ""), style("Scanning for MCP servers...").bold());
    }
    let report = import::import(&existing, &sources);

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for res in &report.scanned {
                if let Some(ref e) = res.error { eprintln!("  {} {} — {}", Emoji("⚠️", ""), res.source, style(e).red()); }
            }
            if report.new_servers.is_empty() { println!("\n{} No new servers found.", Emoji("✅", "")); return Ok(()); }
            if dry_run { return Ok(()); }

            let labels: Vec<_> = report.new_servers.iter().map(|s| format!("{:<15} {}", style(&s.name).bold(), style(format!("(from {})", s.source)).dim())).collect();
            let selections = MultiSelect::with_theme(&ColorfulTheme::default()).with_prompt("Select servers to import").items(&labels).defaults(&vec![true; labels.len()]).interact()?;
            if selections.is_empty() { return Ok(()); }

            let config_file = config_path.cloned().unwrap_or_else(plug_core::config::default_config_path);
            let to_import: Vec<plug_core::import::DiscoveredServer> = selections.iter().map(|&i| report.new_servers[i].clone()).collect();
            let toml = import::servers_to_toml(&to_import, &existing.keys().cloned().collect::<Vec<_>>());
            
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&config_file)?;
            file.write_all(toml.as_bytes())?;
            println!("\n{} Imported {} server(s).", Emoji("✨", ""), to_import.len());
        }
    }
    Ok(())
}

fn cmd_export() -> anyhow::Result<()> {
    use dialoguer::{Confirm, theme::ColorfulTheme, Input, MultiSelect, Select};
    println!("✨ Let's link Plug to your AI clients ✨\n");
    let all_clients = [
        ("Claude Desktop", "claude-desktop"), ("Claude Code", "claude-code"), ("Cursor", "cursor"),
        ("VS Code Copilot", "vscode"), ("Windsurf", "windsurf"), ("Gemini CLI", "gemini-cli"),
        ("Codex CLI", "codex-cli"), ("OpenCode", "opencode"), ("Zed", "zed"),
        ("Cline (VS Code)", "cline"), ("Cline CLI", "cline-cli"), ("RooCode", "roocode"),
        ("Factory", "factory"), ("Nanobot", "nanobot"), ("JetBrains Junie", "junie"),
        ("Kilo Code", "kilo"), ("Google Antigravity", "antigravity"),
    ];

    let mut items = Vec::new();
    for (display, target) in all_clients {
        let linked = is_linked(target, false);
        let installed = if let Ok(t) = target.parse::<plug_core::export::ExportTarget>() {
            if let Some(path) = plug_core::export::default_config_path(t, false) {
                if path.exists() { true }
                else if let Some(p) = path.parent() { p.exists() && !p.to_string_lossy().ends_with(".config") && p != dirs::home_dir().unwrap_or_default() }
                else { false }
            } else { false }
        } else { false };
        if linked || installed {
            let label = if linked { format!("{display} (Linked)") } else { format!("{display} (Detected)") };
            items.push((label, target, display, linked));
        }
    }

    if items.is_empty() {
        println!("No clients detected.");
        if !Confirm::with_theme(&ColorfulTheme::default()).with_prompt("Show all?").default(true).interact()? { return Ok(()); }
        for (display, target) in all_clients { items.push((display.to_string(), target, display, is_linked(target, false))); }
    } else {
        if Confirm::with_theme(&ColorfulTheme::default()).with_prompt("Show all supported clients?").default(false).interact()? {
            items.clear();
            for (display, target) in all_clients {
                let linked = is_linked(target, false);
                let label = if linked { format!("{display} (Linked)") } else { display.to_string() };
                items.push((label, target, display, linked));
            }
        }
    }

    let labels: Vec<_> = items.iter().map(|(l, ..)| l.clone()).collect();
    let defaults: Vec<_> = items.iter().map(|(.., l)| *l).collect();
    let selections = MultiSelect::with_theme(&ColorfulTheme::default()).with_prompt("Space to toggle [Linked], Enter to apply").items(&labels).defaults(&defaults).interact()?;

    for (idx, (_, target, _display, was_linked)) in items.iter().enumerate() {
        let is_selected = selections.contains(&idx);
        if is_selected && !was_linked { execute_export(target, false, 3282, true, false)?; }
        else if !is_selected && *was_linked { execute_unlink(target, false)?; }
    }

    println!();
    if Confirm::with_theme(&ColorfulTheme::default()).with_prompt("Configure custom client?").default(false).interact()? {
        let path_str: String = Input::with_theme(&ColorfulTheme::default()).with_prompt("Config path").interact_text()?;
        let path = if path_str.starts_with("~/") { dirs::home_dir().unwrap().join(&path_str[2..]) } else { std::path::PathBuf::from(path_str) };
        let format = Select::with_theme(&ColorfulTheme::default()).with_prompt("Format").items(&["JSON", "JSON (VS Code style)", "TOML"]).default(0).interact()?;
        let (snippet, is_toml) = match format {
            0 => (serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"command":"plug","args":["connect"]}}})).unwrap(), false),
            1 => (serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"command":"plug","args":["connect"]}}}})).unwrap(), false),
            2 => ("\n[mcp_servers.plug]\ncommand = \"plug\"\nargs = [\"connect\"]\n".to_string(), true),
            _ => unreachable!(),
        };
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let existing = if path.exists() { std::fs::read_to_string(&path)? } else { String::new() };
        let updated = if is_toml { let mut un = plug_core::import::unlink_toml(&existing); if !un.ends_with('\n') { un.push('\n'); } un.push_str(&snippet); un } else { merge_json_config(&existing, &snippet)? };
        std::fs::write(&path, updated)?;
    }
    Ok(())
}

fn execute_unlink(target: &str, project: bool) -> anyhow::Result<()> {
    let target_enum: plug_core::export::ExportTarget = target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let path = plug_core::export::default_config_path(target_enum, project).ok_or_else(|| anyhow::anyhow!("no path"))?;
    if !path.exists() { return Ok(()); }
    let existing = std::fs::read_to_string(&path)?;
    let unlinked = if path.extension().and_then(|e| e.to_str()) == Some("toml") { plug_core::import::unlink_toml(&existing) } else { unmerge_json_config(&existing)? };
    std::fs::write(&path, unlinked)?;
    Ok(())
}

fn is_linked(target: &str, project: bool) -> bool {
    let target_enum: plug_core::export::ExportTarget = match target.parse() { Ok(t) => t, Err(_) => return false };
    let path = match plug_core::export::default_config_path(target_enum, project) { Some(p) => p, None => return false };
    if !path.exists() { return false; }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if path.extension().and_then(|e| e.to_str()) == Some("toml") { content.contains("[mcp_servers.plug]") } else { content.contains("\"plug\":") }
}

fn unmerge_json_config(existing: &str) -> anyhow::Result<String> {
    let mut json: serde_json::Value = serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        for key in ["mcpServers", "context_servers"] { if let Some(inner) = obj.get_mut(key).and_then(|v| v.as_object_mut()) { inner.remove("plug"); } }
        if let Some(mcp) = obj.get_mut("mcp").and_then(|v| v.as_object_mut()) { if let Some(srv) = mcp.get_mut("servers").and_then(|v| v.as_object_mut()) { srv.remove("plug"); } }
    }
    Ok(serde_json::to_string_pretty(&json)?)
}

fn execute_export(target: &str, http: bool, port: u16, write: bool, project: bool) -> anyhow::Result<()> {
    use plug_core::export::{ExportOptions, ExportTarget, ExportTransport};
    let target_enum: ExportTarget = target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let transport = if http { ExportTransport::Http } else { ExportTransport::Stdio };
    let options = ExportOptions { target: target_enum, transport, port };
    let snippet = plug_core::export::export_config(&options);
    if write {
        let path = plug_core::export::default_config_path(target_enum, project).ok_or_else(|| anyhow::anyhow!("no path"))?;
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let existing = if path.exists() { std::fs::read_to_string(&path)? } else { String::new() };
        let updated = if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let mut unlinked = plug_core::import::unlink_toml(&existing); if !unlinked.ends_with('\n') { unlinked.push('\n'); } unlinked.push_str(&snippet); unlinked
        } else { merge_json_config(&existing, &snippet)? };
        std::fs::write(&path, updated)?;
    } else { println!("{snippet}"); }
    Ok(())
}

fn merge_json_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_json: serde_json::Value = serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    let snippet_json: serde_json::Value = serde_json::from_str(snippet)?;
    if let (Some(e_obj), Some(s_obj)) = (existing_json.as_object_mut(), snippet_json.as_object()) {
        for (k, v) in s_obj {
            if let (Some(e_inner), Some(s_inner)) = (e_obj.get_mut(k).and_then(|v| v.as_object_mut()), v.as_object()) {
                for (ik, iv) in s_inner { e_inner.insert(ik.clone(), iv.clone()); }
            } else { e_obj.insert(k.clone(), v.clone()); }
        }
    }
    Ok(serde_json::to_string_pretty(&existing_json)?)
}

async fn cmd_doctor(config_path: Option<&std::path::PathBuf>, output: &OutputFormat) -> anyhow::Result<()> {
    let resolved = config_path.cloned().unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(config_path)?;
    let report = plug_core::doctor::run_doctor(&config, &resolved).await;
    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for c in &report.checks { println!("[{:>4}] {}: {}", format!("{:?}", c.status), c.name, c.message); }
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

fn cmd_setup(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    use dialoguer::{Confirm, theme::ColorfulTheme};
    println!("✨ Welcome to Plug Setup ✨\n");
    let existing = match plug_core::config::load_config(config_path) { Ok(cfg) => cfg.servers, Err(_) => std::collections::HashMap::new() };
    let report = plug_core::import::import(&existing, plug_core::import::ClientSource::all());
    if !report.new_servers.is_empty() {
        println!("Found {} servers:", report.new_servers.len());
        if Confirm::with_theme(&ColorfulTheme::default()).with_prompt("Import them?").default(true).interact()? {
            let path = config_path.cloned().unwrap_or_else(plug_core::config::default_config_path);
            if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
            let toml = plug_core::import::servers_to_toml(&report.new_servers, &existing.keys().cloned().collect::<Vec<_>>());
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            file.write_all(toml.as_bytes())?;
        }
    }
    cmd_export()?;
    Ok(())
}

fn cmd_config(config_path: Option<&std::path::PathBuf>, path_only: bool) -> anyhow::Result<()> {
    let path = config_path.cloned().unwrap_or_else(plug_core::config::default_config_path);
    if path_only { println!("{}", path.display()); }
    else { if path.exists() { open::that(&path)?; } else { println!("Config missing at {}. Run setup.", path.display()); } }
    Ok(())
}
