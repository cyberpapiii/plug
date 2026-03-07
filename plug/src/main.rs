#![deny(unsafe_code)]

/// Load `.env` file vars into the process environment.
///
/// SAFETY: Called before tokio runtime starts (single-threaded at this point).
/// `set_var` is unsafe in Rust 2024 because it's not thread-safe, but we're
/// guaranteed single-threaded here since this runs before `#[tokio::main]`.
#[allow(unsafe_code)]
fn apply_dotenv() {
    for (key, value) in plug_core::dotenv::load_dotenv() {
        unsafe {
            std::env::set_var(&key, &value);
        }
    }
}

mod commands;
mod daemon;
mod ipc_proxy;
mod runtime;
mod ui;
mod views;

use clap::{Parser, Subcommand};

const HELP_OVERVIEW: &str = "\
Workflow:
  Get started
    plug start              Start the background service
    plug setup              Discover servers and link clients
    plug clients            View and manage AI clients

  Inspect
    plug status             Show runtime health and next actions
    plug clients            Show linked, detected, and live clients
    plug servers            View and manage configured servers
    plug tools              View and manage available tools
    plug doctor             Diagnose setup problems

  Maintain
    plug repair             Refresh linked client configs
    plug config check       Validate config syntax and rules
    plug config --path      Print config file path
    plug link               Link plug to your AI clients
    plug unlink             Remove plug from your AI client configs

  Internal
    plug connect            stdio adapter invoked by AI clients
    plug serve --daemon     Run the background service
";

#[derive(Parser)]
#[command(
    name = "plug",
    version,
    about = "MCP multiplexer — one config, every client connected",
    after_help = HELP_OVERVIEW,
    styles = ui::cli_styles()
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
pub(crate) enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    #[command(display_order = 1)]
    /// Start the background plug service
    Start,
    #[command(display_order = 2)]
    /// Discover servers, import config, and link your AI clients
    Setup {
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 3)]
    /// Show runtime health and the next useful action
    Status {
        /// Reveal the HTTP auth token (hidden by default)
        #[arg(long)]
        show_token: bool,
    },
    #[command(display_order = 4)]
    /// Diagnose problems with your plug setup
    Doctor,
    #[command(display_order = 5)]
    /// Refresh linked AI client configuration files
    Repair,
    #[command(display_order = 6)]
    /// Internal: reload service config from disk
    Reload,
    #[command(display_order = 7)]
    /// View and manage linked, detected, and live AI clients
    Clients,
    #[command(display_order = 8)]
    /// View and manage configured servers
    Servers,
    #[command(display_order = 9)]
    /// View and manage available tools from your servers
    Tools {
        #[command(subcommand)]
        command: Option<ToolCommands>,
    },
    #[command(display_order = 10)]
    /// Link plug to your AI clients
    Link {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 11)]
    /// Remove plug from your AI client configs
    Unlink {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
    },
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
        #[arg(long)]
        daemon: bool,
    },
    #[command(display_order = 15)]
    /// Internal: stop the background plug service
    Stop,
    #[command(display_order = 16)]
    /// Open the plug config file in your default editor
    Config {
        #[arg(long)]
        path: bool,
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },
    #[command(display_order = 17)]
    /// Advanced: import MCP servers from existing AI client configs
    Import {
        #[arg(long, value_delimiter = ',')]
        clients: Option<Vec<String>>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    #[command(display_order = 18, hide = true)]
    /// Compatibility alias for `plug link`
    Export {
        targets: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    Path,
    Check,
}

#[derive(Subcommand)]
pub(crate) enum ServerCommands {
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
    Remove {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    Edit {
        name: Option<String>,
    },
    Enable {
        name: Option<String>,
    },
    Disable {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum ToolCommands {
    Disable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    Enable {
        #[arg(long)]
        server: Option<String>,
        patterns: Vec<String>,
    },
    Disabled,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    apply_dotenv();
    plug_core::tls::ensure_rustls_provider_installed();

    let cli = Cli::parse();

    let log_level = if cli.verbose > 0 {
        match cli.verbose {
            1 => "debug",
            _ => "trace",
        }
    } else {
        match &cli.command {
            Some(Commands::Status { .. })
            | Some(Commands::Servers)
            | Some(Commands::Tools { .. }) => "none",
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
        None => views::overview::cmd_overview(cli.config.as_ref(), &cli.output).await?,
        Some(Commands::Start) => runtime::cmd_start(cli.config.as_ref(), &cli.output).await?,
        Some(Commands::Connect) => runtime::cmd_connect(cli.config.as_ref()).await?,
        Some(Commands::Serve { daemon }) => {
            if daemon {
                runtime::cmd_daemon(cli.config.as_ref()).await?;
            } else {
                runtime::cmd_serve(cli.config.as_ref()).await?;
            }
        }
        Some(Commands::Status { show_token }) => {
            views::overview::cmd_status(cli.config.as_ref(), &cli.output, show_token).await?
        }
        Some(Commands::Stop) => runtime::cmd_daemon_stop().await?,
        Some(Commands::Servers) => {
            views::servers::cmd_server_list(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Clients) => {
            views::clients::cmd_client_list(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Tools { command }) => {
            commands::tools::cmd_tool_command(
                cli.config.as_ref(),
                command,
                &cli.output,
                cli.verbose,
            )
            .await?
        }
        Some(Commands::Link { targets, all, yes }) => {
            commands::clients::cmd_link(targets, all, yes)?
        }
        Some(Commands::Unlink { targets, all, yes }) => {
            commands::clients::cmd_unlink(targets, all, yes)?
        }
        Some(Commands::Server { command }) => {
            commands::servers::cmd_server_command(cli.config.as_ref(), command, &cli.output).await?
        }
        Some(Commands::Import {
            clients,
            all,
            dry_run,
            yes,
        }) => commands::misc::cmd_import(
            cli.config.as_ref(),
            clients,
            all,
            dry_run,
            yes,
            &cli.output,
        )?,
        Some(Commands::Doctor) => {
            commands::misc::cmd_doctor(cli.config.as_ref(), &cli.output).await?
        }
        Some(Commands::Repair) => commands::misc::cmd_repair()?,
        Some(Commands::Setup { yes }) => commands::misc::cmd_setup(cli.config.as_ref(), yes)?,
        Some(Commands::Reload) => commands::misc::cmd_reload().await?,
        Some(Commands::Config { path, command }) => {
            commands::config::cmd_config(cli.config.as_ref(), path, command, &cli.output)?
        }
        Some(Commands::Export { targets, all, yes }) => {
            commands::clients::cmd_link(targets, all, yes)?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_command_rejects_stdio_flag() {
        let result = Cli::try_parse_from(["plug", "serve", "--stdio"]);
        assert!(result.is_err());
    }

    #[test]
    fn serve_command_accepts_daemon_flag() {
        let cli = Cli::try_parse_from(["plug", "serve", "--daemon"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Serve { daemon: true })
        ));
    }
}
