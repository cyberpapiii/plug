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

use std::io::IsTerminal;
use std::sync::Arc;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use dialoguer::console::{style, Style};
use dialoguer::theme::ColorfulTheme;

const HELP_OVERVIEW: &str = "\
Workflow:
  Get started
    plug start              Start the background service
    plug setup              Discover servers and link clients
    plug link               Link plug to your AI clients

  Inspect
    plug status             Show runtime health and next actions
    plug clients            Show linked, detected, and live clients
    plug servers            Show configured servers
    plug tools              Show available tools
    plug doctor             Diagnose setup problems

  Maintain
    plug repair             Refresh linked client configs
    plug config check       Validate config syntax and rules
    plug config --path      Print config file path
    plug server add         Add a configured server
    plug tools disabled     Show disabled tool patterns

  Internal
    plug connect            stdio adapter invoked by AI clients
    plug serve --daemon     Run the background service
";

const HEADER_LINE: &str = "────────────────────────────────────────";
const MIN_CONTENT_WIDTH: usize = 24;

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::Blue.on_default().effects(Effects::BOLD))
        .placeholder(AnsiColor::Yellow.on_default())
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Red.on_default().effects(Effects::BOLD))
        .context(AnsiColor::White.on_default().dimmed())
}

fn cli_prompt_theme() -> ColorfulTheme {
    ColorfulTheme {
        defaults_style: Style::new().for_stderr().cyan().bold(),
        prompt_style: Style::new().for_stderr().bold().white(),
        prompt_prefix: style("◆".to_string()).for_stderr().cyan().bold(),
        prompt_suffix: style("›".to_string()).for_stderr().cyan(),
        success_prefix: style("●".to_string()).for_stderr().green().bold(),
        success_suffix: style("·".to_string()).for_stderr().black().bright(),
        error_prefix: style("✕".to_string()).for_stderr().red().bold(),
        error_style: Style::new().for_stderr().red().bold(),
        hint_style: Style::new().for_stderr().black().bright(),
        values_style: Style::new().for_stderr().green().bold(),
        active_item_style: Style::new().for_stderr().cyan().bold(),
        inactive_item_style: Style::new().for_stderr().white(),
        active_item_prefix: style("›".to_string()).for_stderr().cyan().bold(),
        inactive_item_prefix: style(" ".to_string()).for_stderr(),
        checked_item_prefix: style("◉".to_string()).for_stderr().green().bold(),
        unchecked_item_prefix: style("○".to_string()).for_stderr().black().bright(),
        picked_item_prefix: style("›".to_string()).for_stderr().cyan().bold(),
        unpicked_item_prefix: style(" ".to_string()).for_stderr(),
        ..ColorfulTheme::default()
    }
}

fn print_heading(title: &str) {
    println!("{}", style(title).bold().cyan());
    let width = terminal_width().min(HEADER_LINE.chars().count()).max(24);
    println!("{}", style(HEADER_LINE.chars().take(width).collect::<String>()).dim());
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|w| *w >= 40)
        .unwrap_or_else(|| console::Term::stdout().size().1 as usize)
        .max(40)
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                if word.chars().count() <= width {
                    current.push_str(word);
                } else {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        chunk.push(ch);
                        if chunk.chars().count() >= width {
                            lines.push(chunk);
                            chunk = String::new();
                        }
                    }
                    current = chunk;
                }
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                if word.chars().count() <= width {
                    current = word.to_string();
                } else {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        chunk.push(ch);
                        if chunk.chars().count() >= width {
                            lines.push(chunk);
                            chunk = String::new();
                        }
                    }
                    current = chunk;
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn print_wrapped_rows(
    prefix_text: &str,
    prefix_display: String,
    value: &str,
    width: usize,
    value_style: impl Fn(&str) -> console::StyledObject<&str>,
) {
    let content_width = width.saturating_sub(prefix_text.chars().count()).max(MIN_CONTENT_WIDTH);
    let lines = wrap_text(value, content_width);
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            println!("{prefix_display}{}", value_style(line));
        } else {
            println!("{}{}", " ".repeat(prefix_text.chars().count()), value_style(line));
        }
    }
}

fn print_label_value(label: &str, value: impl std::fmt::Display) {
    let prefix_text = format!("  {:<8} ", label);
    print_wrapped_rows(
        &prefix_text,
        format!("{}", style(&prefix_text).dim().bold()),
        &value.to_string(),
        terminal_width(),
        |line| style(line),
    );
}

fn print_next_action(index: usize, command: &str, description: &str) {
    let index_label = format!("{index}.");
    let prefix_text = format!("  {index_label:<2} {command:<18} ");
    print_wrapped_rows(
        &prefix_text,
        format!(
            "{} {} ",
            style(format!("  {index_label:<2}")).dim().bold(),
            style(format!("{command:<18}")).cyan().bold()
        ),
        description,
        terminal_width(),
        |line| style(line),
    );
}

fn print_banner(icon: &str, title: &str, subtitle: &str) {
    println!("{} {}", style(icon).cyan().bold(), style(title).bold().cyan());
    println!("{}", style(subtitle).dim());
    println!();
}

fn status_marker(health: &plug_core::types::ServerHealth) -> console::StyledObject<&'static str> {
    match health {
        plug_core::types::ServerHealth::Healthy => style("●").green().bold(),
        plug_core::types::ServerHealth::Degraded => style("!").yellow().bold(),
        plug_core::types::ServerHealth::Failed => style("×").red().bold(),
    }
}

fn status_label(health: &plug_core::types::ServerHealth) -> console::StyledObject<&'static str> {
    match health {
        plug_core::types::ServerHealth::Healthy => style("Healthy").green(),
        plug_core::types::ServerHealth::Degraded => style("Degraded").yellow(),
        plug_core::types::ServerHealth::Failed => style("Failed").red(),
    }
}

fn print_info_line(message: impl std::fmt::Display) {
    println!("{} {}", style("›").cyan().bold(), message);
}

fn print_success_line(message: impl std::fmt::Display) {
    println!("{} {}", style("•").green().bold(), message);
}

fn print_warning_line(message: impl std::fmt::Display) {
    println!("{} {}", style("!").yellow().bold(), message);
}

fn can_prompt_interactively() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LiveClientSupport {
    Supported,
    DaemonRestartRequired,
}

async fn fetch_live_clients() -> (Vec<plug_core::ipc::IpcClientInfo>, LiveClientSupport) {
    match daemon::ipc_request(&plug_core::ipc::IpcRequest::ListClients).await {
        Ok(plug_core::ipc::IpcResponse::Clients { clients }) => {
            (clients, LiveClientSupport::Supported)
        }
        Ok(plug_core::ipc::IpcResponse::Error { code, .. }) if code == "PARSE_ERROR" => {
            (Vec::new(), LiveClientSupport::DaemonRestartRequired)
        }
        _ => (Vec::new(), LiveClientSupport::Supported),
    }
}

fn load_editable_config(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<(std::path::PathBuf, plug_core::config::Config)> {
    use figment::providers::{Format, Serialized, Toml};
    use figment::Figment;

    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);

    let config = if path.exists() {
        Figment::new()
            .merge(Serialized::defaults(plug_core::config::Config::default()))
            .merge(Toml::file(&path))
            .extract()?
    } else {
        plug_core::config::Config::default()
    };

    Ok((path, config))
}

fn save_config(path: &std::path::Path, config: &plug_core::config::Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

fn parse_transport(value: Option<String>, url: &Option<String>) -> anyhow::Result<plug_core::config::TransportType> {
    match value.as_deref() {
        Some("stdio") | None if url.is_none() => Ok(plug_core::config::TransportType::Stdio),
        Some("http") => Ok(plug_core::config::TransportType::Http),
        None => Ok(plug_core::config::TransportType::Http),
        Some(other) => anyhow::bail!("unsupported transport `{other}`; use `stdio` or `http`"),
    }
}

#[derive(Parser)]
#[command(
    name = "plug",
    version,
    about = "MCP multiplexer — one config, every client connected",
    after_help = HELP_OVERVIEW,
    styles = cli_styles()
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
    /// Start the background plug service
    Start,
    #[command(display_order = 2)]
    /// Discover servers, import config, and link your AI clients
    Setup {
        /// Accept the default action for setup prompts
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 3)]
    /// Show runtime health and the next useful action
    Status,
    #[command(display_order = 4)]
    /// Diagnose problems with your plug setup
    Doctor,
    #[command(display_order = 5)]
    /// Refresh linked AI client configuration files
    Repair,
    #[command(display_order = 6)]
    /// Internal: reload service config from disk
    Reload,

    // ─── INSPECTION ──────────────────────────────────────────────────
    #[command(display_order = 7)]
    /// Show linked, detected, and live AI clients
    Clients,
    #[command(display_order = 8)]
    /// Show configured servers
    Servers,
    #[command(display_order = 9)]
    /// Show available tools from your servers
    Tools {
        #[command(subcommand)]
        command: Option<ToolCommands>,
    },
    #[command(display_order = 10)]
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
    #[command(display_order = 11)]
    /// Remove plug from your AI client configs
    Unlink {
        /// Unlink these clients without prompting (e.g. claude-code cursor)
        targets: Vec<String>,
        /// Unlink every currently linked client
        #[arg(long)]
        all: bool,
        /// Accept the default action for unlink prompts
        #[arg(long)]
        yes: bool,
    },

    // ─── SYSTEM / CLIENT COMMANDS ────────────────────────────────────
    #[command(display_order = 12)]
    /// Manage configured servers
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },

    #[command(display_order = 13)]
    /// Internal: start the stdio adapter AI clients invoke
    Connect,
    #[command(display_order = 14)]
    /// Internal: run plug as an HTTP/background service
    Serve {
        /// Also start stdio bridge on stdin/stdout
        #[arg(long)]
        stdio: bool,
        /// Run as headless daemon with IPC socket
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 15)]
    /// Internal: stop the background plug service
    Stop,

    // ─── ADVANCED CONFIG ─────────────────────────────────────────────
    #[command(display_order = 16)]
    /// Open the plug config file in your default editor
    Config {
        /// Just print the config path instead of opening it
        #[arg(long)]
        path: bool,
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },
    #[command(display_order = 17)]
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
    #[command(display_order = 18, hide = true)]
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

#[derive(Subcommand)]
enum ConfigCommands {
    /// Just print the config path instead of opening it
    Path,
    /// Check config syntax and validation rules
    Check,
}

#[derive(Subcommand)]
enum ServerCommands {
    /// Add a new configured server
    Add {
        name: Option<String>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, value_delimiter = ',')]
        args: Vec<String>,
        #[arg(long)]
        transport: Option<String>,
        #[arg(long)]
        disabled: bool,
    },
    /// Remove a configured server
    Remove {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Edit a configured server
    Edit {
        name: Option<String>,
    },
    /// Enable a configured server
    Enable {
        name: Option<String>,
    },
    /// Disable a configured server
    Disable {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum ToolCommands {
    /// Disable tools by exact name or wildcard pattern
    Disable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    /// Re-enable disabled tool patterns
    Enable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    /// Show disabled tool patterns
    Disabled,
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
            Some(Commands::Status) | Some(Commands::Servers) | Some(Commands::Tools { .. }) => {
                "none"
            }
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
        Some(Commands::Start) => {
            cmd_start(cli.config.as_ref(), &cli.output).await?;
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
        Some(Commands::Clients) => {
            cmd_client_list(cli.config.as_ref(), &cli.output).await?;
        }
        Some(Commands::Tools { command }) => {
            cmd_tool_command(cli.config.as_ref(), command, &cli.output, cli.verbose).await?;
        }
        Some(Commands::Link { targets, all, yes }) => {
            cmd_link(targets, all, yes)?;
        }
        Some(Commands::Unlink { targets, all, yes }) => {
            cmd_unlink(targets, all, yes)?;
        }
        Some(Commands::Server { command }) => {
            cmd_server_command(cli.config.as_ref(), command, &cli.output).await?;
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
        Some(Commands::Config { path, command }) => {
            cmd_config(cli.config.as_ref(), path, command, &cli.output)?;
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

async fn cmd_start(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started = ensure_daemon_with_feedback(config_path, false).await?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "start",
                "started": started,
                "running": daemon::connect_to_daemon().await.is_some(),
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "Service", "Background daemon");
    if started {
        print_success_line("Started background service.");
    } else {
        print_info_line("Background service is already running.");
    }
    Ok(())
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
    let (live_clients, live_client_support) = fetch_live_clients().await;
    let live_client_count = live_clients.len();

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
                "live_client_count": live_client_count,
                "live_client_support": live_client_support,
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

    print_banner("◆", "plug", "MCP multiplexer");

    if !config_exists {
        print_heading("Overview");
        print_label_value("Config", style("not found").yellow().bold());
        print_label_value("Path", style(config_path.display()).dim());
        println!();
        print_heading("Next");
        print_next_action(1, "plug setup", "Create config and link clients");
        print_next_action(2, "plug status", "Check runtime health once configured");
        return Ok(());
    }

    let config = plug_core::config::load_config(Some(&config_path))?;
    let daemon_running = daemon::connect_to_daemon().await.is_some();

    print_heading("Overview");
    print_label_value("Path", style(config_path.display()).dim());
    print_label_value("Servers", style(config.servers.len()).bold());
    print_label_value("Clients", style(linked_clients.len()).bold());
    match live_client_support {
        LiveClientSupport::Supported => {
            print_label_value("Live", style(live_client_count).bold());
        }
        LiveClientSupport::DaemonRestartRequired => {
            print_label_value("Live", style("restart required").yellow().bold());
        }
    }
    print_label_value(
        "Service",
        if daemon_running {
            style("running").green().bold()
        } else {
            style("stopped").yellow().bold()
        },
    );

    if !linked_clients.is_empty() {
        print_label_value("Linked", linked_clients.join(", "));
    }

    if matches!(
        live_client_support,
        LiveClientSupport::DaemonRestartRequired
    ) {
        println!();
        print_warning_line("Live client inspection requires restarting the background daemon after this upgrade.");
    }

    println!();
    print_heading("Next");
    if linked_clients.is_empty() {
        print_next_action(1, "plug link", "Link plug to your AI clients");
        print_next_action(2, "plug status", "Check runtime health");
    } else if daemon_running {
        print_next_action(1, "plug status", "Inspect runtime health");
        print_next_action(2, "plug doctor", "Diagnose configuration issues");
    } else {
        print_next_action(1, "plug status", "Start and inspect the service");
        print_next_action(2, "plug repair", "Refresh linked client configs");
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
    // Ensure daemon is running so we get a live status
    let started = ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    if let Ok(plug_core::ipc::IpcResponse::Status {
        servers,
        clients,
        uptime_secs,
    }) = daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
    {
        if matches!(output, OutputFormat::Text) {
            print_banner("◆", "Runtime", "Live service health");
            if started {
                println!();
            }
            print_heading("Service");
            print_label_value("Status", style("running").green().bold());
            print_label_value("Uptime", style(format!("{uptime_secs}s")).bold());
            print_label_value("Clients", style(clients.to_string()).bold());
            println!();
            if servers.is_empty() {
                print_heading("Servers");
                println!("  No servers configured.");
            } else {
                print_heading("Servers");
                println!(
                    "  {:<2} {:<18} {:<12} {:>5}",
                    style("").dim(),
                    style("SERVER").dim(),
                    style("STATUS").dim(),
                    style("TOOLS").dim()
                );
                println!("  {}", style("------------------------------------------------").dim());
                for s in &servers {
                    if s.server_id == "__plug_internal__" {
                        continue;
                    }
                    println!(
                        "  {} {:<18} {:<12} {:>5}",
                        status_marker(&s.health),
                        s.server_id,
                        status_label(&s.health),
                        s.tool_count
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
        print_banner("◆", "Runtime unavailable", "Service is not currently reachable");
        println!();
        print_heading("Configured servers");
        let mut names: Vec<_> = config.servers.keys().collect();
        names.sort();
        for n in names {
            println!(
                "  {} {:<18} {}",
                style("·").dim(),
                n,
                style("not running").dim()
            );
        }
    }
    Ok(())
}

async fn cmd_server_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    loop {
        if let Ok(plug_core::ipc::IpcResponse::Status { servers, .. }) =
            daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
        {
            match output {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&servers)?);
                    return Ok(());
                }
                OutputFormat::Text => {
                    if servers.is_empty() {
                        println!("No servers configured.");
                    } else {
                        print_banner(
                            "◆",
                            "Servers",
                            &format!("{} server(s) active", servers.len().saturating_sub(1)),
                        );
                        if started {
                            println!();
                        }
                        for s in servers {
                            if s.server_id == "__plug_internal__" {
                                continue;
                            }
                            println!(
                                "  {} {:<18} {} ({} tools)",
                                status_marker(&s.health),
                                style(&s.server_id).bold(),
                                status_label(&s.health),
                                s.tool_count
                            );
                        }
                    }
                }
            }
        } else {
            let config = plug_core::config::load_config(config_path)?;
            if matches!(output, OutputFormat::Text) {
                let mut names: Vec<_> = config.servers.keys().collect();
                names.sort();
                print_banner(
                    "◆",
                    "Servers",
                    &format!("{} server(s) configured (daemon not running)", names.len()),
                );
                for n in names {
                    println!("  {} {}", style("·").dim(), style(n).dim());
                }
            }
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_server_actions(config_path, output).await? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}

async fn cmd_client_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    loop {
        let (live, live_client_support) = fetch_live_clients().await;
        let clients = client_views(&live);

        if matches!(output, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "clients": clients,
                    "live_client_support": live_client_support,
                }))?
            );
            return Ok(());
        }

        print_banner("◆", "Clients", "Linked, detected, and live AI clients");
        if started {
            println!();
        }
        if matches!(
            live_client_support,
            LiveClientSupport::DaemonRestartRequired
        ) {
            print_warning_line("Live client inspection requires restarting the background daemon after this upgrade.");
            println!();
        }
        println!(
            "  {:<24} {:<10} {:<10} {:<6}",
            style("CLIENT").dim(),
            style("LINKED").dim(),
            style("DETECTED").dim(),
            style("LIVE").dim()
        );
        println!("  {}", style("----------------------------------------------------------").dim());
        for client in &clients {
            let linked = if client.linked {
                style("yes").green().bold()
            } else {
                style("no").dim()
            };
            let detected = if client.detected {
                style("yes").cyan()
            } else {
                style("no").dim()
            };
            let live_label = if client.live {
                style(format!("yes ({})", client.live_sessions)).green().bold().to_string()
            } else {
                style("no").dim().to_string()
            };
            println!(
                "  {:<24} {:<10} {:<10} {:<6}",
                client.name,
                linked,
                detected,
                live_label
            );
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_client_actions()? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}

async fn ensure_daemon_with_feedback(
    config_path: Option<&std::path::PathBuf>,
    announce: bool,
) -> anyhow::Result<bool> {
    if daemon::connect_to_daemon().await.is_none() {
        auto_start_daemon(config_path)?;
        wait_for_daemon_ready().await?;
        if announce {
            print_info_line("Started background service.");
        }
        return Ok(true);
    }
    Ok(false)
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
    started: Option<bool>,
) -> anyhow::Result<()> {
    use dialoguer::console::style;
    use std::collections::BTreeMap;

    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = match started {
        Some(started) => started,
        None => ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?,
    };

    loop {
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

        let unique_servers: std::collections::BTreeSet<&str> =
            all_tools.iter().map(|t| t.server_id.as_str()).collect();

        match output {
            OutputFormat::Json => {
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
                return Ok(());
            }
            OutputFormat::Text => {
                if tools_by_prefix.is_empty() {
                    println!(
                        "No tools found. Run {} to add servers.",
                        style("plug setup").cyan()
                    );
                    return Ok(());
                }
                let term_width = terminal_width();
                let available_width = term_width.saturating_sub(40);
                print_banner(
                    "◆",
                    "Tools",
                    &format!(
                        "{} tools across {} server(s)",
                        all_tools.len(),
                        unique_servers.len()
                    ),
                );
                if started {
                    println!();
                }
                for (prefix, mut tools) in tools_by_prefix {
                    tools.sort_by(|a, b| a.0.cmp(&b.0));
                    let server_id = &tools[0].1;
                    let annotation = if server_id != &prefix {
                        format!(" {}", style(format!("[{}]", server_id)).dim())
                    } else {
                        String::new()
                    };
                    println!(
                        "{} {} {}{}",
                        style("▸").cyan().bold(),
                        style(&prefix).bold(),
                        style(format!("{} tools", tools.len())).dim(),
                        annotation
                    );
                    for (name, _server_id, title, desc) in &tools {
                        let name_styled = style(format!("  │ {:<28}", name)).cyan();
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

        if !interactive {
            break;
        }
        if !prompt_tool_actions(config_path).await? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}

async fn cmd_tool_command(
    config_path: Option<&std::path::PathBuf>,
    command: Option<ToolCommands>,
    output: &OutputFormat,
    verbose: u8,
) -> anyhow::Result<()> {
    match command {
        None => cmd_tool_list(config_path, output, verbose, None).await,
        Some(ToolCommands::Disabled) => cmd_tool_disabled(config_path, output),
        Some(ToolCommands::Disable { server, patterns }) => {
            cmd_tool_disable(config_path, tool_patterns_for_server(server, patterns)?).await
        }
        Some(ToolCommands::Enable { server, patterns }) => {
            cmd_tool_enable(config_path, tool_patterns_for_server(server, patterns)?)
        }
    }
}

fn tool_patterns_for_server(
    server: Option<String>,
    mut patterns: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    if let Some(server) = server {
        if !patterns.is_empty() {
            anyhow::bail!("pass either patterns or `--server`, not both");
        }
        patterns.push(format!("{server}__*"));
    }
    Ok(patterns)
}

fn prompt_client_actions() -> anyhow::Result<bool> {
    use dialoguer::Select;

    let options = ["Done", "Link clients", "Unlink clients"];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Manage clients")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_link(Vec::new(), false, false)?;
            Ok(true)
        }
        Some(2) => {
            cmd_unlink(Vec::new(), false, false)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

async fn prompt_server_actions(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<bool> {
    use dialoguer::Select;

    let options = [
        "Done",
        "Add server",
        "Edit server",
        "Remove server",
        "Enable server",
        "Disable server",
    ];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Manage servers")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_server_add(config_path, None, None, None, Vec::new(), None, false)?;
            Ok(true)
        }
        Some(2) => {
            cmd_server_edit(config_path, None, output).await?;
            Ok(true)
        }
        Some(3) => {
            cmd_server_remove(config_path, None, false)?;
            Ok(true)
        }
        Some(4) => {
            cmd_server_set_enabled(config_path, None, true)?;
            Ok(true)
        }
        Some(5) => {
            cmd_server_set_enabled(config_path, None, false)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

async fn prompt_tool_actions(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<bool> {
    use dialoguer::Select;

    let options = ["Done", "Disable tools", "Enable tools", "Show disabled patterns"];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Manage tools")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_tool_disable(config_path, Vec::new()).await?;
            Ok(true)
        }
        Some(2) => {
            cmd_tool_enable(config_path, Vec::new())?;
            Ok(true)
        }
        Some(3) => {
            cmd_tool_disabled(config_path, &OutputFormat::Text)?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn cmd_tool_disabled(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let (path, config) = load_editable_config(config_path)?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path,
                "disabled_tools": config.disabled_tools,
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "Disabled tools", "Configured exact names and wildcard patterns");
    print_label_value("Path", style(path.display()).dim());
    if config.disabled_tools.is_empty() {
        println!();
        print_info_line("No disabled tool patterns configured.");
        return Ok(());
    }
    println!();
    for pattern in config.disabled_tools {
        println!("  {} {}", style("·").dim(), pattern);
    }
    Ok(())
}

async fn cmd_tool_disable(
    config_path: Option<&std::path::PathBuf>,
    mut patterns: Vec<String>,
) -> anyhow::Result<()> {
    use dialoguer::MultiSelect;

    let (path, mut config) = load_editable_config(config_path)?;

    if patterns.is_empty() {
        let _ = ensure_daemon_with_feedback(config_path, true).await?;
        let mut all_tools: Vec<String> = if let Ok(plug_core::ipc::IpcResponse::Tools { tools }) =
            daemon::ipc_request(&plug_core::ipc::IpcRequest::ListTools).await
        {
            tools.into_iter()
                .filter(|tool| tool.server_id != "__plug_internal__")
                .map(|tool| tool.name)
                .collect()
        } else {
            Vec::new()
        };
        all_tools.sort();
        all_tools.dedup();
        if all_tools.is_empty() {
            anyhow::bail!("no live tools available to disable");
        }

        let selections = MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Select tools to disable")
            .items(&all_tools)
            .interact()?;
        patterns = selections
            .into_iter()
            .map(|index| all_tools[index].clone())
            .collect();
        if patterns.is_empty() {
            return Ok(());
        }
    }

    let mut added = Vec::new();
    for pattern in patterns {
        if !config.disabled_tools.iter().any(|existing| existing == &pattern) {
            config.disabled_tools.push(pattern.clone());
            added.push(pattern);
        }
    }
    config.disabled_tools.sort();
    save_config(&path, &config)?;

    if added.is_empty() {
        print_info_line("No new disabled tool patterns were added.");
    } else {
        print_success_line(format!("Disabled {} tool pattern(s).", added.len()));
    }
    Ok(())
}

fn cmd_tool_enable(
    config_path: Option<&std::path::PathBuf>,
    mut patterns: Vec<String>,
) -> anyhow::Result<()> {
    use dialoguer::MultiSelect;

    let (path, mut config) = load_editable_config(config_path)?;
    if config.disabled_tools.is_empty() {
        print_info_line("No disabled tool patterns configured.");
        return Ok(());
    }

    if patterns.is_empty() {
        let selections = MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Select disabled patterns to re-enable")
            .items(&config.disabled_tools)
            .defaults(&vec![false; config.disabled_tools.len()])
            .interact()?;
        patterns = selections
            .into_iter()
            .map(|index| config.disabled_tools[index].clone())
            .collect();
        if patterns.is_empty() {
            return Ok(());
        }
    }

    let before = config.disabled_tools.len();
    config
        .disabled_tools
        .retain(|existing| !patterns.iter().any(|pattern| pattern == existing));
    save_config(&path, &config)?;

    let removed = before.saturating_sub(config.disabled_tools.len());
    if removed == 0 {
        print_info_line("No matching disabled tool patterns were found.");
    } else {
        print_success_line(format!("Re-enabled {} tool pattern(s).", removed));
    }
    Ok(())
}

async fn cmd_server_command(
    config_path: Option<&std::path::PathBuf>,
    command: ServerCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ServerCommands::Add {
            name,
            command,
            url,
            args,
            transport,
            disabled,
        } => cmd_server_add(config_path, name, command, url, args, transport, disabled),
        ServerCommands::Remove { name, yes } => cmd_server_remove(config_path, name, yes),
        ServerCommands::Edit { name } => cmd_server_edit(config_path, name, output).await,
        ServerCommands::Enable { name } => cmd_server_set_enabled(config_path, name, true),
        ServerCommands::Disable { name } => cmd_server_set_enabled(config_path, name, false),
    }
}

fn cmd_server_add(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    command: Option<String>,
    url: Option<String>,
    args: Vec<String>,
    transport: Option<String>,
    disabled: bool,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, Select};

    let (path, mut config) = load_editable_config(config_path)?;
    let name = match name {
        Some(name) => name,
        None => Input::with_theme(&cli_prompt_theme())
            .with_prompt("Server name")
            .interact_text()?,
    };

    if config.servers.contains_key(&name) {
        anyhow::bail!("server `{name}` already exists");
    }

    let provided_transport = transport.clone();
    let non_interactive = provided_transport.is_some() || command.is_some() || url.is_some() || !args.is_empty();
    let transport = match transport {
        Some(value) => parse_transport(Some(value), &url)?,
        None if command.is_some() => plug_core::config::TransportType::Stdio,
        None if url.is_some() => plug_core::config::TransportType::Http,
        None => match Select::with_theme(&cli_prompt_theme())
            .with_prompt("Transport")
            .items(["stdio", "http"])
            .default(0)
            .interact()?
        {
            0 => plug_core::config::TransportType::Stdio,
            _ => plug_core::config::TransportType::Http,
        },
    };

    let server = match transport {
        plug_core::config::TransportType::Stdio => {
            let command = match command {
                Some(command) => command,
                None => Input::with_theme(&cli_prompt_theme())
                    .with_prompt("Command")
                    .interact_text()?,
            };
            let args = if args.is_empty() {
                let value: String = Input::with_theme(&cli_prompt_theme())
                    .with_prompt("Args (space-separated, optional)")
                    .allow_empty(true)
                    .interact_text()?;
                if value.trim().is_empty() {
                    Vec::new()
                } else {
                    value.split_whitespace().map(|part| part.to_string()).collect()
                }
            } else {
                args
            };
            plug_core::config::ServerConfig {
                command: Some(command),
                args,
                env: std::collections::HashMap::new(),
                enabled: !disabled,
                transport,
                url: None,
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: std::collections::HashMap::new(),
                tool_groups: Vec::new(),
            }
        }
        plug_core::config::TransportType::Http => {
            let url = match url {
                Some(url) => url,
                None => Input::with_theme(&cli_prompt_theme())
                    .with_prompt("URL")
                    .interact_text()?,
            };
            let enabled = if disabled {
                false
            } else if non_interactive {
                true
            } else {
                Confirm::with_theme(&cli_prompt_theme())
                    .with_prompt("Enable immediately?")
                    .default(true)
                    .interact()?
            };
            plug_core::config::ServerConfig {
                command: None,
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                enabled,
                transport,
                url: Some(url),
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: std::collections::HashMap::new(),
                tool_groups: Vec::new(),
            }
        }
    };

    config.servers.insert(name.clone(), server);
    save_config(&path, &config)?;
    print_success_line(format!("Added server `{name}`."));
    Ok(())
}

fn cmd_server_remove(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Select};

    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers to remove.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt("Select a server to remove")
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    if !config.servers.contains_key(&name) {
        anyhow::bail!("unknown server `{name}`");
    }

    if !yes
        && !Confirm::with_theme(&cli_prompt_theme())
            .with_prompt(format!("Remove server `{name}`?"))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    config.servers.remove(&name);
    save_config(&path, &config)?;
    print_success_line(format!("Removed server `{name}`."));
    Ok(())
}

async fn cmd_server_edit(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, Select};

    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers to edit.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt("Select a server to edit")
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    let server = config
        .servers
        .get_mut(&name)
        .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;

    if matches!(output, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(server)?);
        return Ok(());
    }

    let enabled = Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Enabled?")
        .default(server.enabled)
        .interact()?;
    server.enabled = enabled;

    match server.transport {
        plug_core::config::TransportType::Stdio => {
            let command: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("Command")
                .with_initial_text(server.command.clone().unwrap_or_default())
                .interact_text()?;
            let args: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("Args (space-separated)")
                .with_initial_text(server.args.join(" "))
                .allow_empty(true)
                .interact_text()?;
            server.command = Some(command);
            server.args = if args.trim().is_empty() {
                Vec::new()
            } else {
                args.split_whitespace().map(|part| part.to_string()).collect()
            };
        }
        plug_core::config::TransportType::Http => {
            let url: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("URL")
                .with_initial_text(server.url.clone().unwrap_or_default())
                .interact_text()?;
            server.url = Some(url);
        }
    }

    save_config(&path, &config)?;
    print_success_line(format!("Updated server `{name}`."));
    Ok(())
}

fn cmd_server_set_enabled(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    enabled: bool,
) -> anyhow::Result<()> {
    use dialoguer::Select;

    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers found.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt(if enabled {
                    "Select a server to enable"
                } else {
                    "Select a server to disable"
                })
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    let server = config
        .servers
        .get_mut(&name)
        .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;
    server.enabled = enabled;
    save_config(&path, &config)?;
    if enabled {
        print_success_line(format!("Enabled server `{name}`."));
    } else {
        print_success_line(format!("Disabled server `{name}`."));
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
    use dialoguer::console::style;
    use dialoguer::MultiSelect;
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
        print_banner("◆", "Import", "Scan existing AI client configs for MCP servers");
        print_info_line(style("Scanning for MCP servers...").bold());
    }
    let report = import::import(&existing, &sources);

    match output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for res in &report.scanned {
                if let Some(ref e) = res.error {
                    eprintln!(
                        "  {} {:<16} {}",
                        style("!").yellow().bold(),
                        res.source,
                        style(e).red()
                    );
                }
            }
            if report.new_servers.is_empty() {
                println!();
                print_success_line("No new servers found.");
                return Ok(());
            }
            if dry_run {
                println!();
                print_success_line(format!("Found {} importable server(s).", report.new_servers.len()));
                return Ok(());
            }

            println!();
            print_heading("Discovered");
            for server in &report.new_servers {
                println!(
                    "  {} {:<18} {}",
                    style("·").dim(),
                    style(&server.name).bold(),
                    style(format!("from {}", server.source)).dim()
                );
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
                MultiSelect::with_theme(&cli_prompt_theme())
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
            println!();
            print_success_line(format!("Imported {} server(s).", to_import.len()));
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

fn is_detected(target: &str) -> bool {
    if let Ok(t) = target.parse::<plug_core::export::ExportTarget>() {
        if let Some(path) = plug_core::export::default_config_path(t, false) {
            if path.exists() {
                true
            } else if let Some(parent) = path.parent() {
                parent.exists()
                    && !parent.to_string_lossy().ends_with(".config")
                    && parent != dirs::home_dir().unwrap_or_default()
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct ClientView {
    name: String,
    target: String,
    linked: bool,
    detected: bool,
    live: bool,
    live_sessions: usize,
}

fn client_target_from_info(client_info: Option<&str>) -> Option<&'static str> {
    let info = client_info?;
    match plug_core::client_detect::detect_client(info) {
        plug_core::types::ClientType::ClaudeDesktop => Some("claude-desktop"),
        plug_core::types::ClientType::ClaudeCode => Some("claude-code"),
        plug_core::types::ClientType::Cursor => Some("cursor"),
        plug_core::types::ClientType::Windsurf => Some("windsurf"),
        plug_core::types::ClientType::VSCodeCopilot => Some("vscode"),
        plug_core::types::ClientType::GeminiCli => Some("gemini-cli"),
        plug_core::types::ClientType::CodexCli => Some("codex-cli"),
        plug_core::types::ClientType::OpenCode => Some("opencode"),
        plug_core::types::ClientType::Zed => Some("zed"),
        plug_core::types::ClientType::Unknown => None,
    }
}

fn client_views(live: &[plug_core::ipc::IpcClientInfo]) -> Vec<ClientView> {
    let mut live_counts: std::collections::HashMap<&'static str, usize> = std::collections::HashMap::new();
    for session in live {
        if let Some(target) = client_target_from_info(session.client_info.as_deref()) {
            *live_counts.entry(target).or_insert(0) += 1;
        }
    }

    let mut views = all_client_targets()
        .iter()
        .map(|(name, target)| {
            let linked = is_linked(target, false);
            let detected = is_detected(target);
            let live_sessions = *live_counts.get(target).unwrap_or(&0);
            ClientView {
                name: (*name).to_string(),
                target: (*target).to_string(),
                linked,
                detected,
                live: live_sessions > 0,
                live_sessions,
            }
        })
        .collect::<Vec<_>>();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

fn detected_or_linked_clients() -> Vec<(&'static str, &'static str, bool)> {
    let mut items = Vec::new();
    for (display, target) in all_client_targets() {
        let linked = is_linked(target, false);
        let installed = is_detected(target);
        if linked || installed {
            items.push((*display, *target, linked));
        }
    }
    items
}

fn cmd_link(targets: Vec<String>, all: bool, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, MultiSelect, Select};

    if !targets.is_empty() {
        for target in &targets {
            execute_export(target, false, 3282, true, false)?;
        }
        return Ok(());
    }

    print_banner("◆", "Link clients", "Choose which AI clients should point at plug");

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
                format!("{display}  {}", style("[linked]").green().dim())
            } else {
                format!("{display}  {}", style("[detected]").cyan().dim())
            };
            (label, target, display, linked)
        })
        .collect::<Vec<_>>();

    if items.is_empty() {
        print_warning_line("No clients detected.");
        if yes {
            println!(
                "Pass explicit targets like `plug link claude-code cursor` or run `plug link` interactively."
            );
            return Ok(());
        }
        if Confirm::with_theme(&cli_prompt_theme())
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
        && Confirm::with_theme(&cli_prompt_theme())
            .with_prompt("Show all supported clients?")
            .default(false)
            .interact()?
    {
        items.clear();
        for (display, target) in all_client_targets() {
            let linked = is_linked(target, false);
            let label = if linked {
                format!("{display}  {}", style("[linked]").green().dim())
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
        MultiSelect::with_theme(&cli_prompt_theme())
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
    if Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Configure custom client?")
        .default(false)
        .interact()?
    {
        let path_str: String = Input::with_theme(&cli_prompt_theme())
            .with_prompt("Config path")
            .interact_text()?;
        let path = if let Some(stripped) = path_str.strip_prefix("~/") {
            dirs::home_dir().unwrap().join(stripped)
        } else {
            std::path::PathBuf::from(path_str)
        };
        let format = Select::with_theme(&cli_prompt_theme())
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

fn cmd_unlink(targets: Vec<String>, all: bool, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, MultiSelect};

    if !targets.is_empty() {
        for target in &targets {
            execute_unlink(target, false)?;
        }
        return Ok(());
    }

    let items = all_client_targets()
        .iter()
        .filter(|(_, target)| is_linked(target, false))
        .map(|(display, target)| (display.to_string(), *target))
        .collect::<Vec<_>>();

    if items.is_empty() {
        print_warning_line("No linked clients found.");
        return Ok(());
    }

    print_banner("◆", "Unlink clients", "Remove plug from selected AI client configs");

    if all {
        for (_, target) in &items {
            execute_unlink(target, false)?;
        }
        return Ok(());
    }

    if yes {
        for (_, target) in &items {
            execute_unlink(target, false)?;
        }
        return Ok(());
    }

    if !Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Choose which linked clients to remove?")
        .default(true)
        .interact()?
    {
        return Ok(());
    }

    let labels = items.iter().map(|(display, _)| display.clone()).collect::<Vec<_>>();
    let selections = MultiSelect::with_theme(&cli_prompt_theme())
        .with_prompt("Space to toggle, Enter to unlink")
        .items(&labels)
        .defaults(&vec![true; labels.len()])
        .interact()?;

    for index in selections {
        execute_unlink(items[index].1, false)?;
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
            print_banner("◆", "Doctor", "Diagnose problems with your plug setup");
            for c in &report.checks {
                let marker = match c.status {
                    plug_core::doctor::CheckStatus::Pass => style("●").green().bold(),
                    plug_core::doctor::CheckStatus::Warn => style("!").yellow().bold(),
                    plug_core::doctor::CheckStatus::Fail => style("×").red().bold(),
                };
                let prefix_text = format!("  {} {:<24} ", "•", c.name);
                let prefix_display = format!(
                    "  {} {} ",
                    marker,
                    style(format!("{:<24}", c.name)).bold()
                );
                print_wrapped_rows(
                    &prefix_text,
                    prefix_display,
                    &c.message,
                    terminal_width(),
                    |line| style(line),
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
    use dialoguer::Confirm;

    print_banner(
        "◆",
        "Plug setup",
        "Discover servers, import config, and link your AI clients",
    );
    let existing = match plug_core::config::load_config(config_path) {
        Ok(cfg) => cfg.servers,
        Err(_) => std::collections::HashMap::new(),
    };
    let report = plug_core::import::import(&existing, plug_core::import::ClientSource::all());
    if !report.new_servers.is_empty() {
        print_heading("Discovered");
        print_success_line(format!("Found {} server(s).", report.new_servers.len()));
        for server in &report.new_servers {
            println!(
                "  {} {:<18} {}",
                style("·").dim(),
                style(&server.name).bold(),
                style(format!("from {}", server.source)).dim()
            );
        }
        println!();
        if yes
            || Confirm::with_theme(&cli_prompt_theme())
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

fn cmd_config(
    config_path: Option<&std::path::PathBuf>,
    path_only: bool,
    command: Option<ConfigCommands>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    if path_only {
        println!("{}", path.display());
        return Ok(());
    }

    match command {
        Some(ConfigCommands::Path) => {
            println!("{}", path.display());
        }
        Some(ConfigCommands::Check) => {
            let exists = path.exists();
            let result = if exists {
                match plug_core::config::load_config(Some(&path)) {
                    Ok(config) => {
                        let errors = plug_core::config::validate_config(&config);
                        serde_json::json!({
                            "path": path,
                            "exists": true,
                            "valid": errors.is_empty(),
                            "errors": errors
                        })
                    }
                    Err(error) => serde_json::json!({
                        "path": path,
                        "exists": true,
                        "valid": false,
                        "errors": [error.to_string()]
                    }),
                }
            } else {
                serde_json::json!({
                    "path": path,
                    "exists": false,
                    "valid": false,
                    "errors": ["config file not found"]
                })
            };

            if matches!(output, OutputFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_banner("◆", "Config check", "Validate config syntax and core rules");
                print_label_value("Path", style(path.display()).dim());
                if !exists {
                    print_warning_line("Config file not found.");
                } else if let Some(errors) = result.get("errors").and_then(|v| v.as_array()) {
                    if errors.is_empty() {
                        print_success_line("Config is valid.");
                    } else {
                        println!();
                        print_heading("Issues");
                        for error in errors {
                            if let Some(error) = error.as_str() {
                                println!("  {} {}", style("×").red().bold(), error);
                            }
                        }
                    }
                }
            }
        }
        None => {
            if path.exists() {
                open::that(&path)?;
            } else {
                println!("Config missing at {}. Run setup.", path.display());
            }
        }
    }
    Ok(())
}

/// `plug repair` — clean up and refresh all client configurations.
fn cmd_repair() -> anyhow::Result<()> {
    use dialoguer::console::style;

    println!(
        "{} {}",
        style("◆").cyan().bold(),
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
            print!("  {} Refreshing {}... ", style("›").cyan().bold(), target);
            if let Err(e) = execute_export(target, false, 3282, true, false) {
                println!("{}", style(format!("failed: {e}")).red());
            } else {
                println!("{}", style("done").green());
                repaired_count += 1;
            }
        }
    }

    if repaired_count == 0 {
        println!("\n{} No linked clients found to repair.", style("•").green().bold());
    } else {
        println!(
            "\n{} Successfully repaired {} client configuration(s).",
            style("•").green().bold(),
            repaired_count
        );
    }

    Ok(())
}
